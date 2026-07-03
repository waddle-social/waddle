use super::permissions::{
    delete_channel_parent_tuple, write_channel_parent_tuple, write_channel_parent_tuple_if_absent,
};
use super::spaces_bookmark_cleanup::cleanup_stale_space_bookmarks;
use super::*;
use crate::space_identity::SpaceNode;

pub(super) async fn handle_spaces_items(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    spaces_domain: &str,
    requester_jid: &BareJid,
    node: &str,
    max_items: Option<u32>,
    item_ids: &[String],
) -> Vec<String> {
    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    match crate::pubsub_authz::can_subscribe(
        &state.deps.protocol.pubsub_storage,
        &spaces_jid,
        node,
        requester_jid,
        false,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            let node_meta = state
                .deps
                .protocol
                .pubsub_storage
                .get_node(&spaces_jid, node)
                .await
                .ok()
                .flatten();
            let is_outcast = crate::pubsub_authz::effective_affiliation(
                &state.deps.protocol.pubsub_storage,
                &spaces_jid,
                node,
                requester_jid,
                false,
            )
            .await
            .is_ok_and(|affiliation| affiliation.is_outcast());
            let error = if let Some(node_meta) = node_meta {
                if is_outcast {
                    PubSubError::Forbidden
                } else if matches!(
                    node_meta.config.access_model,
                    waddle_xmpp::pubsub::AccessModel::Whitelist
                ) {
                    PubSubError::ClosedNode
                } else {
                    PubSubError::Forbidden
                }
            } else {
                PubSubError::NodeNotFound
            };
            return vec![iq_to_xml(build_pubsub_error(iq, error))];
        }
        Err(error) => {
            warn!(
                node,
                requester = %requester_jid,
                error = %error,
                "Failed to authorize Spaces items access"
            );
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
        }
    }
    backfill_missing_space_bookmarks(state, &spaces_jid, node, item_ids).await;
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

async fn backfill_missing_space_bookmarks(
    state: &WebSocketState,
    spaces_jid: &BareJid,
    node: &str,
    item_ids: &[String],
) {
    let space_node = SpaceNode::from(node);
    let links = match state
        .deps
        .app_state
        .channel_space_link_store
        .list_channels_in_space_node(&space_node)
        .await
    {
        Ok(links) => links,
        Err(error) => {
            warn!(node, error = %error, "Failed to list channel-space links for Spaces bookmark backfill");
            return;
        }
    };
    if links.is_empty() {
        return;
    }

    let existing_ids = match state
        .deps
        .protocol
        .pubsub_storage
        .get_items(spaces_jid, node, None, &[])
        .await
    {
        Ok(items) => items
            .into_iter()
            .map(|item| item.id)
            .collect::<std::collections::HashSet<_>>(),
        Err(error) => {
            warn!(node, error = %error, "Failed to inspect Spaces items before bookmark backfill");
            return;
        }
    };

    for channel_jid in links {
        let item_id = channel_jid.to_string();
        if existing_ids.contains(&item_id)
            || (!item_ids.is_empty() && !item_ids.iter().any(|id| id == &item_id))
        {
            continue;
        }
        let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&channel_jid) else {
            warn!(channel = %channel_jid, "Skipping Spaces bookmark backfill for non-managed room JID");
            continue;
        };
        let room = match state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
                room_jid: channel_jid.clone(),
            })
            .await
        {
            Ok(Some(room)) => room,
            Ok(None) => {
                warn!(channel = %channel_jid, "Skipping Spaces bookmark backfill for missing MUC room");
                continue;
            }
            Err(error) => {
                warn!(channel = %channel_jid, error = %error, "Failed to load MUC room for Spaces bookmark backfill");
                continue;
            }
        };
        let config = match room.ask(waddle_xmpp::muc::room_actor::GetConfig).await {
            Ok(config) => config,
            Err(error) => {
                warn!(channel = %channel_jid, error = %error, "Failed to load room config for Spaces bookmark backfill");
                continue;
            }
        };
        let Some(channel_type) =
            channel_type_for_space_bookmark(state, &channel_id, &channel_jid, &config).await
        else {
            continue;
        };
        let item = match waddle_xmpp::xep::build_channel_item(
            &waddle_xmpp::ChannelInfo {
                id: channel_id.clone(),
                name: config.name,
                channel_type,
            },
            &state.deps.service_domains.muc,
        ) {
            Ok(item) => item,
            Err(error) => {
                warn!(channel = %channel_jid, error = %error, "Failed to build Spaces bookmark backfill item");
                continue;
            }
        };
        let stale_nodes = match state
            .deps
            .protocol
            .pubsub_storage
            .list_node_names_for_item(spaces_jid, &item_id)
            .await
        {
            Ok(stale_nodes) => stale_nodes,
            Err(error) => {
                warn!(channel = %channel_jid, node, error = %error, "Failed to enumerate stale Spaces bookmarks before backfill");
                continue;
            }
        };
        let publish_result = match state
            .deps
            .protocol
            .pubsub_storage
            .publish_item(spaces_jid, node, &item, None, false)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                warn!(channel = %channel_jid, node, error = %error, "Failed to backfill Spaces bookmark item");
                continue;
            }
        };
        let parent_tuple_created = match write_channel_parent_tuple_if_absent(
            state,
            &channel_id,
            node,
        )
        .await
        {
            Ok(created) => created,
            Err(error) => {
                let _ = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .retract_item(spaces_jid, node, &publish_result.item_id)
                    .await;
                warn!(channel = %channel_jid, node, error = %error, "Backfilled Spaces bookmark but failed to repair channel parent tuple; retracted backfill item");
                continue;
            }
        };
        if let Err(error) = cleanup_stale_space_bookmarks(
            state,
            spaces_jid,
            &channel_id,
            node,
            &item_id,
            &stale_nodes,
        )
        .await
        {
            let tuple_ready_for_retract = if parent_tuple_created {
                match delete_channel_parent_tuple(state, &channel_id, node).await {
                    Ok(_) => true,
                    Err(delete_error) => {
                        warn!(
                            channel = %channel_jid,
                            node,
                            error = %delete_error,
                            "Failed to delete operation-created parent tuple after stale cleanup failure; preserving backfilled Spaces bookmark"
                        );
                        false
                    }
                }
            } else {
                true
            };
            if tuple_ready_for_retract {
                match state
                    .deps
                    .protocol
                    .pubsub_storage
                    .retract_item(spaces_jid, node, &publish_result.item_id)
                    .await
                {
                    Ok(_) => {}
                    Err(retract_error) => {
                        if parent_tuple_created {
                            let _ = write_channel_parent_tuple(state, &channel_id, node).await;
                        }
                        warn!(
                            channel = %channel_jid,
                            node,
                            error = %retract_error,
                            "Failed to retract backfilled Spaces bookmark after stale cleanup failure; preserving repaired parent tuple"
                        );
                    }
                }
            }
            warn!(channel = %channel_jid, node, error = %error, "Backfilled Spaces bookmark but failed to clean stale bookmarks");
        }
    }
}

