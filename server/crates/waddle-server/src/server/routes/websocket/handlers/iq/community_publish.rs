use super::community_rsvp::{handle_community_rsvp_publish, is_well_formed_rsvp_item};
use super::session_jid::session_bare_jid;
use super::{
    build_pubsub_error, build_pubsub_publish_result, iq_to_xml, spaces_node_mutation_allowed, warn,
    BareJid, PubSubError, PubSubItem, Session, WebSocketState,
};
use crate::server::routes::websocket::handlers::pubsub_fanout;
use crate::server::routes::websocket::ResolvedPrincipal;

/// Publish to a community pubsub node — the XEP-0472 social feed at
/// `urn:xmpp:pubsub-social-feed:1` or the xCal events node. Both live on
/// `community.<domain>` (distinct from the spaces service so the spaces
/// enumeration only returns real spaces). The feed is member-postable
/// (gated by the node's pubsub publish-model); calendar event creation
/// keeps the owner-only spaces gate. The standard pubsub fan-out runs
/// either way so subscribers see new posts in real time. XEP-0501 stories
/// are not here: they live on each author's own PEP service.
pub(super) async fn handle_community_publish(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    item: PubSubItem,
    principal: Option<ResolvedPrincipal<'_>>,
) -> Vec<String> {
    let session = principal.map(ResolvedPrincipal::session);
    if node != waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED
        && node != waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS
    {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
    }
    // RSVP carve-out: the events node accepts per-attendee RSVP items
    // from any authenticated session, bypassing the
    // server-owner/space-owner gate. Each user owns their own RSVP
    // item (`<master-uid>-rsvp-<localpart>`) carrying a single
    // attendee whose URI bare-JID matches the publisher. This keeps
    // RSVPs scoped to the publishing user and avoids granting
    // Publisher affiliation on the master events node.
    if node == waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS && session.is_some() {
        let user_domain = state.deps.auth_state.xmpp_domain.as_str();
        if is_well_formed_rsvp_item(session, user_domain, &item) {
            return handle_community_rsvp_publish(iq, state, community_domain, node, item).await;
        }
    }
    handle_community_non_bookmark_publish(iq, state, community_domain, node, item, session).await
}

/// The authenticated publisher permitted to post to an open community
/// node, or `None` if the request is not authorized.
///
/// The feed is member-postable: any authenticated member may
/// post when the node's `publish_model` is Open. This honors the node's
/// configured pubsub publish-model via `can_publish` rather than the
/// owner-only Spaces structural gate, while still requiring an
/// authenticated session and rejecting outcasts. The returned JID is
/// the server-side session principal — the value the service stamps as
/// the item publisher (XEP-0060 §7.1.2.1).
pub(super) async fn community_member_publisher(
    state: &WebSocketState,
    session: Option<&Session>,
    community_jid: &BareJid,
    node: &str,
) -> Result<Option<BareJid>, String> {
    let Some(session) = session else {
        return Ok(None);
    };
    let user_domain = state.deps.auth_state.xmpp_domain.as_str();
    let entity = session_bare_jid(session, user_domain)
        .map_err(|_| "invalid session JID for community publish".to_string())?;
    let allowed = crate::pubsub_authz::can_publish(
        &state.deps.protocol.pubsub_storage,
        community_jid,
        node,
        &entity,
        false,
    )
    .await
    .map_err(|error| format!("can_publish failed for community node: {error}"))?;
    Ok(allowed.then_some(entity))
}

