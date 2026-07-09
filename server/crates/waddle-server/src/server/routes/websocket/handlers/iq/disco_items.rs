use super::*;
use crate::server::disco_targets::calls_available;

enum RoomCommandTarget {
    GroupDm,
    NotGroupDm,
    NotMember,
    Missing,
    Failed,
}

fn full_muc_room_target(target_to: Option<&str>, muc_domain: &str) -> bool {
    target_to
        .and_then(|target| target.split_once('/').map(|(bare, _)| bare))
        .and_then(|bare| bare.parse::<BareJid>().ok())
        .is_some_and(|room_jid| room_jid.domain().as_str() == muc_domain)
}

async fn classify_room_command_target(
    state: &WebSocketState,
    room_jid: &BareJid,
    requester: Option<&FullJid>,
) -> RoomCommandTarget {
    let Some(requester_bare) = requester.map(|jid| jid.to_bare()) else {
        return RoomCommandTarget::NotMember;
    };
    if let Some(room_actor) = get_room_actor(state, room_jid).await {
        let snapshot = match room_actor.ask(GetSnapshot).await {
            Ok(snapshot) => snapshot.room,
            Err(_) => {
                warn!(
                    operation = "room_snapshot",
                    "Failed to load room snapshot for disco#items"
                );
                return RoomCommandTarget::Failed;
            }
        };
        return if snapshot.config.group_dm {
            if snapshot.get_affiliation(&requester_bare) >= Affiliation::Member {
                RoomCommandTarget::GroupDm
            } else {
                let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
                    return RoomCommandTarget::NotMember;
                };
                if requester_has_durable_group_dm_membership(state, &requester_bare, &channel_id)
                    .await
                {
                    RoomCommandTarget::GroupDm
                } else {
                    RoomCommandTarget::NotMember
                }
            }
        } else {
            RoomCommandTarget::NotGroupDm
        };
    }

    match get_managed_channel_for_room(state, room_jid).await {
        Ok(Some(channel)) if channel.channel_type == waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM => {
            let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
                return RoomCommandTarget::Missing;
            };
            if requester_has_group_dm_membership_tuple(state, &requester_bare, &channel_id)
                .await
                .is_some_and(|allowed| allowed)
                || requester_has_durable_group_dm_membership(state, &requester_bare, &channel_id)
                    .await
            {
                return RoomCommandTarget::GroupDm;
            }
            match state
                .deps
                .app_state
                .permission_actor
                .ask(CheckPermission {
                    subject: Subject::user(requester_bare.to_string()),
                    permission: Permission::Member,
                    object: Object::new(ObjectType::Channel, channel_id),
                })
                .await
            {
                Err(kameo::error::SendError::HandlerError(_)) => {
                    warn!(
                        operation = "membership_check",
                        "Failed to check persisted group-DM membership for disco#items"
                    );
                    RoomCommandTarget::Failed
                }
                Ok(response) if response.allowed => RoomCommandTarget::GroupDm,
                Ok(_) => RoomCommandTarget::NotMember,
                Err(_) => {
                    warn!(
                        operation = "permission_check",
                        "Failed to ask permission actor for disco#items"
                    );
                    RoomCommandTarget::Failed
                }
            }
        }
        Ok(Some(_)) => RoomCommandTarget::NotGroupDm,
        Ok(None) => RoomCommandTarget::Missing,
        Err(_) => {
            warn!(
                operation = "room_lookup",
                "Failed to load persisted room for disco#items"
            );
            RoomCommandTarget::Failed
        }
    }
}

async fn requester_has_group_dm_membership_tuple(
    state: &WebSocketState,
    requester_bare: &BareJid,
    channel_id: &str,
) -> Option<bool> {
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(requester_bare.to_string()),
            permission: Permission::Member,
            object: Object::new(ObjectType::Channel, channel_id),
        })
        .await
        .ok()
        .map(|response| response.allowed)
}

async fn requester_has_durable_group_dm_membership(
    state: &WebSocketState,
    requester_bare: &BareJid,
    channel_id: &str,
) -> bool {
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbQueryOne {
            sql: r#"
                SELECT 1 FROM permission_tuples
                WHERE object_type = 'channel'
                  AND object_id = ?
                  AND relation = 'member'
                  AND subject_type = 'user'
                  AND subject_id = ?
                  AND subject_relation IS NULL
                LIMIT 1
            "#
            .to_string(),
            params: vec![channel_id.into(), requester_bare.to_string().into()],
        })
        .await
        .is_ok_and(|row| row.is_some())
}

