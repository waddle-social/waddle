use super::permissions::{permission_allowed, write_channel_parent_tuple};
use super::*;
use crate::server::routes::websocket::handlers::pubsub_fanout;

/// PEP self-or-to check (XEP-0163 §3).
///
/// Returns `true` when the IQ is directed at `target_jid` (a PEP service) *or*
/// when no `to=` attribute is present and `user_jid` is the implicit PEP owner.
/// Use this in every pubsub IQ arm so that to-less self-targeted IQs receive
/// the same owner-derived affiliation as explicitly addressed PEP requests.
pub(super) fn is_pep_self_or_to(
    iq: &xmpp_parsers::iq::Iq,
    target_jid: &BareJid,
    user_jid: &BareJid,
) -> bool {
    is_pep_request_to(iq, target_jid) || is_pep_request(iq, user_jid)
}

pub(super) fn spaces_service_bare_jid(spaces_domain: &str) -> Result<BareJid, String> {
    spaces_domain
        .parse::<BareJid>()
        .map_err(|error| format!("invalid spaces service JID: {error}"))
}

pub(super) fn space_details_from_node(node: &waddle_xmpp::pubsub::PubSubNode) -> SpaceDetails {
    let name = if node.node_name == "general" {
        "General".to_string()
    } else {
        node.node_name.clone()
    };
    SpaceDetails {
        id: node.node_name.clone(),
        name,
        description: None,
        owner_id: node.owner.to_string(),
        icon_url: None,
        is_public: true,
        created_at: node.created_at.to_rfc3339(),
    }
}

fn channels_to_disco_items(channels: Vec<XmppChannelRecord>, muc_domain: &str) -> Vec<DiscoItem> {
    channels
        .into_iter()
        .filter_map(|channel| {
            waddle_xmpp::managed_room_jid(&channel.id, muc_domain)
                .ok()
                .map(|room_jid| DiscoItem::muc_room(&room_jid.to_string(), &channel.name))
        })
        .collect()
}

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

pub(super) async fn canonical_channel_disco_items(
    state: &WebSocketState,
    muc_domain: &str,
    limit: usize,
) -> Vec<DiscoItem> {
    match list_xmpp_channels(
        state.deps.app_state.db_pool.global_actor().clone(),
        limit,
        0,
    )
    .await
    {
        Ok(channels) => channels_to_disco_items(channels, muc_domain),
        Err(error) => {
            warn!(error = %error, "Failed to list canonical channels for MUC discovery");
            Vec::new()
        }
    }
}