/// Publish a non-bookmark item to a community pubsub node. Used by
/// `handle_community_publish` (feed + calendar events on
/// `community.<domain>`). The feed is member-postable (gated
/// by the node's pubsub publish-model via `community_member_publisher`)
/// and the item is stamped with the server-derived publisher; calendar
/// events keep the owner-only Spaces gate except for validated RSVP
/// items handled earlier. Either way the standard pubsub fan-out runs
/// so subscribers see new posts in real time.
async fn handle_community_non_bookmark_publish(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    mut item: PubSubItem,
    session: Option<&Session>,
) -> Vec<String> {
    let Ok(community_jid) = community_domain.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    // Resolve the node before authorizing or mutating it. Community nodes
    // are bootstrapped at startup; checking existence first keeps the
    // error semantics correct (NodeNotFound, not a misleading Forbidden)
    // for the deleted-node edge case.
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
    // The feed is member-postable and the service stamps the
    // authenticated publisher on each item. Calendar events stay owner-only
    // and carry no publisher attribution here. A storage/permission backend
    // failure is an internal error, not an authorization denial.
    let publisher = if node == waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED {
        match community_member_publisher(state, session, &community_jid, node).await {
            Ok(Some(entity)) => Some(entity),
            Ok(None) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
            Err(error) => {
                warn!(node, error = %error, "Failed to authorize community member publish");
                return vec![iq_to_xml(build_pubsub_error(
                    iq,
                    PubSubError::InternalServerError,
                ))];
            }
        }
    } else {
        match spaces_node_mutation_allowed(
            state,
            session.map(ResolvedPrincipal::from_authenticated_session),
            node,
        )
        .await
        {
            Ok(true) => None,
            Ok(false) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
            Err(error) => {
                warn!(node, error = %error, "Failed to authorize community publish");
                return vec![iq_to_xml(build_pubsub_error(
                    iq,
                    PubSubError::InternalServerError,
                ))];
            }
        }
    };

    // The feed is a shared, open-publish node, so the service binds
    // each item to its author: stamp the authenticated JID into Atom author
    // fields so a member cannot impersonate another. The storage write below
    // atomically refuses cross-publisher item-id clobbering.
    // `publisher` is `Some` only for the open member-postable feed node.
    if let Some(entity) = publisher.as_ref() {
        if node == waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED {
            let Some(payload) = item.payload.as_ref() else {
                return vec![iq_to_xml(build_pubsub_error(
                    iq,
                    PubSubError::PayloadRequired,
                ))];
            };
            // The service assigns the item id when the publisher omits it
            // (XEP-0060 §7.1.2.1) so the alternate link below can name it.
            let item_id = item
                .id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            if waddle_xmpp_core::xep0472::parse_feed_entry(&item_id, payload).is_none() {
                return vec![iq_to_xml(build_pubsub_error(
                    iq,
                    PubSubError::InvalidPayload,
                ))];
            }
            let stamped =
                waddle_xmpp_core::xep0472::stamp_feed_entry_author(payload, &entity.to_string());
            // XEP-0472 §"Publishing a Post": the `xmpp:` alternate link
            // "SHOULD be inserted at server side".
            let href = format!(
                "xmpp:{community_domain}?;node={};item={}",
                urlencoding::encode(node),
                urlencoding::encode(&item_id)
            );
            item.payload = Some(waddle_xmpp_core::xep0472::ensure_feed_entry_xmpp_link(
                &stamped, &href,
            ));
            item.id = Some(item_id);
        }
    }

    let publish_result = if let Some(entity) = publisher.as_ref() {
        state
            .deps
            .protocol
            .pubsub_storage
            .publish_item_if_missing_or_publisher(&community_jid, node, &item, entity, false)
            .await
    } else {
        state
            .deps
            .protocol
            .pubsub_storage
            .publish_item(&community_jid, node, &item, None, false)
            .await
    };
    match publish_result {
        Ok(result) => {
            pubsub_fanout::fan_out_publish(
                state,
                pubsub_fanout::FanOutRequest {
                    owner: &community_jid,
                    node,
                    published_item: &item,
                    item_id: &result.item_id,
                    publisher: publisher.as_ref(),
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
                community_publish_error_from_xmpp(&error),
            ))]
        }
    }
}

fn community_publish_error_from_xmpp(error: &waddle_xmpp::XmppError) -> PubSubError {
    match error {
        waddle_xmpp::XmppError::PubSubPayloadRequired(_) => PubSubError::PayloadRequired,
        waddle_xmpp::XmppError::PubSubInvalidPayload(_) => PubSubError::InvalidPayload,
        waddle_xmpp::XmppError::Stanza {
            condition: waddle_xmpp::StanzaErrorCondition::Forbidden,
            ..
        }
        | waddle_xmpp::XmppError::PermissionDenied(_) => PubSubError::Forbidden,
        waddle_xmpp::XmppError::Stanza {
            condition: waddle_xmpp::StanzaErrorCondition::ItemNotFound,
            ..
        } => PubSubError::NodeNotFound,
        waddle_xmpp::XmppError::Stanza {
            condition: waddle_xmpp::StanzaErrorCondition::BadRequest,
            ..
        } => PubSubError::BadRequest,
        _ => PubSubError::InternalServerError,
    }
}
