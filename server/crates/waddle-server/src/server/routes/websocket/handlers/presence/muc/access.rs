use super::*;
use crate::permissions::{
    DeleteTuple, ListRelations, PermissionError, Relation, SubjectType, Tuple, WriteTuple,
};

pub(super) async fn resolve_managed_channel_affiliation(
    state: &WebSocketState,
    user_jid: &BareJid,
    room_jid: &BareJid,
    channel_id: &str,
    members_only: bool,
    allow_repairs: bool,
) -> Result<Option<Affiliation>, ()> {
    let object = Object::new(ObjectType::Channel, channel_id);
    let subject = Subject::user(user_jid.to_string());
    if let Some(affiliation) =
        direct_channel_affiliation(state, object.clone(), subject.clone()).await?
    {
        return Ok(Some(affiliation));
    }

    if !members_only {
        let server = Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID);
        if check_channel_permission(state, server.clone(), subject.clone(), Permission::Owner)
            .await?
        {
            return Ok(Some(Affiliation::Owner));
        }
        if check_channel_permission(state, server.clone(), subject.clone(), Permission::Admin)
            .await?
        {
            return Ok(Some(Affiliation::Admin));
        }
        if check_channel_permission(state, server, subject.clone(), Permission::Member).await? {
            return Ok(Some(Affiliation::None));
        }
    }

    if check_channel_permission(state, object.clone(), subject.clone(), Permission::Read).await? {
        return Ok(read_access_affiliation(members_only));
    }

    // The parent-tuple repair below issues `WriteTuple`/`DeleteTuple`
    // effects. Join admission opts in (`allow_repairs = true`) so a stale
    // Space→channel projection self-heals on the way into the room. The
    // read-only MAM archive gate opts out: an archive query must not
    // mutate the permission graph. A member whose Space-inherited read
    // access hinges on a broken parent tuple is denied here until their
    // next join repairs it; explicit channel affiliations (resolved by
    // `direct_channel_affiliation` above) are unaffected.
    if allow_repairs {
        restore_space_parent_tuples_for_room(state, room_jid, channel_id).await?;
        if check_channel_permission(state, object, subject, Permission::Read).await? {
            return Ok(read_access_affiliation(members_only));
        }
    }
    Ok(None)
}

/// XEP-0313 §5.1 archive-access decision for a MUC room MAM query.
pub enum RoomArchiveAccess {
    Allowed,
    Denied,
    Error,
}

/// XEP-0313 §5.1: "A MUC archive MUST check that the user requesting the
/// archive has the right to enter it at the time of the query and only
/// allow access if so." The decision mirrors join admission: members-only
/// rooms require at least Member affiliation, open rooms admit any
/// non-outcast. An unmanaged room without a live actor has no admission
/// data, so it fails closed.
///
/// `channel` is the managed-channel row for `room_jid` (or `None` for an
/// unmanaged room), resolved once by the caller and shared with
/// `group_dm_archive_visibility` so a single MAM query does not fetch the
/// same row twice.
pub async fn resolve_muc_room_archive_access(
    state: &WebSocketState,
    room_jid: &BareJid,
    requester: Option<&BareJid>,
    channel: Option<&XmppChannelRecord>,
) -> RoomArchiveAccess {
    let Some(requester) = requester else {
        return RoomArchiveAccess::Denied;
    };

    let snapshot = match get_room_actor(state, room_jid).await {
        Some(actor) => match actor.ask(GetSnapshot).await {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                warn!(
                    room = %room_jid,
                    requester = %requester,
                    error = ?error,
                    "Failed to snapshot room for MAM access gate"
                );
                return RoomArchiveAccess::Error;
            }
        },
        None => None,
    };

    if let Some(channel) = channel {
        // Same precedence as join admission: a live actor's config wins
        // over a stale channel row.
        let members_only = snapshot
            .as_ref()
            .map(|snapshot| snapshot.room.config.members_only)
            .unwrap_or(channel.members_only);
        return match resolve_managed_channel_affiliation(
            state,
            requester,
            room_jid,
            &channel.id,
            members_only,
            // Read-only archive gate: never mutate the permission graph.
            false,
        )
        .await
        {
            Ok(Some(Affiliation::Outcast)) => RoomArchiveAccess::Denied,
            Ok(Some(_)) => RoomArchiveAccess::Allowed,
            Ok(None) if members_only => RoomArchiveAccess::Denied,
            Ok(None) => RoomArchiveAccess::Allowed,
            Err(()) => RoomArchiveAccess::Error,
        };
    }

    // Unmanaged room (instant room, no channel row). Admission data lives
    // only in the live actor's in-memory affiliation list and config; both
    // are lost when the room is evicted. If no actor is live we cannot
    // authorize the request, so we deliberately fail closed rather than
    // fail open: an instant room reconfigured members-only before eviction
    // would otherwise leak its archive to non-members (the #1093 bypass).
    // Managed channels — the norm in Waddle — always resolve above via the
    // persisted channel row and are unaffected.
    match snapshot {
        Some(snapshot) if snapshot.room.can_user_join(requester) => RoomArchiveAccess::Allowed,
        Some(_) => {
            debug!(
                room = %room_jid,
                requester = %requester,
                "MAM archive denied: requester may not enter unmanaged room"
            );
            RoomArchiveAccess::Denied
        }
        None => {
            debug!(
                room = %room_jid,
                requester = %requester,
                "MAM archive denied: no live actor for unmanaged room (fail-closed)"
            );
            RoomArchiveAccess::Denied
        }
    }
}

