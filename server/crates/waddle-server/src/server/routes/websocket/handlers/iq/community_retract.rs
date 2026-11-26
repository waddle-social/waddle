use super::community_publish::community_member_publisher;
use super::{
    build_pubsub_error, build_pubsub_success, iq_to_xml, spaces_node_mutation_allowed, warn,
    BareJid, PubSubError, WebSocketState,
};
use crate::server::routes::websocket::ResolvedPrincipal;

pub(super) async fn handle_community_retract(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    item_id: &str,
    principal: Option<ResolvedPrincipal<'_>>,
) -> Vec<String> {
    let session = principal.map(ResolvedPrincipal::session);
    if node != waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED
        && node != waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS
    {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
    }
    let Ok(community_jid) = community_domain.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    if node == waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED {
        match spaces_node_mutation_allowed(
            state,
            session.map(ResolvedPrincipal::from_authenticated_session),
            node,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => match community_member_publisher(state, session, &community_jid, node)
                .await
            {
                Ok(Some(entity)) => {
                    if let Some(error) =
                        community_item_retract_error(state, &community_jid, node, item_id, &entity)
                            .await
                    {
                        return vec![iq_to_xml(build_pubsub_error(iq, error))];
                    }
                }
                Ok(None) => {
                    return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
                }
                Err(error) => {
                    warn!(node, error = %error, "Failed to authorize community member retract");
                    return vec![iq_to_xml(build_pubsub_error(
                        iq,
                        PubSubError::InternalServerError,
                    ))];
                }
            },
            Err(error) => {
                warn!(node, error = %error, "Failed to authorize community owner retract");
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
            Ok(true) => {}
            Ok(false) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
            Err(error) => {
                warn!(node, error = %error, "Failed to authorize community retract");
                return vec![iq_to_xml(build_pubsub_error(
                    iq,
                    PubSubError::InternalServerError,
                ))];
            }
        }
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .retract_item(&community_jid, node, item_id)
        .await
    {
        Ok(true) => {
            vec![iq_to_xml(build_pubsub_success(iq))]
        }
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

/// Refuse to let `entity` retract an open community item published by a
/// different member. The feed node is member-postable, but item
/// deletion remains owner-admin or original-publisher only.
async fn community_item_retract_error(
    state: &WebSocketState,
    community_jid: &BareJid,
    node: &str,
    item_id: &str,
    entity: &BareJid,
) -> Option<PubSubError> {
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_items(community_jid, node, None, &[item_id.to_owned()])
        .await
    {
        Ok(existing) => match existing.first() {
            Some(existing_item) if existing_item.publisher.as_ref() != Some(entity) => {
                Some(PubSubError::Forbidden)
            }
            None => Some(PubSubError::ItemNotFound),
            _ => None,
        },
        Err(error) => {
            warn!(node, item_id, error = %error, "Failed to check community item retract ownership");
            Some(PubSubError::InternalServerError)
        }
    }
}
