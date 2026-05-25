use super::*;

pub(super) async fn handle_extensions_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
) -> Option<DiscoInfoResponse<'a>> {
    if req.target_to != Some(req.extensions_domain) {
        return None;
    }

    if req.node == Some(NODE_COMMANDS) {
        let identities = vec![Identity::command_list(Some("Extension Commands"))];
        let features = vec![
            Feature::disco_info(),
            Feature::disco_items(),
            Feature::commands(),
        ];
        let response =
            build_disco_info_response(req.request_iq, &identities, &features, Some(NODE_COMMANDS));
        return Some(DiscoInfoResponse::iq(response));
    }

    if let Some(node) = req.node {
        let commands = state.deps.protocol.command_registry.list_commands().await;
        if command_name_by_boundary(&commands, node, CommandBoundary::Extensions).is_some() {
            let Some((plugin, descriptor)) = state
                .deps
                .protocol
                .extension_manager
                .command_descriptors()
                .into_iter()
                .find(|(_, descriptor)| descriptor.node.as_str() == node)
            else {
                return Some(DiscoInfoResponse::error(
                    req.id,
                    req.response_from,
                    req.response_to,
                    item_not_found_iq_error("Requested item not found."),
                ));
            };
            let identities = vec![Identity::automation(Some(descriptor.name.as_str()))];
            let features = vec![
                Feature::disco_info(),
                Feature::commands(),
                Feature::new(DATA_FORMS_NS),
                Feature::new(EXTENSION_COMMAND_FORM_TYPE),
            ];
            let manifest = state
                .deps
                .protocol
                .extension_manager
                .manifest_for_plugin(plugin.as_str());
            let profile = manifest
                .as_ref()
                .and_then(|manifest| manifest.profile.as_ref());
            let form = extension_command_metadata_form(&plugin, &descriptor, profile);
            let response = build_disco_info_response_with_extensions(
                req.request_iq,
                &identities,
                &features,
                Some(node),
                &[form],
            );
            return Some(DiscoInfoResponse::iq(response));
        }

        let Some(route) = state
            .deps
            .protocol
            .extension_manager
            .route_descriptors()
            .iter()
            .find(|route| extension_route_disco_node(route) == node)
        else {
            return Some(DiscoInfoResponse::error(
                req.id,
                req.response_from,
                req.response_to,
                item_not_found_iq_error("Requested item not found."),
            ));
        };
        let identities = vec![Identity::new(
            "waddle",
            "extension-route",
            Some(route.label.as_str()),
        )];
        let features = vec![
            Feature::disco_info(),
            Feature::new(DATA_FORMS_NS),
            Feature::new("urn:waddle:extension:1"),
            Feature::new(EXTENSION_ROUTE_FORM_TYPE),
            Feature::new(route.payload_namespace.as_str()),
        ];
        let manifest = state
            .deps
            .protocol
            .extension_manager
            .manifest_for_plugin(route.plugin.as_str());
        let profile = manifest
            .as_ref()
            .and_then(|manifest| manifest.profile.as_ref());
        let form = extension_route_metadata_form(route, profile);
        let response = build_disco_info_response_with_extensions(
            req.request_iq,
            &identities,
            &features,
            Some(node),
            &[form],
        );
        return Some(DiscoInfoResponse::iq(response));
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
    let response = build_disco_info_response(req.request_iq, &identities, &features, None);
    Some(DiscoInfoResponse::iq(response))
}
