use super::*;

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
            let items = canonical_channel_disco_items(state, muc_domain, 500).await;

            let response = build_disco_items_response(request_iq, &items, None);
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let commands = state.deps.protocol.command_registry.list_commands().await;
            let command_refs = command_refs_by_boundary(&commands, false);
            let response = build_command_items(request_iq, &command_refs, domain);
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(extensions_domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let commands = state.deps.protocol.command_registry.list_commands().await;
            let command_refs = command_refs_by_boundary(&commands, true);
            let response = build_command_items(request_iq, &command_refs, extensions_domain);
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
                        Ok(nodes) => nodes
                            .into_iter()
                            .map(|node| {
                                let name = if node == "general" { "General" } else { &node };
                                DiscoItem::spaces_node(spaces_domain, &node, Some(name))
                            })
                            .collect(),
                        Err(error) => {
                            warn!(error = %error, "Failed to list Spaces nodes");
                            vec![]
                        }
                    },
                    Err(error) => {
                        warn!(error = %error, "Invalid Spaces service JID");
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
                        Err(error) => {
                            warn!(error = %error, owner = %owner_bare_jid, "Failed to list owner Push Service nodes");
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
                            Err(error) => {
                                warn!(target = %target_bare, error = %error, "Failed to list PEP nodes");
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
        let items = vec![
            DiscoItem::muc_service(muc_domain, Some("Chatrooms")),
            DiscoItem::upload_service(upload_domain, Some("HTTP File Upload")),
            DiscoItem::spaces_service(spaces_domain, Some("Spaces")),
            DiscoItem::community_service(community_domain, Some("Community")),
            DiscoItem::pubsub_service(extensions_domain, Some("Extensions")),
            DiscoItem::pubsub_service(push_domain, Some("Push Service")),
        ];
        let response = build_disco_items_response(request_iq, &items, None);
        return vec![iq_to_xml(response)];
    }
    Vec::new()
}
