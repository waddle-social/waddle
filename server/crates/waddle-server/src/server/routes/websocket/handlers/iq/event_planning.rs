//! Room-affiliation protected XEP-0060 storage for event workspaces.
//!
//! The calendar service is public. Planning data instead lives under the
//! event room's JID, and every operation resolves the current MUC affiliation.
//! Notifications and subscriptions are intentionally unavailable: a stale
//! subscription must never survive removal from the room.
//!
//! A room an enrolled host-grant scope manages is decided from its canonical
//! grants instead (`HostGrantGate`): reading the workspace needs `read`,
//! writing one's own objects `send_message`, and the organizer's rights (the
//! `event` object, editing and retracting other members' objects) `manage`.
//! The room actor's affiliation list is only a mirror there and decides
//! nothing; its snapshot still selects the room incarnation's backing node.

use jid::{BareJid, FullJid};
use tracing::warn;
use waddle_xmpp::pubsub::{NodeConfig, SendLastPublishedItem};
use waddle_xmpp::pubsub::{PubSubError, PubSubItem, PubSubRequest};
use waddle_xmpp::{Affiliation, StanzaErrorCondition, StanzaErrorType, XmppError};
use waddle_xmpp_core::event_planning::{PlanningItem, PlanningKind, NODE_EVENT_PLANNING};
use xmpp_parsers::iq::Iq;

use super::{
    build_pubsub_error, build_pubsub_items_result, build_pubsub_publish_result,
    build_pubsub_success, build_xmpp_error_response, get_room_actor_result, iq_to_xml,
    WebSocketState,
};
use crate::permissions::host_grant::{
    HostGrantAction, ManagedRoomGrants, PrincipalEvidence, RoomBinding, RoomGate,
};

fn error(iq: &Iq, condition: PubSubError) -> Vec<String> {
    vec![iq_to_xml(build_pubsub_error(iq, condition))]
}

fn is_member(affiliation: Affiliation) -> bool {
    matches!(
        affiliation,
        Affiliation::Owner | Affiliation::Admin | Affiliation::Member
    )
}

fn is_organizer(affiliation: Affiliation) -> bool {
    matches!(affiliation, Affiliation::Owner | Affiliation::Admin)
}

/// What a request may do in an event workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// Nothing: the workspace is not shown to it.
    Outsider,
    /// Read the workspace and write its own objects.
    Member,
    /// A member who also maintains the `event` object and may edit and
    /// retract other members' objects.
    Organizer,
}

impl Standing {
    /// The standing a room actor's affiliation gives, in a room no enrolled
    /// host-grant scope manages.
    fn of(affiliation: Affiliation) -> Self {
        if is_organizer(affiliation) {
            Self::Organizer
        } else if is_member(affiliation) {
            Self::Member
        } else {
            Self::Outsider
        }
    }

    fn is_organizer(self) -> bool {
        self == Self::Organizer
    }
}

/// The host-grant action a member needs for `request`: `read` to retrieve
/// the workspace, `send_message` to write to it. Organizer rights need
/// `manage` as well.
fn member_action(request: &PubSubRequest) -> HostGrantAction {
    match request {
        PubSubRequest::Items { .. } => HostGrantAction::Read,
        _ => HostGrantAction::SendMessage,
    }
}

/// The workspace request of a session, which no S2S stream carries: a
/// PubSub request to a room JID is refused on inbound S2S streams.
const SESSION: PrincipalEvidence = PrincipalEvidence::NoStream;

/// The standing `author` holds now in a managed room, from the decision
/// `decided` and, once its fence ticket is no longer current, from the
/// grants read again. A room that cannot be decided now gives none.
async fn managed_standing(
    state: &WebSocketState,
    decided: &ManagedRoomGrants,
    author: &BareJid,
    action: HostGrantAction,
) -> Standing {
    let gate = &state.deps.app_state.host_grants;
    if !gate.still_allows(decided, author, &SESSION, action).await {
        Standing::Outsider
    } else if gate
        .still_allows(decided, author, &SESSION, HostGrantAction::Manage)
        .await
    {
        Standing::Organizer
    } else {
        Standing::Member
    }
}

