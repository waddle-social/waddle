use super::permissions::{
    delete_channel_parent_tuple, permission_allowed, write_channel_parent_tuple,
    write_channel_parent_tuple_if_absent,
};
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

/// Bring a well-known PEP node's stored config into line with the
/// current `NodeConfig::pep_for_node` defaults BEFORE a publish lands
/// its item.
///
/// Use case: an earlier version of Waddle auto-created the
/// `urn:xmpp:vcard4` node with `AccessModel::Presence` (the bare
/// `pep_default()`). After XEP-0292 §6.1 was wired through
/// `pep_for_node` the canonical access model is `Open`, but the
/// already-created node stays on the old config until something
/// explicitly reconfigures it. A user retrying their first vCard4
/// publish after upgrading would otherwise still be invisible to
/// non-roster peers. We reconcile the config in-place so the next
/// publish lands on a spec-conformant node.
///
/// Scope is deliberately narrow: only nodes whose well-known defaults
/// are stricter than ad-hoc PEP defaults (currently `urn:xmpp:vcard4`)
/// — we don't bulk-rewrite arbitrary user node configs here.
pub(super) async fn reconcile_well_known_pep_node_config(
    state: &WebSocketState,
    owner: &BareJid,
    node: &str,
) {
    if node != waddle_xmpp_core::pubsub::PEP_NODE_VCARD4
        && node != waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_DND
    {
        return;
    }
    let storage = &state.deps.protocol.pubsub_storage;
    let existing = match storage.get_node(owner, node).await {
        Ok(Some(node)) => node,
        Ok(None) => return,
        Err(error) => {
            warn!(
                node,
                error = %error,
                "Failed to read PEP node config for reconcile-on-publish; \
                 letting publish proceed against whatever config is stored"
            );
            return;
        }
    };
    let canonical = waddle_xmpp_core::pubsub::NodeConfig::pep_for_node(node);
    if existing.config == canonical {
        return;
    }
    if let Err(error) = storage.update_node_config(owner, node, &canonical).await {
        warn!(
            node,
            error = %error,
            "Failed to reconcile PEP node config to XEP-defaults on publish; \
             publish will proceed against the divergent config"
        );
    }
}

pub(super) fn spaces_service_bare_jid(spaces_domain: &str) -> Result<BareJid, String> {
    spaces_domain
        .parse::<BareJid>()
        .map_err(|error| format!("invalid spaces service JID: {error}"))
}

fn space_jid_for_node(spaces_jid: &BareJid, node: &str) -> Option<BareJid> {
    format!("{}@{}", node, spaces_jid.domain()).parse().ok()
}

pub(super) fn space_details_from_node(
    node: &waddle_xmpp::pubsub::PubSubNode,
) -> Option<SpaceDetails> {
    let access_model = waddle_xmpp::SpaceAccessModel::from_pubsub(node.config.access_model)?;
    let name = if node.node_name == "general" {
        "General".to_string()
    } else {
        node.node_name.clone()
    };
    Some(SpaceDetails {
        id: node.node_name.clone(),
        name,
        description: None,
        owner_id: node.owner.to_string(),
        icon_url: None,
        is_public: matches!(
            node.config.access_model,
            waddle_xmpp::pubsub::AccessModel::Open
        ),
        access_model,
        created_at: node.created_at.to_rfc3339(),
    })
}

