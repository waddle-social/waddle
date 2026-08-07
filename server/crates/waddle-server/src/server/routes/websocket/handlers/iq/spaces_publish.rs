use super::permissions::{
    delete_channel_parent_tuple, write_channel_parent_tuple, write_channel_parent_tuple_if_absent,
};
use super::spaces_bookmark_cleanup::cleanup_stale_space_bookmarks;
use super::*;
use crate::server::routes::websocket::handlers::pubsub_fanout;
use crate::server::routes::websocket::ResolvedPrincipal;
use crate::space_identity::{space_jid_for_node, SpaceNode};

pub(super) async fn handle_spaces_publish(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    muc_domain: &str,
    spaces_domain: &str,
    node: &str,
    item: PubSubItem,
    principal: Option<ResolvedPrincipal<'_>>,
) -> Vec<String> {
    match spaces_node_mutation_allowed(state, principal, node).await {
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

    let stale_nodes = match state
        .deps
        .protocol
        .pubsub_storage
        .list_node_names_for_item(&spaces_jid, item_id)
        .await
    {
        Ok(stale_nodes) => stale_nodes,
        Err(error) => {
            warn!(
                channel_id = %channel_id,
                node,
                item_id,
                error = %error,
                "Failed to enumerate prior Space nodes for room bookmark"
            );
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    };
    let previous_link = match state
        .deps
        .app_state
        .channel_space_link_store
        .get(&bookmark.jid)
        .await
    {
        Ok(link) => link,
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to read channel-space link before Spaces publish");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    };
    let current_item_filter = [item_id.to_string()];
    let previous_item = match state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces_jid, node, Some(1), &current_item_filter)
        .await
    {
        Ok(items) => items
            .into_iter()
            .next()
            .map(|stored| stored.to_pubsub_item()),
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to read existing Spaces item before publish");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    };

    match state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces_jid, node, &item, None, false)
        .await
    {
        Ok(result) => {
            let space_node = SpaceNode::from(node);
            let projected_space_jid = space_jid_for_node(&spaces_jid, &space_node);
            if let Some(space_jid) = projected_space_jid.as_ref() {
                if let Err(error) = state
                    .deps
                    .app_state
                    .channel_space_link_store
                    .set(&crate::channel_space_links::ChannelSpaceLink {
                        channel_jid: bookmark.jid.clone(),
                        space_jid: space_jid.clone(),
                        space_node: space_node.clone(),
                        created_at: chrono::Utc::now().timestamp(),
                    })
                    .await
                {
                    warn!(
                        channel_id = %channel_id,
                        node,
                        error = %error,
                        "Published Spaces item but failed to sync channel-space link projection; \
                         rolling back to keep PubSub and durable discovery state consistent"
                    );
                    if let Err(rollback_error) = rollback_spaces_publish(
                        state,
                        SpacesPublishRollback {
                            spaces_jid: &spaces_jid,
                            node,
                            item_id: &result.item_id,
                            previous_item: previous_item.as_ref(),
                            channel_id: &channel_id,
                            channel_jid: &bookmark.jid,
                            previous_link: previous_link.as_ref(),
                            parent_tuple_created: false,
                        },
                    )
                    .await
                    {
                        warn!(
                            channel_id = %channel_id,
                            node,
                            error = %rollback_error,
                            "Failed to roll back Spaces publish after link projection failure"
                        );
                    }
                    return vec![iq_to_xml(build_pubsub_error(
                        iq,
                        PubSubError::InternalServerError,
                    ))];
                }
            } else {
                warn!(
                    node,
                    spaces = %spaces_jid,
                    "Clearing channel-space link projection for non-JID Space node"
                );
                if let Err(error) = state
                    .deps
                    .app_state
                    .channel_space_link_store
                    .clear(&bookmark.jid)
                    .await
                {
                    warn!(
                        channel_id = %channel_id,
                        node,
                        error = %error,
                        "Published Spaces item but failed to clear stale channel-space link projection; \
                         rolling back to keep PubSub and durable discovery state consistent"
                    );
                    if let Err(rollback_error) = rollback_spaces_publish(
                        state,
                        SpacesPublishRollback {
                            spaces_jid: &spaces_jid,
                            node,
                            item_id: &result.item_id,
                            previous_item: previous_item.as_ref(),
                            channel_id: &channel_id,
                            channel_jid: &bookmark.jid,
                            previous_link: previous_link.as_ref(),
                            parent_tuple_created: false,
                        },
                    )
                    .await
                    {
                        warn!(
                            channel_id = %channel_id,
                            node,
                            error = %rollback_error,
                            "Failed to roll back Spaces publish after link projection clear failure"
                        );
                    }
                    return vec![iq_to_xml(build_pubsub_error(
                        iq,
                        PubSubError::InternalServerError,
                    ))];
                }
            }
            let parent_tuple_created =
                match write_channel_parent_tuple_if_absent(state, &channel_id, node).await {
                    Ok(created) => created,
                    Err(error) => {
                        warn!(
                            channel_id = %channel_id,
                            node,
                            error = %error,
                            "Published Spaces item but failed to sync channel parent tuple; \
                             rolling back to keep PubSub and permission graph consistent"
                        );
                        if let Err(rollback_error) = rollback_spaces_publish(
                            state,
                            SpacesPublishRollback {
                                spaces_jid: &spaces_jid,
                                node,
                                item_id: &result.item_id,
                                previous_item: previous_item.as_ref(),
                                channel_id: &channel_id,
                                channel_jid: &bookmark.jid,
                                previous_link: previous_link.as_ref(),
                                parent_tuple_created: false,
                            },
                        )
                        .await
                        {
                            warn!(
                                channel_id = %channel_id,
                                node,
                                error = %rollback_error,
                                "Failed to roll back Spaces publish after parent tuple failure"
                            );
                        }
                        return vec![iq_to_xml(build_pubsub_error(
                            iq,
                            PubSubError::InternalServerError,
                        ))];
                    }
                };
            if let Err(error) = cleanup_stale_space_bookmarks(
                state,
                &spaces_jid,
                &channel_id,
                node,
                item_id,
                &stale_nodes,
            )
            .await
            {
                warn!(
                    channel_id = %channel_id,
                    node,
                    item_id,
                    error = %error,
                    "Published Spaces item but failed to clean stale prior Space bookmarks"
                );
                if let Err(rollback_error) = rollback_spaces_publish(
                    state,
                    SpacesPublishRollback {
                        spaces_jid: &spaces_jid,
                        node,
                        item_id: &result.item_id,
                        previous_item: previous_item.as_ref(),
                        channel_id: &channel_id,
                        channel_jid: &bookmark.jid,
                        previous_link: previous_link.as_ref(),
                        parent_tuple_created,
                    },
                )
                .await
                {
                    warn!(
                        channel_id = %channel_id,
                        node,
                        item_id,
                        error = %rollback_error,
                        "Failed to roll back Spaces publish after stale cleanup failure"
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

async fn restore_channel_space_link_projection(
    state: &WebSocketState,
    channel_jid: &BareJid,
    previous_link: Option<&crate::channel_space_links::ChannelSpaceLink>,
) -> Result<(), String> {
    if let Some(link) = previous_link {
        state
            .deps
            .app_state
            .channel_space_link_store
            .set(link)
            .await
            .map_err(|error| format!("channel-space link restore failed: {error}"))?;
    } else {
        state
            .deps
            .app_state
            .channel_space_link_store
            .clear(channel_jid)
            .await
            .map_err(|error| format!("channel-space link clear failed: {error}"))?;
    }
    Ok(())
}

struct SpacesPublishRollback<'a> {
    spaces_jid: &'a BareJid,
    node: &'a str,
    item_id: &'a str,
    previous_item: Option<&'a PubSubItem>,
    channel_id: &'a str,
    channel_jid: &'a BareJid,
    previous_link: Option<&'a crate::channel_space_links::ChannelSpaceLink>,
    parent_tuple_created: bool,
}

async fn rollback_spaces_publish(
    state: &WebSocketState,
    rollback: SpacesPublishRollback<'_>,
) -> Result<(), String> {
    if rollback.parent_tuple_created {
        delete_channel_parent_tuple(state, rollback.channel_id, rollback.node)
            .await
            .map_err(|error| format!("parent tuple rollback failed: {error}"))?;
    }
    if let Some(item) = rollback.previous_item {
        if let Err(error) = state
            .deps
            .protocol
            .pubsub_storage
            .publish_item(rollback.spaces_jid, rollback.node, item, None, false)
            .await
        {
            if rollback.parent_tuple_created {
                let _ = write_channel_parent_tuple(state, rollback.channel_id, rollback.node).await;
            }
            return Err(format!("pubsub restore Spaces item failed: {error}"));
        }
    } else {
        if let Err(error) = state
            .deps
            .protocol
            .pubsub_storage
            .retract_item(rollback.spaces_jid, rollback.node, rollback.item_id)
            .await
        {
            if rollback.parent_tuple_created {
                let _ = write_channel_parent_tuple(state, rollback.channel_id, rollback.node).await;
            }
            return Err(format!("pubsub rollback Spaces item failed: {error}"));
        }
    }
    restore_channel_space_link_projection(state, rollback.channel_jid, rollback.previous_link)
        .await?;
    Ok(())
}
