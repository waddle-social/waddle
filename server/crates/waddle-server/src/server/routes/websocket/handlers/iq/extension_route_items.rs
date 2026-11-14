use super::permissions::permission_allowed;
use super::{
    build_pubsub_error, build_pubsub_items_result, iq_to_xml, managed_channel_permission_allowed,
    warn, BareJid, CheckPermission, Object, ObjectType, Permission, PubSubError, Subject,
    WebSocketState,
};
use crate::server::routes::websocket::ResolvedPrincipal;
use waddle_xmpp::identity::CanonicalBareJid;

/// The room an extension route node names, from the first route whose
/// pattern the node matches. `Some(Err(()))` when that room segment is not a
/// canonical bare JID: the checks below run on the parsed room while the
/// affiliation is written to the node as named, so a spelling the `jid`
/// parser would fold (`Room@muc.example.com`) must not stand for the room.
fn extension_route_room_for_node(
    state: &WebSocketState,
    node: &str,
) -> Option<Result<BareJid, ()>> {
    state
        .deps
        .protocol
        .extension_manager
        .route_descriptors()
        .iter()
        .find_map(|route| {
            extension_route_placeholder_value(route.state_node.as_str(), node, "room")
        })
        .map(|room| {
            CanonicalBareJid::parse_exact(&room)
                .map(CanonicalBareJid::into_bare_jid)
                .map_err(|_| ())
        })
}

fn extension_route_placeholder_value(
    pattern: &str,
    candidate: &str,
    placeholder: &str,
) -> Option<String> {
    let pattern_parts: Vec<_> = pattern.split(':').collect();
    let candidate_parts: Vec<_> = candidate.split(':').collect();
    if pattern_parts.len() != candidate_parts.len() {
        return None;
    }
    let placeholder = format!("{{{placeholder}}}");
    let mut value = None;
    for (pattern_part, candidate_part) in pattern_parts.iter().zip(candidate_parts) {
        if *pattern_part == placeholder {
            if candidate_part.is_empty() {
                return None;
            }
            value = Some(candidate_part.to_string());
            continue;
        }
        if *pattern_part != candidate_part {
            return None;
        }
    }
    value
}

/// What a room-bound extension route node allows an account, decided from
/// the room's current right and never from the node's PubSub affiliations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExtensionRouteAccess {
    /// The node is not bound to a room; PubSub's own rules apply.
    NotRoomBound,
    Allowed,
    Denied,
}

/// The room right behind a room-bound extension route node of the
/// extensions service `owner`.
///
/// Reading a room's extension state hands the reader a PubSub `member`
/// affiliation on the node, and that row outlives a later ban. Every use of
/// the node that an affiliation would otherwise decide (subscribe, event
/// fan-out, publish) is therefore decided by the room as it stands: the
/// account must not be a channel outcast and must hold `permission` on the
/// channel. A room an enrolled host-grant scope manages serves no extension
/// state, so every account is denied there, as it is when the room cannot
/// be read.
pub(crate) async fn extension_route_room_access(
    state: &WebSocketState,
    owner: &BareJid,
    node: &str,
    account: &BareJid,
    permission: Permission,
) -> ExtensionRouteAccess {
    if owner.as_str() != state.deps.service_domains.extensions {
        return ExtensionRouteAccess::NotRoomBound;
    }
    let room_jid = match extension_route_room_for_node(state, node) {
        None => return ExtensionRouteAccess::NotRoomBound,
        Some(Err(())) => return ExtensionRouteAccess::Denied,
        Some(Ok(room_jid)) => room_jid,
    };
    if room_jid.domain().as_str() != state.deps.service_domains.muc {
        return ExtensionRouteAccess::Denied;
    }
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) else {
        return ExtensionRouteAccess::Denied;
    };
    match host_grant_room_serves_extensions(state, &room_jid).await {
        Ok(true) => {}
        Ok(false) | Err(()) => return ExtensionRouteAccess::Denied,
    }
    let subject = Subject::user(account.to_string());
    let outcast = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: subject.clone(),
            permission: Permission::Custom("outcast".into()),
            object: Object::new(ObjectType::Channel, channel_id.clone()),
        })
        .await;
    match outcast {
        Ok(response) if !response.allowed => {}
        Ok(_) => return ExtensionRouteAccess::Denied,
        Err(error) => {
            warn!(node, error = %error, "Failed to check extension route outcast state");
            return ExtensionRouteAccess::Denied;
        }
    }
    match crate::server::extension_commands::pubsub::managed_channel_permission_allowed(
        &state.deps.app_state,
        &subject,
        &channel_id,
        permission,
    )
    .await
    {
        Ok(true) => ExtensionRouteAccess::Allowed,
        Ok(false) => ExtensionRouteAccess::Denied,
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize extension route access");
            ExtensionRouteAccess::Denied
        }
    }
}