pub(super) async fn handle_disco_items_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
) -> Vec<String> {
    let iq = ctx.iq;
    let id = ctx.id;
    let payload_ns = ctx.payload_ns;
    let target_to = ctx.target_to;
    let domain = ctx.domain;
    let muc_domain = ctx.muc_domain;
    let upload_domain = ctx.upload_domain;
    let spaces_domain = ctx.spaces_domain;
    let community_domain = ctx.community_domain;
    let extensions_domain = ctx.extensions_domain;
    let push_domain = ctx.push_domain;
    let response_from = ctx.response_from;
    let response_to = ctx.response_to;

    // Disco items - list services/rooms
    if payload_ns == "http://jabber.org/protocol/disco#items" {
        let request_iq = &iq;
        let query = match parse_disco_items_query(request_iq) {
            Ok(query) => query,
            Err(_) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    bad_request_iq_error("Malformed IQ payload."),
                )];
            }
        };

        if target_to == Some(muc_domain) {
            debug!("Disco items query on MUC service");
            let items = match canonical_channel_disco_items(state, muc_domain, 500).await {
                Ok(items) => items,
                Err(_) => {
                    return vec![build_iq_error_xml_typed(
                        id,
                        None,
                        None,
                        internal_server_error_iq_error("Failed to list MUC rooms."),
                    )];
                }
            };

            let response = build_disco_items_response(request_iq, &items, None);
            return vec![iq_to_xml(response)];
        }

        if query.node.as_deref() == Some(NODE_COMMANDS) {
            if full_muc_room_target(target_to, muc_domain) {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )];
            }
            if let Some(room_jid) = target_to
                .filter(|target| !target.contains('/'))
                .and_then(|target| target.parse::<BareJid>().ok())
                .filter(|room_jid| room_jid.domain().as_str() == muc_domain)
            {
                match classify_room_command_target(state, &room_jid, phase.bound_jid()).await {
                    RoomCommandTarget::GroupDm => {
                        let commands = state.deps.protocol.command_registry.list_commands().await;
                        let command_refs =
                            command_refs_by_boundary(&commands, CommandBoundary::MucRoom);
                        let response =
                            build_command_items(request_iq, &command_refs, target_to.unwrap());
                        return vec![iq_to_xml(response)];
                    }
                    RoomCommandTarget::NotGroupDm | RoomCommandTarget::NotMember => {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            item_not_found_iq_error("Requested item not found."),
                        )];
                    }
                    RoomCommandTarget::Missing => {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            item_not_found_iq_error("Requested room not found."),
                        )];
                    }
                    RoomCommandTarget::Failed => {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            internal_server_error_iq_error("Internal server error."),
                        )];
                    }
                }
            }
        }

        if target_to == Some(domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let commands = state.deps.protocol.command_registry.list_commands().await;
            let command_refs = command_refs_by_boundary(&commands, CommandBoundary::Server);
            let response = build_command_items(request_iq, &command_refs, domain);
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(extensions_domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let commands = state.deps.protocol.command_registry.list_commands().await;
            let command_refs = command_refs_by_boundary(&commands, CommandBoundary::Extensions);
            let response = build_command_items(request_iq, &command_refs, extensions_domain);
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(push_domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let commands = state.deps.protocol.command_registry.list_commands().await;
            let command_refs = command_refs_by_boundary(&commands, CommandBoundary::PushService);
            let response = build_command_items(request_iq, &command_refs, push_domain);
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(extensions_domain) {
            if let Some(node) = query.node.as_deref() {
                let known_route_node = state
                    .deps
                    .protocol
                    .extension_manager
                    .route_descriptors()
                    .iter()
                    .any(|route| extension_route_disco_node(route) == node);
                if !known_route_node {
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        item_not_found_iq_error("Requested item not found."),
                    )];
                }
                let response = build_disco_items_response(request_iq, &[], Some(node));
                return vec![iq_to_xml(response)];
            }
            let items = state
                .deps
                .protocol
                .extension_manager
                .route_descriptors()
                .iter()
                .map(|route| {
                    let node = extension_route_disco_node(route);
                    DiscoItem::new(
                        extensions_domain,
                        Some(route.label.as_str()),
                        Some(node.as_str()),
                    )
                })
                .collect::<Vec<_>>();
            let response = build_disco_items_response(request_iq, &items, None);
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(community_domain) {
            // The community pubsub service hosts exactly two
            // well-known nodes: the XEP-0472 feed and the XEP-0501
            // stories node. Surface both so clients can enumerate
            // before subscribing.
            let items: Vec<DiscoItem> = match query.node.as_deref() {
                Some(_) => vec![],
                None => vec![
                    DiscoItem::new(
                        community_domain,
                        Some("Community Feed"),
                        Some(waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED),
                    ),
                    DiscoItem::new(
                        community_domain,
                        Some("Community Stories"),
                        Some(waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES),
                    ),
                    DiscoItem::new(
                        community_domain,
                        Some("Community Events"),
                        Some(waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS),
                    ),
                ],
            };
            let response = build_disco_items_response(request_iq, &items, query.node.as_deref());
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(spaces_domain) {
            let items: Vec<DiscoItem> = match query.node.as_deref() {
                Some(_) => vec![],
                None => match spaces_service_bare_jid(spaces_domain) {
                    Ok(spaces_jid) => match state
                        .deps
                        .protocol
                        .pubsub_storage
                        .list_nodes(&spaces_jid)
                        .await
                    {
                        Ok(nodes) => {
                            let requester = phase.bound_jid().map(|jid| jid.to_bare());
                            let mut items = Vec::new();
                            for node in nodes {
                                let Ok(Some(space_node)) = state
                                    .deps
                                    .protocol
                                    .pubsub_storage
                                    .get_node(&spaces_jid, &node)
                                    .await
                                else {
                                    continue;
                                };
                                let Some(space) = space_details_from_node(&space_node) else {
                                    continue;
                                };
                                let visible = match requester.as_ref() {
                                    Some(requester) => crate::pubsub_authz::can_subscribe(
                                        &state.deps.protocol.pubsub_storage,
                                        &spaces_jid,
                                        &node,
                                        requester,
                                        false,
                                    )
                                    .await
                                    .unwrap_or(false),
                                    None => matches!(
                                        space_node.config.access_model,
                                        waddle_xmpp::pubsub::AccessModel::Open
                                    ),
                                };
                                if visible {
                                    items.push(DiscoItem::spaces_node(
                                        spaces_domain,
                                        &node,
                                        Some(&space.name),
                                    ));
                                }
                            }
                            items
                        }
                        Err(_) => {
                            warn!(operation = "spaces_list", "Failed to list Spaces nodes");
                            vec![]
                        }
                    },
                    Err(_) => {
                        warn!(
                            operation = "spaces_target_parse",
                            "Invalid Spaces service JID"
                        );
                        vec![]
                    }
                },
            };

            let response = build_disco_items_response(request_iq, &items, query.node.as_deref());
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(push_domain) {
            let items: Vec<DiscoItem> = match query.node.as_deref() {
                Some(_) => Vec::new(),
                None => {
                    let Some(owner_bare_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
                        let response =
                            build_disco_items_response(request_iq, &[], query.node.as_deref());
                        return vec![iq_to_xml(response)];
                    };
                    match state
                        .deps
                        .protocol
                        .push_service
                        .list_node_names_for_owner(&owner_bare_jid)
                        .await
                    {
                        Ok(nodes) => nodes
                            .into_iter()
                            .map(|node| DiscoItem::pubsub_node(push_domain, &node))
                            .collect(),
                        Err(_) => {
                            warn!(
                                operation = "push_node_list",
                                "Failed to list owner Push Service nodes"
                            );
                            Vec::new()
                        }
                    }
                }
            };

            let response = build_disco_items_response(request_iq, &items, query.node.as_deref());
            return vec![iq_to_xml(response)];
        }

        if let Some(target) = target_to {
            if query.node.is_none() {
                if let Ok(target_bare) = target.parse::<BareJid>() {
                    if target_bare.node().is_some() {
                        let items = match state
                            .deps
                            .protocol
                            .pubsub_storage
                            .list_nodes(&target_bare)
                            .await
                        {
                            Ok(nodes) => nodes
                                .into_iter()
                                .filter(|node| {
                                    node == PEP_NODE_AVATAR_DATA || node == PEP_NODE_AVATAR_METADATA
                                })
                                .map(|node| DiscoItem::pubsub_node(&target_bare.to_string(), &node))
                                .collect::<Vec<_>>(),
                            Err(_) => {
                                warn!(operation = "pep_node_list", "Failed to list PEP nodes");
                                vec![]
                            }
                        };
                        let response = build_disco_items_response(request_iq, &items, None);
                        return vec![iq_to_xml(response)];
                    }
                }
            }
        }

        debug!("Disco items query on server");
        let calls_mixer_domain = format!("calls.{domain}");
        let mut items = vec![
            DiscoItem::muc_service(muc_domain, Some("Chatrooms")),
            DiscoItem::upload_service(upload_domain, Some("HTTP File Upload")),
            DiscoItem::spaces_service(spaces_domain, Some("Spaces")),
            DiscoItem::community_service(community_domain, Some("Community")),
            DiscoItem::pubsub_service(extensions_domain, Some("Extensions")),
            DiscoItem::pubsub_service(push_domain, Some("Push Service")),
        ];
        if calls_available(state) {
            items.push(DiscoItem::calls_mixer(
                &calls_mixer_domain,
                Some("Group Call Mixer"),
            ));
        }
        let response = build_disco_items_response(request_iq, &items, None);
        return vec![iq_to_xml(response)];
    }
    Vec::new()
}

fn channels_to_disco_items(channels: Vec<XmppChannelRecord>, muc_domain: &str) -> Vec<DiscoItem> {
    channels
        .into_iter()
        .filter(|channel| channel.channel_type != waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM)
        .filter(|channel| channel.public_room)
        .filter_map(|channel| {
            waddle_xmpp::managed_room_jid(&channel.id, muc_domain)
                .ok()
                .map(|room_jid| DiscoItem::muc_room(&room_jid.to_string(), &channel.name))
        })
        .collect()
}

async fn canonical_channel_disco_items(
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
            warn!(
                operation = "channel_list",
                "Failed to list canonical channels for MUC discovery"
            );
            Err(error)
        }
    }
}