fn channels_to_disco_items(channels: Vec<XmppChannelRecord>, muc_domain: &str) -> Vec<DiscoItem> {
    channels
        .into_iter()
        .filter(|channel| channel.public_room)
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
) -> Result<Vec<DiscoItem>, String> {
    match list_xmpp_channels(
        state.deps.app_state.db_pool.global_actor().clone(),
        limit,
        0,
    )
    .await
    {
        Ok(channels) => Ok(channels_to_disco_items(channels, muc_domain)),
        Err(error) => {
            warn!(error = %error, "Failed to list canonical channels for MUC discovery");
            Err(error)
        }
    }
}

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
    let Some(space_jid) = space_jid_for_node(spaces_jid, node) else {
        warn!(
            node,
            spaces = %spaces_jid,
            "Skipping channel-space link bookmark backfill for non-JID Space node"
        );
        return;
    };
    let links = match state
        .deps
        .app_state
        .channel_space_link_store
        .list_channels_in_space(&space_jid)
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
        let channel_type = if config.forum { "forum" } else { "text" };
        let item = match waddle_xmpp::xep::build_channel_item(
            &waddle_xmpp::ChannelInfo {
                id: channel_id.clone(),
                name: config.name,
                channel_type: channel_type.to_string(),
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
            let projected_space_jid = space_jid_for_node(&spaces_jid, node);
            if let Some(space_jid) = projected_space_jid.as_ref() {
                if let Err(error) = state
                    .deps
                    .app_state
                    .channel_space_link_store
                    .set(&crate::channel_space_links::ChannelSpaceLink {
                        channel_jid: bookmark.jid.clone(),
                        space_jid: space_jid.clone(),
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
    if is_story_attachment_node(node) {
        return handle_story_attachment_publish(iq, state, community_domain, node, item, session)
            .await;
    }
    if node != waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED
        && node != waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES
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

fn is_story_attachment_node(node: &str) -> bool {
    let prefix = format!("{}/", waddle_xmpp::xep::xep0470::PUBSUB_ATTACHMENTS_NODE_PREFIX);
    node.starts_with(&prefix)
        && (node.contains("node=urn%3Axmpp%3Astories%3A0")
            || node.contains("node=urn:xmpp:stories:0"))
}

fn story_attachment_target_item(node: &str) -> Option<String> {
    let target = node.split_once("?;")?.1;
    target.split(';').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if key != "item" {
            return None;
        }
        urlencoding::decode(value)
            .ok()
            .map(|decoded| decoded.into_owned())
            .filter(|item| !item.is_empty())
    })
}

fn validated_story_attachment_item(
    session: Option<&Session>,
    user_domain: &str,
    item: &PubSubItem,
) -> Result<BareJid, PubSubError> {
    let Some(session) = session else {
        return Err(PubSubError::Forbidden);
    };
    let publisher = format!(
        "{}@{}",
        session.xmpp_localpart.to_ascii_lowercase(),
        user_domain.to_ascii_lowercase()
    )
    .parse::<BareJid>()
    .map_err(|_| PubSubError::InvalidJid)?;
    if item.id.as_deref() != Some(publisher.as_str()) {
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
    attachment_node: &str,
) -> Result<(), PubSubError> {
    let storage = &state.deps.protocol.pubsub_storage;
    let Some(story_item_id) = story_attachment_target_item(attachment_node) else {
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
            &[story_item_id],
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
    if created || node.config.access_model != stories_node.config.access_model
        || node.config.publish_model != stories_node.config.publish_model
    {
        let mut config = node.config;
        config.access_model = stories_node.config.access_model;
        config.publish_model = stories_node.config.publish_model;
        storage
            .update_node_config(community_jid, attachment_node, &config)
            .await
            .map_err(|_| PubSubError::InternalServerError)?;
    }
    Ok(())
}

async fn handle_story_attachment_publish(
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
    if let Err(error) = ensure_story_attachment_node(state, &community_jid, node).await {
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

/// `true` when `item` is a well-formed RSVP for the publishing
/// session: item id of the form `<uid>-rsvp-<localpart>` where
/// `<localpart>` matches the session, payload is a single VEVENT
/// carrying exactly one `<attendee>` whose URI bare JID matches
/// `<localpart>@<user_domain>`, and the event contains no
/// master-event-only fields (no SUMMARY/DTSTART/RRULE).
fn is_well_formed_rsvp_item(
    session: Option<&Session>,
    user_domain: &str,
    item: &PubSubItem,
) -> bool {
    let Some(session) = session else {
        return false;
    };
    let Some(item_id) = &item.id else {
        return false;
    };
    let Some((master_uid, localpart)) = parse_rsvp_item_id(item_id) else {
        return false;
    };
    if !localpart.eq_ignore_ascii_case(&session.xmpp_localpart) {
        return false;
    }
    // Master UID must be non-empty; we don't constrain its shape
    // further (matches the master item's id).
    if master_uid.is_empty() {
        return false;
    }
    let Some(payload) = &item.payload else {
        return false;
    };
    if !waddle_xmpp_core::xcal::is_vcalendar_element(payload) {
        return false;
    }
    let ns_xcal = waddle_xmpp_core::xcal::NS_XCAL;
    let Some(vevent) = payload
        .children()
        .find(|c| c.name() == "vevent" && c.ns() == ns_xcal)
    else {
        return false;
    };
    // Master-event-only fields MUST NOT appear on an RSVP item.
    let forbidden = [
        "summary",
        "dtstart",
        "dtend",
        "rrule",
        "description",
        "location",
        "organizer",
    ];
    for child in vevent.children() {
        if child.ns() != ns_xcal {
            return false;
        }
        if forbidden.contains(&child.name()) {
            return false;
        }
    }
    let attendees: Vec<_> = vevent
        .children()
        .filter(|c| c.name() == "attendee" && c.ns() == ns_xcal)
        .collect();
    if attendees.len() != 1 {
        return false;
    }
    let uri = attendees[0].text();
    let uri_trimmed = uri.trim();
    let attendee_bare = waddle_xmpp_core::xcal::xmpp_uri_to_bare_jid(uri_trimmed);
    let expected_jid = format!(
        "{}@{}",
        localpart.to_ascii_lowercase(),
        user_domain.to_ascii_lowercase()
    );
    attendee_bare.as_deref() == Some(expected_jid.as_str())
}

/// Split a string like `evt-launch-rsvp-alice` into
/// `("evt-launch", "alice")`. Returns `None` for inputs without the
/// `-rsvp-` separator.
fn parse_rsvp_item_id(item_id: &str) -> Option<(&str, &str)> {
    let (master_uid, localpart) = item_id.rsplit_once("-rsvp-")?;
    if localpart.is_empty() {
        return None;
    }
    Some((master_uid, localpart))
}

/// Extract (author bare-JID, master event UID, partstat) from a
/// well-formed RSVP pubsub item. The item is already validated by
/// `is_well_formed_rsvp_item` at this point — we only return `Some`
/// when every field needed for the feed bridge is intact.
fn rsvp_bridge_context(
    item: &PubSubItem,
) -> Option<(BareJid, String, waddle_xmpp_core::xcal::PartStat)> {
    let item_id = item.id.as_deref()?;
    let (master_uid, _localpart) = parse_rsvp_item_id(item_id)?;
    let payload = item.payload.as_ref()?;
    if !waddle_xmpp_core::xcal::is_vcalendar_element(payload) {
        return None;
    }
    let ns_xcal = waddle_xmpp_core::xcal::NS_XCAL;
    let vevent = payload
        .children()
        .find(|c| c.name() == "vevent" && c.ns() == ns_xcal)?;
    let attendee = vevent
        .children()
        .find(|c| c.name() == "attendee" && c.ns() == ns_xcal)?;
    let partstat = attendee
        .attr("partstat")
        .and_then(waddle_xmpp_core::xcal::PartStat::from_str_value)?;
    let uri = attendee.text();
    let bare = waddle_xmpp_core::xcal::xmpp_uri_to_bare_jid(uri.trim())?;
    let author_jid = bare.parse::<BareJid>().ok()?;
    Some((author_jid, master_uid.to_string(), partstat))
}

async fn handle_community_rsvp_publish(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    item: PubSubItem,
) -> Vec<String> {
    let bridge_context = rsvp_bridge_context(&item);
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
            warn!(node, error = %error, "Failed to resolve community node for RSVP publish");
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
            // Bridge into the social feed so "X is going to <event>"
            // surfaces alongside manual posts. Best-effort: failures
            // are logged inside `observe_rsvp` and never block the
            // RSVP publish itself.
            if let Some((author_jid, master_uid, partstat)) = bridge_context {
                let _ = state
                    .deps
                    .protocol
                    .pep_feed_bridge
                    .observe_rsvp(
                        &state.deps.protocol.pubsub_storage,
                        &community_jid,
                        &author_jid,
                        &master_uid,
                        partstat,
                    )
                    .await;
            }
            vec![iq_to_xml(build_pubsub_publish_result(
                iq,
                node,
                &result.item_id,
            ))]
        }
        Err(error) => {
            warn!(node, error = %error, "Failed to publish community RSVP item");
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))]
        }
    }
}

pub(super) async fn handle_community_items(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    max_items: Option<u32>,
    item_ids: &[String],
) -> Vec<String> {
    if !is_story_attachment_node(node)
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

pub(super) async fn handle_community_retract(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    community_domain: &str,
    node: &str,
    item_id: &str,
    session: Option<&Session>,
) -> Vec<String> {
    if is_story_attachment_node(node) {
        return handle_story_attachment_retract(iq, state, community_domain, node, item_id, session)
            .await;
    }
    if node != waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED
        && node != waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES
        && node != waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS
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

async fn handle_story_attachment_retract(
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
    let publisher = format!(
        "{}@{}",
        session.xmpp_localpart.to_ascii_lowercase(),
        user_domain.to_ascii_lowercase()
    );
    if item_id != publisher {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::BadRequest))];
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
            warn!(node, item_id, error = %error, "Failed to retract story attachment item");
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

async fn cleanup_stale_space_bookmarks(
    state: &WebSocketState,
    spaces_jid: &BareJid,
    channel_id: &str,
    keep_node: &str,
    item_id: &str,
    stale_nodes: &[String],
) -> Result<(), String> {
    let mut removed_stale: Vec<RemovedStaleSpaceBookmark> = Vec::new();
    for stale in stale_nodes.iter().filter(|name| name.as_str() != keep_node) {
        let item_filter = [item_id.to_string()];
        let previous_item = match state
            .deps
            .protocol
            .pubsub_storage
            .get_items(spaces_jid, stale, Some(1), &item_filter)
            .await
        {
            Ok(items) => items
                .into_iter()
                .next()
                .map(|stored| stored.to_pubsub_item()),
            Err(error) => {
                restore_stale_space_bookmarks(state, spaces_jid, channel_id, &removed_stale).await;
                return Err(format!(
                    "pubsub read stale channel bookmark from {stale} failed: {error}"
                ));
            }
        };
        let parent_tuple_deleted = match delete_channel_parent_tuple(state, channel_id, stale).await
        {
            Ok(deleted) => deleted,
            Err(error) => {
                restore_stale_space_bookmarks(state, spaces_jid, channel_id, &removed_stale).await;
                return Err(error);
            }
        };
        match state
            .deps
            .protocol
            .pubsub_storage
            .retract_item(spaces_jid, stale, item_id)
            .await
        {
            Ok(_) => {
                removed_stale.push(RemovedStaleSpaceBookmark {
                    node: stale.clone(),
                    item: previous_item,
                    parent_tuple_deleted,
                });
            }
            Err(error) => {
                if parent_tuple_deleted {
                    let _ = write_channel_parent_tuple(state, channel_id, stale).await;
                }
                restore_stale_space_bookmarks(state, spaces_jid, channel_id, &removed_stale).await;
                return Err(format!(
                    "pubsub retract stale channel bookmark from {stale} failed: {error}"
                ));
            }
        }
    }
    Ok(())
}

struct RemovedStaleSpaceBookmark {
    node: String,
    item: Option<PubSubItem>,
    parent_tuple_deleted: bool,
}

async fn restore_stale_space_bookmarks(
    state: &WebSocketState,
    spaces_jid: &BareJid,
    channel_id: &str,
    removed_stale: &[RemovedStaleSpaceBookmark],
) {
    for removed in removed_stale.iter().rev() {
        if let Some(item) = removed.item.as_ref() {
            match state
                .deps
                .protocol
                .pubsub_storage
                .publish_item(spaces_jid, &removed.node, item, None, false)
                .await
            {
                Ok(_) => {
                    if removed.parent_tuple_deleted {
                        let _ = write_channel_parent_tuple(state, channel_id, &removed.node).await;
                    }
                }
                Err(error) => {
                    warn!(
                        channel_id = %channel_id,
                        node = %removed.node,
                        error = %error,
                        "Failed to restore stale Space bookmark after cleanup rollback"
                    );
                }
            }
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
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    let target_space_jid = space_jid_for_node(&spaces_jid, node);
    let item_filter = [item_id.to_string()];
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces_jid, node, Some(1), &item_filter)
        .await
    {
        Ok(items) if items.is_empty() => {
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::ItemNotFound))];
        }
        Ok(_) => {}
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to preflight Spaces item before retract");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    }
    let link_to_restore = match state
        .deps
        .app_state
        .channel_space_link_store
        .get(&room_jid)
        .await
    {
        Ok(Some(link)) if target_space_jid.as_ref() == Some(&link.space_jid) => Some(link),
        Ok(_) => None,
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to read channel-space link before Spaces retract");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    };
    let parent_tuple_deleted = match delete_channel_parent_tuple(state, &channel_id, node).await {
        Ok(deleted) => deleted,
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to clear channel parent tuple before Spaces retract");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    };
    if link_to_restore.is_some() {
        if let Err(error) = state
            .deps
            .app_state
            .channel_space_link_store
            .clear(&room_jid)
            .await
        {
            if parent_tuple_deleted {
                let _ = write_channel_parent_tuple(state, &channel_id, node).await;
            }
            warn!(item_id, node, error = %error, "Failed to clear channel-space link before Spaces retract");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .retract_item(&spaces_jid, node, item_id)
        .await
    {
        Ok(true) => vec![iq_to_xml(build_pubsub_success(iq))],
        Ok(false) => {
            if parent_tuple_deleted {
                if let Err(restore_error) =
                    write_channel_parent_tuple(state, &channel_id, node).await
                {
                    warn!(
                        item_id,
                        node,
                        error = %restore_error,
                        "Failed to restore channel parent tuple after Spaces retract returned item-not-found"
                    );
                }
            }
            if let Some(link) = link_to_restore.as_ref() {
                if let Err(restore_error) = state
                    .deps
                    .app_state
                    .channel_space_link_store
                    .set(link)
                    .await
                {
                    warn!(
                        item_id,
                        node,
                        error = %restore_error,
                        "Failed to restore channel-space link after Spaces retract returned item-not-found"
                    );
                }
            }
            vec![iq_to_xml(build_pubsub_error(iq, PubSubError::ItemNotFound))]
        }
        Err(error) => {
            if parent_tuple_deleted {
                if let Err(restore_error) =
                    write_channel_parent_tuple(state, &channel_id, node).await
                {
                    warn!(
                        item_id,
                        node,
                        error = %restore_error,
                        "Failed to restore channel parent tuple after Spaces retract failure"
                    );
                }
            }
            if let Some(link) = link_to_restore.as_ref() {
                if let Err(restore_error) = state
                    .deps
                    .app_state
                    .channel_space_link_store
                    .set(link)
                    .await
                {
                    warn!(
                        item_id,
                        node,
                        error = %restore_error,
                        "Failed to restore channel-space link after Spaces retract failure"
                    );
                }
            }
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
