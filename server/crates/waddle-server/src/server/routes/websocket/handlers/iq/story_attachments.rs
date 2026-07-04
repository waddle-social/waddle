use super::session_jid::session_bare_jid;
use super::*;
use crate::server::routes::websocket::handlers::pubsub_fanout;

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoryAttachmentTarget {
    item_id: String,
}

pub(super) fn is_story_attachment_node(node: &str, community_domain: &str) -> bool {
    parse_story_attachment_target(node, community_domain).is_some()
}

fn story_attachment_summary_node() -> String {
    format!(
        "{}/{}",
        waddle_xmpp::xep::xep0470::NS_PUBSUB_ATTACHMENTS_SUMMARY,
        waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES
    )
}

pub(super) fn is_story_attachment_summary_node(node: &str) -> bool {
    node == story_attachment_summary_node()
}

fn parse_story_attachment_target(
    node: &str,
    community_domain: &str,
) -> Option<StoryAttachmentTarget> {
    let prefix = format!(
        "{}/",
        waddle_xmpp::xep::xep0470::PUBSUB_ATTACHMENTS_NODE_PREFIX
    );
    let target_uri = node.strip_prefix(&prefix)?;
    let target_uri = target_uri.strip_prefix("xmpp:")?;
    let (target_domain, query) = target_uri.split_once("?;")?;
    if target_domain != community_domain {
        return None;
    }
    let mut parts = query.split(';');
    let node_param = parts.next()?;
    let item_param = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let ("node", encoded_node) = node_param.split_once('=')? else {
        return None;
    };
    let ("item", encoded_item) = item_param.split_once('=')? else {
        return None;
    };
    let target_node = urlencoding::decode(encoded_node).ok()?.into_owned();
    if target_node != waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES {
        return None;
    }
    let item_id = urlencoding::decode(encoded_item).ok()?.into_owned();
    if item_id.is_empty() {
        return None;
    }
    let canonical = format!(
        "{prefix}xmpp:{community_domain}?;node={};item={}",
        urlencoding::encode(waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES),
        urlencoding::encode(&item_id)
    );
    if node != canonical {
        return None;
    }
    Some(StoryAttachmentTarget { item_id })
}

fn story_attachment_node_for_item(community_domain: &str, item_id: &str) -> String {
    format!(
        "{}/xmpp:{community_domain}?;node={};item={}",
        waddle_xmpp::xep::xep0470::PUBSUB_ATTACHMENTS_NODE_PREFIX,
        urlencoding::encode(waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES),
        urlencoding::encode(item_id)
    )
}

fn validated_story_attachment_item(
    session: Option<&Session>,
    user_domain: &str,
    item: &PubSubItem,
) -> Result<BareJid, PubSubError> {
    let Some(session) = session else {
        return Err(PubSubError::Forbidden);
    };
    let publisher = session_bare_jid(session, user_domain)?;
    let Some(item_id) = item.id.as_deref() else {
        return Err(PubSubError::BadRequest);
    };
    let requested_publisher = item_id
        .parse::<BareJid>()
        .map_err(|_| PubSubError::BadRequest)?;
    if requested_publisher != publisher {
        return Err(PubSubError::BadRequest);
    }
    let Some(payload) = item.payload.as_ref() else {
        return Err(PubSubError::BadRequest);
    };
    let Some(attachments) = waddle_xmpp::xep::xep0470::parse_attachments_element(payload) else {
        return Err(PubSubError::BadRequest);
    };
    if let Some(reactions) = attachments.reactions_set() {
        reactions.validate().map_err(|_| PubSubError::BadRequest)?;
    }
    Ok(publisher)
}

async fn ensure_story_attachment_node(
    state: &WebSocketState,
    community_jid: &BareJid,
    community_domain: &str,
    attachment_node: &str,
) -> Result<(), PubSubError> {
    let storage = &state.deps.protocol.pubsub_storage;
    let Some(target) = parse_story_attachment_target(attachment_node, community_domain) else {
        return Err(PubSubError::BadRequest);
    };
    let stories_node = storage
        .get_node(
            community_jid,
            waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES,
        )
        .await
        .map_err(|_| PubSubError::NodeNotFound)?
        .ok_or(PubSubError::NodeNotFound)?;
    let story_items = storage
        .get_items(
            community_jid,
            waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES,
            None,
            &[target.item_id],
        )
        .await
        .map_err(|_| PubSubError::NodeNotFound)?;
    if story_items.is_empty() {
        return Err(PubSubError::ItemNotFound);
    }
    let (node, created) = storage
        .get_or_create_node(community_jid, attachment_node)
        .await
        .map_err(|_| PubSubError::InternalServerError)?;
    if created
        || node.config.access_model != stories_node.config.access_model
        || node.config.publish_model != stories_node.config.publish_model
        || node.config.max_items != 10_000
    {
        let mut config = node.config;
        config.access_model = stories_node.config.access_model;
        config.publish_model = stories_node.config.publish_model;
        config.max_items = 10_000;
        storage
            .update_node_config(community_jid, attachment_node, &config)
            .await
            .map_err(|_| PubSubError::InternalServerError)?;
    }
    Ok(())
}