/// Whether `author` still holds `standing` in a managed room, asked just
/// before the effect or the response it was decided for, once awaits
/// separated them. A room no enrolled scope manages (`decided` is `None`)
/// keeps the room actor's snapshot it was decided with.
async fn still_holds(
    state: &WebSocketState,
    decided: Option<&ManagedRoomGrants>,
    author: &BareJid,
    action: HostGrantAction,
    standing: Standing,
) -> bool {
    let Some(decided) = decided else {
        return true;
    };
    match standing {
        Standing::Outsider => false,
        Standing::Member => {
            state
                .deps
                .app_state
                .host_grants
                .still_allows(decided, author, &SESSION, action)
                .await
        }
        Standing::Organizer => {
            managed_standing(state, decided, author, action).await == Standing::Organizer
        }
    }
}

fn can_publish_kind(item: &PlanningItem, author: &BareJid, standing: Standing) -> bool {
    if item.kind == PlanningKind::Event {
        return item.id == "event" && standing.is_organizer();
    }
    if item.id == "event" {
        return false;
    }
    if let Some(expected) = item.self_owned_id(author) {
        return item.id == expected;
    }
    // Reserved personal-object identifiers cannot be reused for another kind.
    !item.id.starts_with("attendance:") && !item.id.starts_with("vote:")
}