/// Whether a room serves extension route state: a room that an enrolled
/// host-grant scope manages does not (`Ok(false)`), and `Err` when that
/// cannot be read.
async fn host_grant_room_serves_extensions(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<bool, ()> {
    use crate::permissions::host_grant::{RoomBinding, RoomGate};
    match state
        .deps
        .app_state
        .host_grants
        .room(RoomBinding::Room(room_jid))
        .await
    {
        RoomGate::Unmanaged => Ok(true),
        RoomGate::Managed(_) => Ok(false),
        RoomGate::Unavailable => Err(()),
    }
}

pub(super) struct PubSubItemsRead<'a> {
    pub(super) target_jid: &'a BareJid,
    pub(super) requester_jid: &'a BareJid,
    pub(super) node: &'a str,
    pub(super) max_items: Option<u32>,
    pub(super) item_ids: &'a [String],
}

pub(super) async fn handle_extension_route_items(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    muc_domain: &str,
    principal: Option<ResolvedPrincipal<'_>>,
    request: PubSubItemsRead<'_>,
) -> Vec<String> {
    let node = request.node;
    let room_jid = match extension_route_room_for_node(state, node) {
        Some(Ok(room_jid)) => room_jid,
        Some(Err(())) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))],
        None => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))],
    };
    if room_jid.domain().as_str() != muc_domain {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    }
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    // A room an enrolled host-grant scope manages serves no extension
    // state: the read below would hand the reader a PubSub affiliation the
    // canonical grants know nothing about.
    match host_grant_room_serves_extensions(state, &room_jid).await {
        Ok(true) => {}
        Ok(false) | Err(()) => {
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))]
        }
    }
    match permission_allowed(
        state,
        principal,
        Object::new(ObjectType::Channel, channel_id.clone()),
        Permission::Custom("outcast".into()),
    )
    .await
    {
        Ok(true) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
        Ok(false) => {}
        Err(error) => {
            warn!(node, error = %error, "Failed to check extension route outcast state");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
        }
    }
    match managed_channel_permission_allowed(state, principal, &channel_id, Permission::View).await
    {
        Ok(true) => {}
        Ok(false) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize extension route read");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
        }
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(request.target_jid, request.node)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return vec![iq_to_xml(build_pubsub_items_result(iq, node, &[]))],
        Err(error) => {
            warn!(node, error = %error, "Failed to retrieve extension route PubSub node");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
        }
    }
    if let Err(error) = state
        .deps
        .protocol
        .pubsub_storage
        .set_affiliation(
            request.target_jid,
            request.node,
            request.requester_jid,
            waddle_xmpp::pubsub::Affiliation::Member,
        )
        .await
    {
        warn!(node, error = %error, "Failed to sync extension route PubSub affiliation");
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
    }
    match crate::pubsub_authz::can_subscribe(
        &state.deps.protocol.pubsub_storage,
        request.target_jid,
        request.node,
        request.requester_jid,
        false,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize extension route PubSub access");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
        }
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_items(
            request.target_jid,
            request.node,
            request.max_items,
            request.item_ids,
        )
        .await
    {
        Ok(stored_items) => {
            let items: Vec<_> = stored_items
                .iter()
                .map(|item| item.to_pubsub_item())
                .collect();
            vec![iq_to_xml(build_pubsub_items_result(iq, node, &items))]
        }
        Err(error) => {
            warn!(node, error = %error, "Failed to retrieve extension route PubSub items");
            vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))]
        }
    }
}