async fn direct_channel_affiliation(
    state: &WebSocketState,
    object: Object,
    subject: Subject,
) -> Result<Option<Affiliation>, ()> {
    let relations = state
        .deps
        .app_state
        .permission_actor
        .ask(ListRelations { subject, object })
        .await
        .map_err(|error| {
            warn!(error = ?error, "Failed to load direct managed MUC affiliation relations");
        })?;

    let affiliation = ["outcast", "owner", "admin", "member"]
        .into_iter()
        .find(|relation| relations.iter().any(|actual| actual.name == *relation))
        .and_then(|relation| match relation {
            "outcast" => Some(Affiliation::Outcast),
            "owner" => Some(Affiliation::Owner),
            "admin" => Some(Affiliation::Admin),
            "member" => Some(Affiliation::Member),
            _ => None,
        });

    Ok(affiliation)
}

fn read_access_affiliation(members_only: bool) -> Option<Affiliation> {
    if members_only {
        None
    } else {
        Some(Affiliation::None)
    }
}

async fn restore_space_parent_tuples_for_room(
    state: &WebSocketState,
    room_jid: &BareJid,
    channel_id: &str,
) -> Result<(), ()> {
    let item_id = room_jid.to_string();
    let nodes = state
        .deps
        .protocol
        .pubsub_storage
        .list_node_names_for_item(&state.deps.app_state.spaces_jid, &item_id)
        .await
        .map_err(|error| {
            warn!(
                room = %room_jid,
                channel_id,
                error = %error,
                "Failed to enumerate Spaces bookmarks while repairing managed MUC permissions"
            );
        })?;

    let mut valid_nodes = Vec::new();
    for node in nodes {
        if space_node_has_room_bookmark(state, room_jid, channel_id, &node, &item_id).await? {
            valid_nodes.push(node);
        }
    }

    let repair_nodes = space_parent_repair_nodes(state, room_jid, channel_id, valid_nodes).await?;
    for node in repair_nodes {
        let tuple = Tuple::new(
            Object::new(ObjectType::Channel, channel_id),
            Relation::new("parent"),
            Subject::userset(SubjectType::Space, &node, ""),
        );
        match state
            .deps
            .app_state
            .permission_actor
            .ask(WriteTuple {
                tuple: tuple.clone(),
            })
            .await
        {
            Ok(())
            | Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => {}
            Err(error) => {
                warn!(
                    room = %room_jid,
                    channel_id,
                    node,
                    error = ?error,
                    "Failed to repair managed MUC channel parent tuple"
                );
                return Err(());
            }
        }
        if !space_node_has_room_bookmark(state, room_jid, channel_id, &node, &item_id).await? {
            match state
                .deps
                .app_state
                .permission_actor
                .ask(DeleteTuple {
                    tuple: tuple.clone(),
                })
                .await
            {
                Ok(())
                | Err(kameo::error::SendError::HandlerError(PermissionError::TupleNotFound)) => {}
                Err(error) => {
                    warn!(
                        room = %room_jid,
                        channel_id,
                        node,
                        error = ?error,
                        "Failed to clean up stale managed MUC channel parent tuple after revalidation"
                    );
                    return Err(());
                }
            }
        }
    }

    Ok(())
}