pub(super) async fn handle_room_pubsub(
    iq: &Iq,
    state: &WebSocketState,
    room: &BareJid,
    sender: &FullJid,
    request: PubSubRequest,
) -> Vec<String> {
    let author = sender.to_bare();
    let action = member_action(&request);
    // A room an enrolled host-grant scope manages is decided on this replica
    // from its canonical grants, before anything about the room is read, and
    // fails closed while they cannot be read. The room actor's affiliation
    // list is a mirror of those grants there and admits nothing.
    let managed = match state
        .deps
        .app_state
        .host_grants
        .room(RoomBinding::Room(room))
        .await
    {
        RoomGate::Unmanaged => None,
        RoomGate::Unavailable => return error(iq, PubSubError::InternalServerError),
        RoomGate::Managed(decided) => {
            if !decided.allows_as(&author, &SESSION, action) {
                return error(iq, PubSubError::Forbidden);
            }
            Some(decided)
        }
    };
    // An alive actor can briefly remain on the previous owner after a
    // distributed claim moves. Resolve ownership before consulting it.
    #[cfg(feature = "clustering")]
    let remote_access =
        match crate::server::routes::interpret::extension_room_owner(state, room).await {
            Ok(Some(_)) => match super::event_planning_access::remote_event_planning_access(
                state, room, sender,
            )
            .await
            {
                Ok(Some(access)) => Some(access),
                Ok(None) => return error(iq, PubSubError::NodeNotFound),
                Err(()) => return error(iq, PubSubError::InternalServerError),
            },
            Ok(None) => None,
            Err(_) => return error(iq, PubSubError::InternalServerError),
        };
    #[cfg(not(feature = "clustering"))]
    let remote_access: Option<super::event_planning_access::EventPlanningAccess> = None;
    let access = if let Some(access) = remote_access {
        access
    } else {
        match get_room_actor_result(state, room).await {
            Ok(Some(actor)) => {
                match super::event_planning_access::local_event_planning_access(&actor, sender)
                    .await
                {
                    Ok(access) => access,
                    Err(()) => return error(iq, PubSubError::InternalServerError),
                }
            }
            Ok(None) => {
                #[cfg(feature = "clustering")]
                {
                    match super::event_planning_access::remote_event_planning_access(
                        state, room, sender,
                    )
                    .await
                    {
                        Ok(Some(access)) => access,
                        Ok(None) => return error(iq, PubSubError::NodeNotFound),
                        Err(()) => return error(iq, PubSubError::InternalServerError),
                    }
                }
                #[cfg(not(feature = "clustering"))]
                {
                    return error(iq, PubSubError::NodeNotFound);
                }
            }
            Err(cause) => {
                warn!(%room, %cause, "Event planning room lookup failed");
                return error(iq, PubSubError::InternalServerError);
            }
        }
    };
    let storage_node = access.storage_node();
    // The snapshot selects the backing node in every room; it decides the
    // standing only in a room no enrolled scope manages.
    let standing = match managed.as_ref() {
        None => Standing::of(access.affiliation),
        Some(decided) => managed_standing(state, decided, &author, action).await,
    };
    if standing == Standing::Outsider {
        return error(iq, PubSubError::Forbidden);
    }
    let decided = managed.as_ref();
    let storage = &state.deps.protocol.pubsub_storage;
    match request {
        PubSubRequest::Items {
            node,
            max_items,
            item_ids,
        } if node == NODE_EVENT_PLANNING && matches!(iq, Iq::Get { .. }) => {
            if max_items == Some(0) || item_ids.len() > 2000 {
                return error(iq, PubSubError::BadRequest);
            }
            match storage
                .get_items(
                    room,
                    &storage_node,
                    Some(max_items.unwrap_or(2001).min(2001)),
                    &item_ids,
                )
                .await
            {
                Ok(stored) => {
                    if stored.len() > 2000 {
                        return vec![build_xmpp_error_response(
                            iq,
                            XmppError::Stanza {
                                condition: StanzaErrorCondition::ResourceConstraint,
                                error_type: StanzaErrorType::Wait,
                                text: Some(
                                    "Event workspace exceeds the 2000-object retrieval limit."
                                        .into(),
                                ),
                            },
                        )];
                    }
                    // The items are sent only if the grants still admit
                    // the reader once they were read.
                    if !still_holds(state, decided, &author, action, standing).await {
                        return error(iq, PubSubError::Forbidden);
                    }
                    let items: Vec<_> = stored.iter().map(|item| item.to_pubsub_item()).collect();
                    vec![iq_to_xml(build_pubsub_items_result(iq, &node, &items))]
                }
                Err(cause) => {
                    warn!(%room, %cause, "Event planning read failed");
                    error(iq, PubSubError::InternalServerError)
                }
            }
        }
        PubSubRequest::Publish {
            node,
            item,
            publish_options,
        } if node == NODE_EVENT_PLANNING && matches!(iq, Iq::Set { .. }) => {
            if publish_options.is_some() {
                return error(iq, PubSubError::BadRequest);
            }
            let Some(payload) = item.payload.as_ref() else {
                return error(iq, PubSubError::PayloadRequired);
            };
            let mut planning = match PlanningItem::from_element(payload) {
                Ok(planning) => planning,
                Err(_) => return error(iq, PubSubError::InvalidPayload),
            };
            if item.id.as_deref() != Some(planning.id.as_str()) {
                return error(iq, PubSubError::InvalidPayload);
            }
            if !can_publish_kind(&planning, &author, standing) {
                return error(iq, PubSubError::Forbidden);
            }
            if planning.kind == PlanningKind::Vote {
                let Some(subject) = planning.subject.as_ref() else {
                    return error(iq, PubSubError::InvalidPayload);
                };
                let polls = match storage
                    .get_items(room, &storage_node, None, std::slice::from_ref(subject))
                    .await
                {
                    Ok(polls) => polls,
                    Err(cause) => {
                        warn!(%room, %cause, "Event planning poll lookup failed");
                        return error(iq, PubSubError::InternalServerError);
                    }
                };
                let poll = polls
                    .first()
                    .and_then(|stored| stored.to_pubsub_item().payload)
                    .and_then(|payload| PlanningItem::from_element(&payload).ok());
                let Some(poll) = poll else {
                    return error(iq, PubSubError::ItemNotFound);
                };
                if poll.kind != PlanningKind::Poll
                    || poll.completed
                    || planning
                        .selections
                        .iter()
                        .any(|selection| !poll.options.iter().any(|option| option.id == *selection))
                {
                    return error(iq, PubSubError::InvalidPayload);
                }
            }
            // An organizer may edit shared objects, but that must not transfer
            // authorship or make an expense appear paid by the organizer.
            // The conditional write below also refuses an intervening change
            // of publisher between this read and the write.
            let mut effective_author = author.clone();
            match storage
                .get_items(
                    room,
                    &storage_node,
                    None,
                    std::slice::from_ref(&planning.id),
                )
                .await
            {
                Ok(existing) => {
                    if let Some(stored) = existing.first() {
                        let original = stored
                            .to_pubsub_item()
                            .payload
                            .and_then(|payload| PlanningItem::from_element(&payload).ok());
                        let organizer_edit = standing.is_organizer()
                            && !matches!(
                                planning.kind,
                                PlanningKind::Attendance
                                    | PlanningKind::Vote
                                    | PlanningKind::Travel
                            );
                        let assignee_edit = original.as_ref().is_some_and(|original| {
                            original.kind == PlanningKind::Task
                                && planning.kind == PlanningKind::Task
                                && original.assignee.as_ref() == Some(&author)
                                && planning.assignee == original.assignee
                        });
                        if organizer_edit || assignee_edit {
                            if let Some(publisher) = stored.publisher.as_ref() {
                                effective_author = publisher.clone();
                            }
                        }
                    }
                }
                Err(cause) => {
                    warn!(%room, %cause, "Event planning author lookup failed");
                    return error(iq, PubSubError::InternalServerError);
                }
            }
            // Neither payload authors nor PubSub publisher attributes are
            // trusted. Authorship comes from the session or existing storage.
            planning.author = Some(effective_author.clone());
            let item = PubSubItem {
                id: Some(planning.id.clone()),
                publisher: Some(effective_author.clone()),
                payload: Some(planning.to_element()),
            };
            // Room addressing is intercepted before generic PubSub dispatch.
            // Whitelist config is an additional barrier against accidental
            // generic reads or notifications introduced by future routes.
            if let Err(cause) = storage.get_or_create_node(room, &storage_node).await {
                warn!(%room, %cause, "Event planning node creation failed");
                return error(iq, PubSubError::InternalServerError);
            }
            let mut config = NodeConfig::spaces_private();
            config.node_type = None;
            config.send_last_published_item = SendLastPublishedItem::Never;
            config.deliver_payloads = false;
            config.notify_retract = false;
            config.notify_delete = false;
            if let Err(cause) = storage
                .update_node_config(room, &storage_node, &config)
                .await
            {
                warn!(%room, %cause, "Event planning node configuration failed");
                return error(iq, PubSubError::InternalServerError);
            }
            // The lookups and node preparation above were awaits: the
            // standing the write relies on is confirmed just before it.
            if !still_holds(state, decided, &author, action, standing).await {
                return error(iq, PubSubError::Forbidden);
            }
            let result = storage
                .publish_item_if_missing_or_publisher(
                    room,
                    &storage_node,
                    &item,
                    &effective_author,
                    false,
                )
                .await;
            match result {
                Ok(published) => vec![iq_to_xml(build_pubsub_publish_result(
                    iq,
                    &node,
                    &published.item_id,
                ))],
                Err(cause) => {
                    warn!(%room, %cause, "Event planning publication refused");
                    vec![build_xmpp_error_response(iq, cause)]
                }
            }
        }
        PubSubRequest::Retract { node, item_id, .. }
            if node == NODE_EVENT_PLANNING && matches!(iq, Iq::Set { .. }) =>
        {
            if item_id == "event" && !standing.is_organizer() {
                return error(iq, PubSubError::Forbidden);
            }
            if !still_holds(state, decided, &author, action, standing).await {
                return error(iq, PubSubError::Forbidden);
            }
            let result = if standing.is_organizer() {
                storage.retract_item(room, &storage_node, &item_id).await
            } else {
                storage
                    .retract_item_if_publisher(room, &storage_node, &item_id, &author)
                    .await
            };
            match result {
                Ok(true) => vec![iq_to_xml(build_pubsub_success(iq))],
                Ok(false) => error(iq, PubSubError::ItemNotFound),
                Err(cause) => {
                    warn!(%room, %cause, "Event planning retraction refused");
                    vec![build_xmpp_error_response(iq, cause)]
                }
            }
        }
        _ => error(iq, PubSubError::Forbidden),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_member, is_organizer};
    use waddle_xmpp::Affiliation;

    #[test]
    fn event_workspace_access_requires_explicit_membership() {
        assert!(!is_member(Affiliation::None));
        assert!(!is_member(Affiliation::Outcast));
        for affiliation in [Affiliation::Member, Affiliation::Admin, Affiliation::Owner] {
            assert!(is_member(affiliation));
        }
        assert!(!is_organizer(Affiliation::Member));
        assert!(is_organizer(Affiliation::Admin));
        assert!(is_organizer(Affiliation::Owner));
    }
}

#[cfg(test)]
#[path = "event_planning/tests.rs"]
mod persistence_tests;
