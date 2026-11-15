//! Room-affiliation protected XEP-0060 storage for event workspaces.
//!
//! The calendar service is public. Planning data instead lives under the
//! event room's JID, and every operation resolves the current MUC affiliation.
//! Notifications and subscriptions are intentionally unavailable: a stale
//! subscription must never survive removal from the room.

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

fn can_publish_kind(item: &PlanningItem, author: &BareJid, affiliation: Affiliation) -> bool {
    if item.kind == PlanningKind::Event {
        return item.id == "event" && is_organizer(affiliation);
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
    let affiliation = access.affiliation;
    let storage_node = access.storage_node();
    if !is_member(affiliation) {
        return error(iq, PubSubError::Forbidden);
    }
    let author = sender.to_bare();
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
            if !can_publish_kind(&planning, &author, affiliation) {
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
                        let organizer_edit = is_organizer(affiliation)
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
            if item_id == "event" && !is_organizer(affiliation) {
                return error(iq, PubSubError::Forbidden);
            }
            let result = if is_organizer(affiliation) {
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
