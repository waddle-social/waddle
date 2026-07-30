use super::*;

pub(super) async fn handle_command_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
) -> Option<DiscoInfoResponse<'a>> {
    if req.target_to == Some(req.domain) && req.node == Some(NODE_COMMANDS) {
        let identities = vec![Identity::command_list(Some("Ad-Hoc Commands"))];
        let features = vec![
            Feature::disco_info(),
            Feature::disco_items(),
            Feature::commands(),
        ];
        let response =
            build_disco_info_response(req.request_iq, &identities, &features, Some(NODE_COMMANDS));
        return Some(DiscoInfoResponse::iq(response));
    }

    if req.target_to == Some(req.domain) {
        if let Some(node) = req.node {
            let commands = state.deps.protocol.command_registry.list_commands().await;
            if let Some(name) = command_name_by_boundary(&commands, node, CommandBoundary::Server) {
                let identities = vec![Identity::automation(Some(name))];
                let features = vec![
                    Feature::disco_info(),
                    Feature::commands(),
                    Feature::new(DATA_FORMS_NS),
                ];
                let response =
                    build_disco_info_response(req.request_iq, &identities, &features, Some(node));
                return Some(DiscoInfoResponse::iq(response));
            }
        }
    }

    None
}

pub(super) async fn handle_server_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
    authenticated_session: Option<&Session>,
) -> DiscoInfoResponse<'a> {
    // Disco info on server. Source the canonical feature catalogue
    // from `waddle-xmpp-core::disco::info::server_features()` so the
    // rich-message XEPs (corrections, retractions, reactions,
    // references, stanza-ids, etc.) declared there stay discoverable
    // here without drift between the two lists. Server-instance
    // additions (Spaces, jabber:iq:search) are appended below,
    // and dynamic extension namespaces extend further still.
    let identities = vec![Identity::server(Some("Waddle"))];
    let mut features = waddle_xmpp::disco::info::server_features();
    features.push(Feature::new("jabber:iq:search"));
    features.extend(extension_features_for_disco(state));
    // Advertise XMPP-native A/V calling only when the Jingle handler
    // is registered. `register_call_handlers` is invoked at startup
    // iff `SfuConfig::from_env` produced a config — so the
    // dispatcher's handler set is the canonical "is calling
    // available" flag. Lying about features that have no behavior
    // is the lesson we took from PR #243.
    if state
        .deps
        .protocol
        .dispatcher
        .has_iq_handler("urn:xmpp:jingle:1")
    {
        features.extend(waddle_xmpp::disco::info::call_features());
    }
    let response = match server_affiliation_for_requester(state, authenticated_session).await {
        Some(role) => build_disco_info_response_with_extensions(
            req.request_iq,
            &identities,
            &features,
            None,
            &[build_server_role_form(role)],
        ),
        None => build_disco_info_response(req.request_iq, &identities, &features, None),
    };
    DiscoInfoResponse::iq(response)
}
