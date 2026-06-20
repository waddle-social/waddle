use super::*;
use crate::permissions::{DeleteTuple, PermissionError, Relation, SubjectType, Tuple, WriteTuple};

pub(super) async fn resolve_managed_channel_affiliation(
    state: &WebSocketState,
    session: &Session,
    room_jid: &BareJid,
    channel_id: &str,
    members_only: bool,
) -> Result<Option<Affiliation>, ()> {
    let object = Object::new(ObjectType::Channel, channel_id);
    let subject = Subject::user(&session.user_jid);
    if check_channel_permission(
        state,
        object.clone(),
        subject.clone(),
        Permission::Custom("outcast".into()),
    )
    .await?
    {
        return Ok(Some(Affiliation::Outcast));
    }

    for (permission, affiliation) in [
        (Permission::Owner, Affiliation::Owner),
        (Permission::Admin, Affiliation::Admin),
        (Permission::Member, Affiliation::Member),
    ] {
        if check_channel_permission(state, object.clone(), subject.clone(), permission).await? {
            return Ok(Some(affiliation));
        }
    }

    if !members_only && matches!(channel_id, "chat" | "announcements" | "github-actions") {
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
            return Ok(Some(Affiliation::Member));
        }
    }

    if check_channel_permission(state, object.clone(), subject.clone(), Permission::Read).await? {
        return Ok(read_access_affiliation(members_only));
    }

    restore_space_parent_tuples_for_room(state, room_jid, channel_id).await?;
    if check_channel_permission(state, object, subject, Permission::Read).await? {
        return Ok(read_access_affiliation(members_only));
    }
    Ok(None)
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
    session: Option<&Session>,
    permission: Permission,
) -> Result<bool, ()> {
    let Some(session) = session else {
        return Ok(false);
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&session.user_jid),
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