pub(super) async fn handle_spaces_items(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    spaces_domain: &str,
    node: &str,
    max_items: Option<u32>,
    item_ids: &[String],
) -> Vec<String> {
    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&spaces_jid, node)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))],
        Err(error) => {
            warn!(node, error = %error, "Failed to retrieve Spaces node");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
        }
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces_jid, node, max_items, item_ids)
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
            warn!(node, error = %error, "Failed to retrieve Spaces items");
            vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))]
        }
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
    session: Option<&Session>,
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
        session,
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
    match managed_channel_permission_allowed(state, session, &channel_id, Permission::View).await {
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

pub(super) async fn handle_spaces_publish(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    muc_domain: &str,
    spaces_domain: &str,
    node: &str,
    item: PubSubItem,
    session: Option<&Session>,
) -> Vec<String> {
    match spaces_node_mutation_allowed(state, session, node).await {
        Ok(true) => {}
        Ok(false) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize Spaces publish");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
        }
    }
    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&spaces_jid, node)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))],
        Err(error) => {
            warn!(node, error = %error, "Failed to resolve Spaces node for publish");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
        }
    }

    let Some(item_id) = item.id.as_deref() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    let Some(payload) = item.payload.as_ref() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    let bookmark = match waddle_xmpp::xep::xep0402::parse_bookmark(item_id, payload) {
        Ok(bookmark) => bookmark,
        Err(error) => {
            warn!(item_id, error = %error, "Invalid XEP-0402 Spaces item");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
        }
    };
    if bookmark.jid.domain().as_str() != muc_domain {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    }
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&bookmark.jid) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    let db_actor = state.deps.app_state.db_pool.global_actor().clone();
    match get_xmpp_channel(db_actor, &channel_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::ItemNotFound))],
        Err(error) => {
            warn!(channel_id = %channel_id, error = %error, "Failed to look up channel for Spaces bookmark");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    }

    // XEP-0503 single-space-membership: a channel has exactly one
    // parent space. If the same bookmark item lives in any other
    // space node, retract it there first — otherwise we'd ship the
    // item in two `<items>` listings and `find_node_for_item` would
    // alphabetically pin the room under whichever space sorted first
    // (the original "rooms always show up under General" bug).
    //
    // Retract failures here are non-fatal: they're logged and the
    // publish proceeds, since `find_node_for_item` now also tiebreaks
    // by most-recent `seq` and will route lookups to the new node.
    // The retract is still attempted so legacy clients listing each
    // space don't see the room twice.
    match state
        .deps
        .protocol
        .pubsub_storage
        .list_node_names_for_item(&spaces_jid, item_id)
        .await
    {
        Ok(stale_nodes) => {
            for stale in stale_nodes.iter().filter(|name| name.as_str() != node) {
                if let Err(error) = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .retract_item(&spaces_jid, stale, item_id)
                    .await
                {
                    warn!(
                        channel_id = %channel_id,
                        from_node = %stale,
                        to_node = node,
                        item_id,
                        error = %error,
                        "Failed to retract room bookmark from prior Space node before re-publish; \
                         room may briefly appear in multiple Spaces"
                    );
                }
            }
        }
        Err(error) => {
            warn!(
                channel_id = %channel_id,
                node,
                item_id,
                error = %error,
                "Failed to enumerate prior Space nodes for room bookmark; \
                 single-membership invariant may be violated"
            );
        }
    }

    match state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces_jid, node, &item, None, false)
        .await
    {
        Ok(result) => {
            if let Err(error) = write_channel_parent_tuple(state, &channel_id, node).await {
                warn!(
                    channel_id = %channel_id,
                    node,
                    error = %error,
                    "Published Spaces item but failed to sync channel parent tuple; \
                     retracting to keep PubSub and permission graph consistent"
                );
                // Compensating retract: remove the just-published bookmark so
                // the server does not end up in a state where the item is
                // advertised in PubSub but the channel is not accessible via
                // Space membership (XEP-0503 §4).
                if let Err(retract_err) = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .retract_item(&spaces_jid, node, &result.item_id)
                    .await
                {
                    warn!(
                        channel_id = %channel_id,
                        node,
                        item_id = %result.item_id,
                        error = %retract_err,
                        "Compensating retract also failed; manual cleanup may be required"
                    );
                }
                return vec![iq_to_xml(build_pubsub_error(
                    iq,
                    PubSubError::InternalServerError,
                ))];
            }
            // Fan-out only after the parent-tuple write succeeds: the
            // compensating-retract path above must NOT emit events for
            // a publish that gets rolled back.
            // Spaces publishes are owned by the spaces service domain,
            // not a user JID. `is_pep = false` skips the §3 roster +
            // owner-self passes (PR #439 review): the publisher's
            // roster has no authorization relationship to a Spaces
            // node, so running those passes would leak the event.
            pubsub_fanout::fan_out_publish(
                state,
                pubsub_fanout::FanOutRequest {
                    owner: &spaces_jid,
                    node,
                    published_item: &item,
                    item_id: &result.item_id,
                    publisher: None,
                    publisher_full: None,
                    is_pep: false,
                },
            )
            .await;
            vec![iq_to_xml(build_pubsub_publish_result(
                iq,
                node,
                &result.item_id,
            ))]
        }
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to publish Spaces item");
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))]
        }
    }
}

/// Publish to a community pubsub node — XEP-0472 social feed at
/// `urn:xmpp:pubsub-social-feed:0` or XEP-0501 stories at
/// `urn:xmpp:stories:0`. Both live on `community.<domain>` (distinct
/// from the spaces service so the spaces enumeration only returns
/// real spaces). Same publish gate as spaces (server owners or
/// space owners) and the standard pubsub fan-out so subscribers see
/// new posts in real time.
pub(super) async fn handle_community_publish(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    item: PubSubItem,
    session: Option<&Session>,
) -> Vec<String> {
    if node != waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED
        && node != waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES
        && node != waddle_xmpp_core::xep0471::PUBSUB_NODE_EVENTS
    {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
    }
    handle_community_non_bookmark_publish(iq, state, community_domain, node, item, session).await
}