pub(super) async fn ensure_story_attachment_summary_node(
    state: &WebSocketState,
    community_jid: &BareJid,
) -> Result<(), PubSubError> {
    let storage = &state.deps.protocol.pubsub_storage;
    let stories_node = storage
        .get_node(
            community_jid,
            waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES,
        )
        .await
        .map_err(|_| PubSubError::NodeNotFound)?
        .ok_or(PubSubError::NodeNotFound)?;
    let summary_node = story_attachment_summary_node();
    let (node, created) = storage
        .get_or_create_node(community_jid, &summary_node)
        .await
        .map_err(|_| PubSubError::InternalServerError)?;
    if created
        || node.config.access_model != stories_node.config.access_model
        || node.config.publish_model != stories_node.config.publish_model
        || node.config.max_items != 10_000
    {
        let mut config = node.config;
        config.access_model = stories_node.config.access_model;
        config.publish_model = stories_node.config.publish_model;
        config.max_items = 10_000;
        storage
            .update_node_config(community_jid, &summary_node, &config)
            .await
            .map_err(|_| PubSubError::InternalServerError)?;
    }
    Ok(())
}

async fn update_story_attachment_summary(
    state: &WebSocketState,
    community_jid: &BareJid,
    community_domain: &str,
    attachment_node: &str,
) -> Result<(), PubSubError> {
    let Some(target) = parse_story_attachment_target(attachment_node, community_domain) else {
        return Err(PubSubError::BadRequest);
    };
    ensure_story_attachment_summary_node(state, community_jid).await?;

    let storage = &state.deps.protocol.pubsub_storage;
    let attachment_items = storage
        .get_items(community_jid, attachment_node, None, &[])
        .await
        .map_err(|_| PubSubError::InternalServerError)?;

    let mut attachments = Vec::new();
    for item in attachment_items {
        let pubsub_item = item.to_pubsub_item();
        let Some(publisher) = pubsub_item.publisher else {
            continue;
        };
        let Some(payload) = pubsub_item.payload.as_ref() else {
            continue;
        };
        let Some(attachment) = waddle_xmpp::xep::xep0470::parse_attachments_element(payload) else {
            continue;
        };
        attachments.push((publisher, attachment));
    }

    let summary = waddle_xmpp::xep::xep0470::summarize_attachments(attachments);
    let summary_item = PubSubItem::new(
        Some(target.item_id),
        Some(waddle_xmpp::xep::xep0470::build_summary_element(&summary)),
    );
    let summary_node = story_attachment_summary_node();
    let result = storage
        .publish_item(
            community_jid,
            &summary_node,
            &summary_item,
            Some(community_jid),
            false,
        )
        .await
        .map_err(|_| PubSubError::InternalServerError)?;
    pubsub_fanout::fan_out_publish(
        state,
        pubsub_fanout::FanOutRequest {
            owner: community_jid,
            node: &summary_node,
            published_item: &summary_item,
            item_id: &result.item_id,
            publisher: Some(community_jid),
            publisher_full: None,
            is_pep: false,
        },
    )
    .await;
    Ok(())
}

pub(super) async fn handle_story_attachment_publish(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    item: PubSubItem,
    session: Option<&Session>,
) -> Vec<String> {
    let user_domain = state.deps.auth_state.xmpp_domain.as_str();
    let publisher = match validated_story_attachment_item(session, user_domain, &item) {
        Ok(publisher) => publisher,
        Err(error) => return vec![iq_to_xml(build_pubsub_error(iq, error))],
    };
    let Ok(community_jid) = community_domain.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    if let Err(error) =
        ensure_story_attachment_node(state, &community_jid, community_domain, node).await
    {
        return vec![iq_to_xml(build_pubsub_error(iq, error))];
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&community_jid, node, &item, Some(&publisher), false)
        .await
    {
        Ok(result) => {
            if let Err(error) =
                update_story_attachment_summary(state, &community_jid, community_domain, node).await
            {
                warn!(
                    node,
                    error = ?error,
                    "Failed to update story attachment summary after accepted publish"
                );
            }
            pubsub_fanout::fan_out_publish(
                state,
                pubsub_fanout::FanOutRequest {
                    owner: &community_jid,
                    node,
                    published_item: &item,
                    item_id: &result.item_id,
                    publisher: Some(&publisher),
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
            warn!(node, error = %error, "Failed to publish story attachment item");
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))]
        }
    }
}

