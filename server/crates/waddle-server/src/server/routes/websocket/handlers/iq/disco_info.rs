use super::*;

pub(super) async fn handle_disco_info_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    authenticated_session: &Option<Session>,
) -> Vec<String> {
    let iq = ctx.iq;
    let id = ctx.id;
    let payload_ns = ctx.payload_ns;
    let target_to = ctx.target_to;
    let domain = ctx.domain;
    let muc_domain = ctx.muc_domain;
    let upload_domain = ctx.upload_domain;
    let spaces_domain = ctx.spaces_domain;
    let extensions_domain = ctx.extensions_domain;
    let response_from = ctx.response_from;
    let response_to = ctx.response_to;

    // Disco info on MUC service
    if payload_ns == "http://jabber.org/protocol/disco#info" {
        let request_iq = &iq;
        let query = match parse_disco_info_query(request_iq) {
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
            let identities = vec![Identity::muc_service(Some("Waddle Chatrooms"))];
            let mut features = vec![
                Feature::muc(),
                Feature::replies(),
                Feature::new(NS_CHANNEL_SEARCH),
            ];
            features.extend(extension_features_for_disco(state));
            let response = build_disco_info_response(request_iq, &identities, &features, None);
            return vec![iq_to_xml(response)];
        }

        // Disco info on a specific room
        if let Some(target) = target_to {
            let room_target = target.split('/').next().unwrap_or(target);
            if let Ok(room_jid) = room_target.parse::<BareJid>() {
                if let Some(room_actor) = get_room_actor(state, &room_jid).await {
                    let snapshot = match room_actor.ask(GetSnapshot).await {
                        Ok(snapshot) => snapshot.room,
                        Err(error) => {
                            warn!(
                                room = %room_jid,
                                error = ?error,
                                "Failed to load room snapshot for disco#info"
                            );
                            return vec![build_iq_error_xml_typed(
                                id,
                                response_from,
                                response_to,
                                internal_server_error_iq_error("Internal server error."),
                            )];
                        }
                    };
                    let managed_channel = get_managed_channel_for_room(state, &room_jid)
                        .await
                        .ok()
                        .flatten();
                    let channel_type = managed_channel
                        .as_ref()
                        .map(|channel| channel.channel_type.as_str())
                        .unwrap_or(if snapshot.config.forum {
                            "forum"
                        } else {
                            "text"
                        });
                    let description = managed_channel
                        .as_ref()
                        .and_then(|channel| channel.description.as_deref())
                        .or(snapshot.config.description.as_deref());
                    let identities = vec![Identity::muc_room(Some(&snapshot.config.name))];
                    let mut features = muc_room_features(
                        snapshot.config.persistent,
                        snapshot.config.members_only,
                        snapshot.config.moderated || channel_type == "announcement",
                        snapshot.config.forum || channel_type == "forum",
                    );
                    features.extend(extension_features_for_disco(state));
                    let mut extensions =
                        room_space_metadata_extensions(state, &room_jid, description).await;
                    let has_space_metadata = !extensions.is_empty();
                    if has_space_metadata {
                        features.push(Feature::spaces());
                    }
                    extensions.push(build_room_metadata_form(
                        channel_type,
                        snapshot.config.pin_permission.as_form_value(),
                    ));
                    let response = build_disco_info_response_with_extensions(
                        request_iq,
                        &identities,
                        &features,
                        None,
                        &extensions,
                    );
                    return vec![iq_to_xml(response)];
                }

                if is_muc_room_jid(state, &room_jid).await {
                    if let Ok(Some(channel)) = get_managed_channel_for_room(state, &room_jid).await
                    {
                        let identities = vec![Identity::muc_room(Some(&channel.name))];
                        let mut features = muc_room_features(
                            true,
                            true,
                            channel.channel_type == "announcement",
                            channel.channel_type == "forum",
                        );
                        features.extend(extension_features_for_disco(state));
                        let mut extensions = room_space_metadata_extensions(
                            state,
                            &room_jid,
                            channel.description.as_deref(),
                        )
                        .await;
                        let has_space_metadata = !extensions.is_empty();
                        if has_space_metadata {
                            features.push(Feature::spaces());
                        }
                        // #422: read the persisted pin policy from
                        // the channel record so dormant rooms
                        // advertise the truth — not the default.
                        extensions.push(build_room_metadata_form(
                            &channel.channel_type,
                            channel.pin_permission.as_form_value(),
                        ));
                        let response = build_disco_info_response_with_extensions(
                            request_iq,
                            &identities,
                            &features,
                            None,
                            &extensions,
                        );
                        return vec![iq_to_xml(response)];
                    }

                    let room_name = room_jid
                        .node()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "Room".to_string());
                    let identities = vec![Identity::muc_room(Some(&room_name))];
                    let mut features = muc_room_features(false, false, false, false);
                    features.extend(extension_features_for_disco(state));
                    let response =
                        build_disco_info_response(request_iq, &identities, &features, None);
                    return vec![iq_to_xml(response)];
                }
            }
        }

        if target_to == Some(domain) && query.node.as_deref() == Some(NODE_COMMANDS) {
            let identities = vec![Identity::command_list(Some("Ad-Hoc Commands"))];
            let features = vec![
                Feature::disco_info(),
                Feature::disco_items(),
                Feature::commands(),
            ];
            let response =
                build_disco_info_response(request_iq, &identities, &features, Some(NODE_COMMANDS));
            return vec![iq_to_xml(response)];
        }

        if target_to == Some(domain) {
            if let Some(node) = query.node.as_deref() {
                let commands = state.deps.protocol.command_registry.list_commands().await;
                if let Some(name) = command_name_by_boundary(&commands, node, false) {
                    let identities = vec![Identity::automation(Some(name))];
                    let features = vec![
                        Feature::disco_info(),
                        Feature::commands(),
                        Feature::new(DATA_FORMS_NS),
                    ];
                    let response =
                        build_disco_info_response(request_iq, &identities, &features, Some(node));
                    return vec![iq_to_xml(response)];
                }
            }
        }

        if target_to == Some(extensions_domain) {
            if query.node.as_deref() == Some(NODE_COMMANDS) {
                let identities = vec![Identity::command_list(Some("Extension Commands"))];
                let features = vec![
                    Feature::disco_info(),
                    Feature::disco_items(),
                    Feature::commands(),
                ];
                let response = build_disco_info_response(
                    request_iq,
                    &identities,
                    &features,
                    Some(NODE_COMMANDS),
                );
                return vec![iq_to_xml(response)];
            }

            if let Some(node) = query.node.as_deref() {
                let commands = state.deps.protocol.command_registry.list_commands().await;
                if command_name_by_boundary(&commands, node, true).is_some() {
                    let Some((plugin, descriptor)) = state
                        .deps
                        .protocol
                        .extension_manager
                        .command_descriptors()
                        .into_iter()
                        .find(|(_, descriptor)| descriptor.node.as_str() == node)
                    else {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            item_not_found_iq_error("Requested item not found."),
                        )];
                    };
                    let identities = vec![Identity::automation(Some(descriptor.name.as_str()))];
                    let features = vec![
                        Feature::disco_info(),
                        Feature::commands(),
                        Feature::new(DATA_FORMS_NS),
                        Feature::new(EXTENSION_COMMAND_FORM_TYPE),
                    ];
                    let form = extension_command_metadata_form(&plugin, &descriptor);
                    let response = build_disco_info_response_with_extensions(
                        request_iq,
                        &identities,
                        &features,
                        Some(node),
                        &[form],
                    );
                    return vec![iq_to_xml(response)];
                }

                let Some(route) = state
                    .deps
                    .protocol
                    .extension_manager
                    .route_descriptors()
                    .iter()
                    .find(|route| extension_route_disco_node(route) == node)
                else {
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        item_not_found_iq_error("Requested item not found."),
                    )];
                };
                let identities = vec![Identity::new(
                    "waddle",
                    "extension-route",
                    Some(route.label.as_str()),
                )];
                let features = vec![
                    Feature::disco_info(),
                    Feature::new("urn:waddle:extension:1"),
                    Feature::new(EXTENSION_ROUTE_FORM_TYPE),
                    Feature::new(route.payload_namespace.as_str()),
                ];
                let form = extension_route_metadata_form(route);
                let response = build_disco_info_response_with_extensions(
                    request_iq,
                    &identities,
                    &features,
                    Some(node),
                    &[form],
                );
                return vec![iq_to_xml(response)];
            }

            let identities = vec![Identity::pubsub_service(Some("Waddle Extensions"))];
            let mut features = vec![
                Feature::disco_info(),
                Feature::disco_items(),
                Feature::commands(),
                Feature::pubsub(),
                Feature::pubsub_retrieve_items(),
                Feature::new("urn:waddle:extension:1"),
            ];
            features.extend(extension_features_for_disco(state));
            let response = build_disco_info_response(request_iq, &identities, &features, None);
            return vec![iq_to_xml(response)];
        }

        // Disco info on spaces service
        if target_to == Some(spaces_domain) {
            if let Some(node) = query.node.as_deref() {
                let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
                    return vec![build_iq_error_xml_typed(
                        id,
                        None,
                        None,
                        internal_server_error_iq_error("Internal server error."),
                    )];
                };
                let space_node = match state
                    .deps
                    .protocol
                    .pubsub_storage
                    .get_node(&spaces_jid, node)
                    .await
                {
                    Ok(Some(node)) => node,
                    Ok(None) => {
                        return vec![build_iq_error_xml_typed(
                            id,
                            None,
                            None,
                            item_not_found_iq_error("Requested item not found."),
                        )];
                    }
                    Err(error) => {
                        warn!(node, error = %error, "Failed to resolve Spaces node info");
                        return vec![build_iq_error_xml_typed(
                            id,
                            None,
                            None,
                            item_not_found_iq_error("Requested item not found."),
                        )];
                    }
                };

                let space = space_details_from_node(&space_node);
                let requester_affiliation =
                    space_affiliation_for_requester(state, authenticated_session.as_ref(), node)
                        .await;
                let identities = vec![Identity::pubsub_leaf(Some(&space.name))];
                let features = vec![
                    Feature::disco_info(),
                    Feature::pubsub(),
                    Feature::pubsub_retrieve_items(),
                    Feature::spaces(),
                ];
                let metadata =
                    build_spaces_metadata_form_for_requester(&space, requester_affiliation);
                let response = build_disco_info_response_with_extensions(
                    request_iq,
                    &identities,
                    &features,
                    Some(node),
                    &[metadata],
                );
                return vec![iq_to_xml(response)];
            }

            let identities = vec![Identity::spaces_service(Some("Spaces"))];
            let features = spaces_service_features();
            let response = build_disco_info_response(request_iq, &identities, &features, None);
            return vec![iq_to_xml(response)];
        }

        // Disco info on upload service (XEP-0363)
        if target_to == Some(upload_domain) {
            let identities = vec![Identity::upload_service(Some("HTTP File Upload"))];
            let features = upload_service_features();
            let response = build_disco_info_response(request_iq, &identities, &features, None);
            return vec![iq_to_xml(response)];
        }

        if let (Some(target), Some(bound_jid)) = (target_to, phase.bound_jid()) {
            if let Ok(target_bare) = target.parse::<BareJid>() {
                if target_bare == bound_jid.to_bare() {
                    let identities = vec![
                        Identity::server(Some("Personal Archive")),
                        build_pep_identity(),
                    ];
                    let mut features = vec![
                        Feature::disco_info(),
                        Feature::mam(),
                        Feature::mam_extended(),
                        Feature::fulltext_mam(),
                    ];
                    features.extend(pep_features());
                    let response =
                        build_disco_info_response(request_iq, &identities, &features, None);
                    return vec![iq_to_xml(response)];
                }
                if target_bare.domain().as_str() == domain && target_bare.node().is_some() {
                    let Some(localpart) = target_bare.node() else {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            item_not_found_iq_error("Requested item not found."),
                        )];
                    };
                    match local_xmpp_account_exists(state, localpart.as_str(), domain).await {
                        Ok(true) => {
                            let identities = vec![build_pep_identity()];
                            let mut features = vec![Feature::disco_info()];
                            features.extend(pep_features().into_iter().filter(|feature| {
                                !matches!(
                                    feature.0.as_str(),
                                    "urn:xmpp:mam:2" | "urn:xmpp:mam:2#extended"
                                )
                            }));
                            let response =
                                build_disco_info_response(request_iq, &identities, &features, None);
                            return vec![iq_to_xml(response)];
                        }
                        Ok(false) => {
                            return vec![build_iq_error_xml_typed(
                                id,
                                response_from,
                                response_to,
                                item_not_found_iq_error("Requested item not found."),
                            )];
                        }
                        Err(error) => {
                            warn!(target = %target_bare, error = %error, "Failed to resolve PEP disco target");
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
        }

        // Disco info on server. Source the canonical feature catalogue
        // from `waddle-xmpp-core::disco::info::server_features()` so the
        // rich-message XEPs (corrections, retractions, reactions,
        // references, stanza-ids, etc.) declared there stay discoverable
        // here without drift between the two lists. Server-instance
        // additions (Spaces, jabber:iq:search, ISR) are appended below,
        // and dynamic extension namespaces extend further still.
        let identities = vec![Identity::server(Some("Waddle"))];
        let mut features = waddle_xmpp::disco::info::server_features();
        features.extend([Feature::new("jabber:iq:search"), Feature::new(ISR_NS)]);
        features.extend(extension_features_for_disco(state));
        let response =
            match server_affiliation_for_requester(state, authenticated_session.as_ref()).await {
                Some(role) => build_disco_info_response_with_extensions(
                    request_iq,
                    &identities,
                    &features,
                    None,
                    &[build_server_role_form(role)],
                ),
                None => build_disco_info_response(request_iq, &identities, &features, None),
            };
        return vec![iq_to_xml(response)];
    }
    Vec::new()
}

async fn local_xmpp_account_exists(
    state: &WebSocketState,
    localpart: &str,
    domain: &str,
) -> Result<bool, String> {
    let row = state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbQueryOne {
            sql: r#"
                SELECT 1
                WHERE EXISTS (
                    SELECT 1 FROM native_users WHERE username = ? AND domain = ?
                )
                OR EXISTS (
                    SELECT 1 FROM users WHERE xmpp_localpart = ?
                )
            "#
            .to_string(),
            params: vec![localpart.into(), domain.into(), localpart.into()],
        })
        .await
        .map_err(|error| error.to_string())?;

    Ok(row.is_some())
}