async fn channel_type_for_space_bookmark(
    state: &WebSocketState,
    channel_id: &str,
    channel_jid: &BareJid,
    config: &waddle_xmpp::muc::RoomConfig,
) -> Option<String> {
    match get_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        channel_id,
    )
    .await
    {
        Ok(existing) => Some(channel_type_from_catalog_or_room_config(
            existing.as_ref().map(|row| row.channel_type.as_str()),
            config,
        )),
        Err(error) => {
            warn!(
                channel = %channel_jid,
                channel_id,
                error = %error,
                "Failed to load channel catalog type for Spaces bookmark backfill"
            );
            None
        }
    }
}

fn channel_type_from_catalog_or_room_config(
    catalog_channel_type: Option<&str>,
    config: &waddle_xmpp::muc::RoomConfig,
) -> String {
    catalog_channel_type
        .and_then(waddle_xmpp::ChannelType::parse)
        .map(|channel_type| channel_type.as_str().to_string())
        .unwrap_or_else(|| channel_type_from_room_config(config).to_string())
}

fn channel_type_from_room_config(config: &waddle_xmpp::muc::RoomConfig) -> &'static str {
    if config.group_dm {
        waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM
    } else if config.forum {
        "forum"
    } else if config.moderated {
        "announcement"
    } else {
        "text"
    }
}

#[cfg(test)]
mod channel_type_projection_tests {
    use super::*;

    #[test]
    fn channel_type_from_room_config_preserves_announcement() {
        let config = waddle_xmpp::muc::RoomConfig {
            moderated: true,
            ..waddle_xmpp::muc::RoomConfig::default()
        };

        assert_eq!(channel_type_from_room_config(&config), "announcement");
    }

    #[test]
    fn catalog_channel_type_takes_precedence_over_room_config() {
        let config = waddle_xmpp::muc::RoomConfig {
            moderated: false,
            ..waddle_xmpp::muc::RoomConfig::default()
        };

        assert_eq!(
            channel_type_from_catalog_or_room_config(Some("announcement"), &config),
            "announcement"
        );
    }
}
