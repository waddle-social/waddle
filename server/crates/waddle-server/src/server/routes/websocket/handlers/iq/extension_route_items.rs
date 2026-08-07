use super::permissions::permission_allowed;
use super::*;
use crate::server::routes::websocket::ResolvedPrincipal;

fn extension_route_room_for_node(state: &WebSocketState, node: &str) -> Option<BareJid> {
    state
        .deps
        .protocol
        .extension_manager
        .route_descriptors()
        .iter()
        .find_map(|route| {
            extension_route_placeholder_value(route.state_node.as_str(), node, "room")
                .and_then(|room| room.parse::<BareJid>().ok())
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
    let Some(room_jid) = extension_route_room_for_node(state, node) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
    };
    if room_jid.domain().as_str() != muc_domain {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    }
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
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
