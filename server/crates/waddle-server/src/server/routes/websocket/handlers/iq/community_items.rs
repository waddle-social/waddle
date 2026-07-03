use super::story_attachments::{is_story_attachment_node, is_story_attachment_summary_node};
use super::*;

pub(super) async fn handle_community_items(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    max_items: Option<u32>,
    item_ids: &[String],
) -> Vec<String> {
    if !is_story_attachment_node(node, community_domain)
        && !is_story_attachment_summary_node(node)
        && node != waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED
        && node != waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES
        && node != waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS
    {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
    }
    let Ok(community_jid) = community_domain.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    // Community nodes (feed/stories/events) are server-managed: the
    // topology bootstrap creates them on every startup. If a read
    // hits before that bootstrap has run (or pre-dates the node's
    // introduction in an older prod DB), surface an empty result
    // rather than `item-not-found` so the chat lands on its empty
    // state instead of an error banner.
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&community_jid, node)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return vec![iq_to_xml(build_pubsub_items_result(iq, node, &[]))],
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