pub(super) async fn cleanup_story_attachment_state(
    state: &WebSocketState,
    community_jid: &BareJid,
    community_domain: &str,
    story_id: &str,
) -> Result<(), PubSubError> {
    let storage = &state.deps.protocol.pubsub_storage;
    let attachment_node = story_attachment_node_for_item(community_domain, story_id);
    storage
        .delete_node(community_jid, &attachment_node)
        .await
        .map_err(|_| PubSubError::InternalServerError)?;

    let summary_node = story_attachment_summary_node();
    let summary_removed = storage
        .retract_item(community_jid, &summary_node, story_id)
        .await
        .map_err(|_| PubSubError::InternalServerError)?;
    if summary_removed {
        pubsub_fanout::fan_out_retract(
            state,
            pubsub_fanout::FanOutRetractRequest {
                owner: community_jid,
                node: &summary_node,
                item_id: story_id,
            },
        )
        .await;
    }
    Ok(())
}

pub(super) async fn handle_story_attachment_retract(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    item_id: &str,
    session: Option<&Session>,
) -> Vec<String> {
    let user_domain = state.deps.auth_state.xmpp_domain.as_str();
    let Some(session) = session else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
    };
    let publisher = match session_bare_jid(session, user_domain) {
        Ok(publisher) => publisher,
        Err(error) => return vec![iq_to_xml(build_pubsub_error(iq, error))],
    };
    let requested_publisher = match item_id.parse::<BareJid>() {
        Ok(requested_publisher) => requested_publisher,
        Err(_) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::BadRequest))],
    };
    if requested_publisher != publisher {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::BadRequest))];
    }
    let Ok(community_jid) = community_domain.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .retract_item(&community_jid, node, publisher.as_str())
        .await
    {
        Ok(true) => {
            if let Err(error) =
                update_story_attachment_summary(state, &community_jid, community_domain, node).await
            {
                warn!(
                    node,
                    error = ?error,
                    "Failed to update story attachment summary after accepted retract"
                );
            }
            vec![iq_to_xml(build_pubsub_success(iq))]
        }
        Ok(false) => vec![iq_to_xml(build_pubsub_error(iq, PubSubError::ItemNotFound))],
        Err(error) => {
            warn!(node, item_id, error = %error, "Failed to retract story attachment item");
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))]
        }
    }
}

#[cfg(test)]
mod story_attachment_target_tests {
    use super::*;

    const COMMUNITY: &str = "community.localhost";
    const PREFIX: &str = waddle_xmpp::xep::xep0470::PUBSUB_ATTACHMENTS_NODE_PREFIX;

    #[test]
    fn parses_only_canonical_story_attachment_target() {
        let node = format!(
            "{PREFIX}/xmpp:{COMMUNITY}?;node=urn%3Axmpp%3Apubsub-social-feed%3Astories%3A0;item=story-1"
        );

        assert_eq!(
            parse_story_attachment_target(&node, COMMUNITY),
            Some(StoryAttachmentTarget {
                item_id: "story-1".to_owned()
            })
        );
    }

    #[test]
    fn rejects_lookalike_story_node_target() {
        let node = format!(
            "{PREFIX}/xmpp:{COMMUNITY}?;node=urn%3Axmpp%3Apubsub-social-feed%3Astories%3A0evil;item=story-1"
        );

        assert_eq!(parse_story_attachment_target(&node, COMMUNITY), None);
    }

    #[test]
    fn rejects_non_canonical_story_attachment_target() {
        let wrong_host = format!(
            "{PREFIX}/xmpp:other.localhost?;node=urn%3Axmpp%3Apubsub-social-feed%3Astories%3A0;item=story-1"
        );
        let extra_param = format!(
            "{PREFIX}/xmpp:{COMMUNITY}?;node=urn%3Axmpp%3Apubsub-social-feed%3Astories%3A0;item=story-1;x=1"
        );

        assert_eq!(parse_story_attachment_target(&wrong_host, COMMUNITY), None);
        assert_eq!(parse_story_attachment_target(&extra_param, COMMUNITY), None);
    }
}