async fn space_node_has_room_bookmark(
    state: &WebSocketState,
    room_jid: &BareJid,
    channel_id: &str,
    node: &str,
    item_id: &str,
) -> Result<bool, ()> {
    let item_filter = [item_id.to_string()];
    let items = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(
            &state.deps.app_state.spaces_jid,
            node,
            Some(1),
            &item_filter,
        )
        .await
        .map_err(|error| {
            warn!(
                room = %room_jid,
                channel_id,
                node,
                error = %error,
                "Failed to load Spaces bookmark while repairing managed MUC permissions"
            );
        })?;
    let Some(item) = items.first() else {
        return Ok(false);
    };
    let Some(payload) = item
        .payload_xml
        .as_ref()
        .and_then(|xml| xml.parse::<Element>().ok())
    else {
        return Ok(false);
    };
    let Ok(bookmark) = waddle_xmpp::xep::xep0402::parse_bookmark(&item.id, &payload) else {
        return Ok(false);
    };
    Ok(bookmark.jid == *room_jid)
}

async fn space_parent_repair_nodes(
    state: &WebSocketState,
    room_jid: &BareJid,
    channel_id: &str,
    valid_nodes: Vec<String>,
) -> Result<Vec<String>, ()> {
    let link = state
        .deps
        .app_state
        .channel_space_link_store
        .get(room_jid)
        .await
        .map_err(|error| {
            warn!(
                room = %room_jid,
                channel_id,
                error = %error,
                "Failed to read channel-space projection while repairing managed MUC permissions"
            );
        })?;
    let Some(link) = link else {
        return match valid_nodes.as_slice() {
            [] => Ok(Vec::new()),
            [node] => Ok(vec![node.clone()]),
            nodes => {
                warn!(
                    room = %room_jid,
                    channel_id,
                    nodes = ?nodes,
                    "Refusing to repair ambiguous managed MUC Space parents without a channel-space projection"
                );
                Ok(Vec::new())
            }
        };
    };

    if link.space_jid.domain() != state.deps.app_state.spaces_jid.domain() {
        warn!(
            room = %room_jid,
            channel_id,
            linked_space = %link.space_jid,
            spaces = %state.deps.app_state.spaces_jid,
            "Refusing to repair managed MUC Space parent from a different Spaces service"
        );
        return Ok(Vec::new());
    }
    let Some(node) = link.space_jid.node().map(|node| node.to_string()) else {
        warn!(
            room = %room_jid,
            channel_id,
            linked_space = %link.space_jid,
            "Refusing to repair managed MUC Space parent without a Space node"
        );
        return Ok(Vec::new());
    };
    if valid_nodes.iter().any(|valid| valid == &node) {
        return Ok(vec![node]);
    }

    warn!(
        room = %room_jid,
        channel_id,
        linked_space = %link.space_jid,
        nodes = ?valid_nodes,
        "Refusing to repair managed MUC Space parent because the projected Space lacks the bookmark"
    );
    Ok(Vec::new())
}

async fn check_channel_permission(
    state: &WebSocketState,
    object: Object,
    subject: Subject,
    permission: Permission,
) -> Result<bool, ()> {
    let response = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject,
            permission,
            object,
        })
        .await
        .map_err(|error| {
            warn!(error = ?error, "Permission actor failed during managed MUC join");
        })?;
    Ok(response.allowed)
}

pub(super) async fn server_permission_allowed(
    state: &WebSocketState,
    principal: Option<crate::server::routes::websocket::ResolvedPrincipal<'_>>,
    permission: Permission,
) -> Result<bool, ()> {
    let Some(principal) = principal else {
        return Ok(false);
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&principal.user_jid),
            permission,
            object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
        })
        .await
        .map(|response| response.allowed)
        .map_err(|error| {
            warn!(error = %error, "Failed to authorize MUC creation");
        })
}

/// Derive the single-space id and channel id from a room's bare JID node.
///
/// Convention: node is the channel id.
/// Falls back to ("default", "default") if the node can't be parsed.
pub fn parse_room_jid_context(room_jid: &jid::BareJid) -> (String, String) {
    if let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) {
        return ("space".to_string(), channel_id);
    }
    ("default".to_string(), "default".to_string())
}

pub async fn get_managed_channel_for_room(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<Option<XmppChannelRecord>, String> {
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
        return Ok(None);
    };
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    get_xmpp_channel(actor, &channel_id).await
}