pub(super) async fn handle_community_items(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    max_items: Option<u32>,
    item_ids: &[String],
) -> Vec<String> {
    if node != waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED
        && node != waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES
        && node != waddle_xmpp_core::xep0471::PUBSUB_NODE_EVENTS
    {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
    }
    let Ok(community_jid) = community_domain.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&community_jid, node)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))],
        Err(error) => {
            warn!(node, error = %error, "Failed to retrieve community node");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
        }
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&community_jid, node, max_items, item_ids)
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
            warn!(node, error = %error, "Failed to retrieve community items");
            vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))]
        }
    }
}

pub(super) async fn handle_community_retract(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    item_id: &str,
    session: Option<&Session>,
) -> Vec<String> {
    if node != waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED
        && node != waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES
        && node != waddle_xmpp_core::xep0471::PUBSUB_NODE_EVENTS
    {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
    }
    match spaces_node_mutation_allowed(state, session, node).await {
        Ok(true) => {}
        Ok(false) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize community retract");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
        }
    }
    let Ok(community_jid) = community_domain.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .retract_item(&community_jid, node, item_id)
        .await
    {
        Ok(true) => vec![iq_to_xml(build_pubsub_success(iq))],
        Ok(false) => vec![iq_to_xml(build_pubsub_error(iq, PubSubError::ItemNotFound))],
        Err(error) => {
            warn!(node, item_id, error = %error, "Failed to retract community item");
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))]
        }
    }
}

/// Publish a non-bookmark item to a spaces-or-community pubsub node.
/// Used by `handle_community_publish` (feed + stories on
/// `community.<domain>`). Same auth gate as space-node mutations
/// (server owners or space owners) and the standard pubsub fan-out
/// so subscribers see new posts in real time.
async fn handle_community_non_bookmark_publish(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    item: PubSubItem,
    session: Option<&Session>,
) -> Vec<String> {
    match spaces_node_mutation_allowed(state, session, node).await {
        Ok(true) => {}
        Ok(false) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize community publish");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
        }
    }
    let Ok(community_jid) = community_domain.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&community_jid, node)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))],
        Err(error) => {
            warn!(node, error = %error, "Failed to resolve community node for publish");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
        }
    }

    match state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&community_jid, node, &item, None, false)
        .await
    {
        Ok(result) => {
            pubsub_fanout::fan_out_publish(
                state,
                pubsub_fanout::FanOutRequest {
                    owner: &community_jid,
                    node,
                    published_item: &item,
                    item_id: &result.item_id,
                    publisher: None,
                    publisher_full: None,
                    is_pep: false,
                },
            )
            .await;
            vec![iq_to_xml(build_pubsub_publish_result(
                iq,
                node,
                &result.item_id,
            ))]
        }
        Err(error) => {
            warn!(node, error = %error, "Failed to publish community item");
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))]
        }
    }
}

pub(super) async fn handle_spaces_retract(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    muc_domain: &str,
    spaces_domain: &str,
    node: &str,
    item_id: &str,
    session: Option<&Session>,
) -> Vec<String> {
    match spaces_node_mutation_allowed(state, session, node).await {
        Ok(true) => {}
        Ok(false) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize Spaces retract");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
        }
    }

    let Ok(room_jid) = item_id.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    if room_jid.domain().as_str() != muc_domain {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    }
    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .retract_item(&spaces_jid, node, item_id)
        .await
    {
        Ok(true) => vec![iq_to_xml(build_pubsub_success(iq))],
        Ok(false) => vec![iq_to_xml(build_pubsub_error(iq, PubSubError::ItemNotFound))],
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to retract Spaces item");
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))]
        }
    }
}

pub(super) async fn room_space_metadata_extensions(
    state: &WebSocketState,
    room_jid: &BareJid,
    description: Option<&str>,
) -> Vec<Element> {
    let spaces_domain = state.deps.service_domains.spaces.clone();
    let Ok(spaces_jid) = spaces_service_bare_jid(&spaces_domain) else {
        return vec![];
    };
    let room_item_id = room_jid.to_string();
    match state
        .deps
        .protocol
        .pubsub_storage
        .find_node_for_item(&spaces_jid, &room_item_id)
        .await
    {
        Ok(Some(space_node)) => build_room_space_metadata_forms_with_description(
            &spaces_domain,
            &space_node.node_name,
            description,
        ),
        Ok(None) => vec![],
        Err(error) => {
            warn!(room = %room_jid, error = %error, "Failed to find Space node for room");
            vec![]
        }
    }
}
