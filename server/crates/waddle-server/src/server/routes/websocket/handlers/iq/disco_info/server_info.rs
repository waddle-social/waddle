use super::*;
use crate::server::disco_targets::{
    calls_available, server_target_features, target_identities, DiscoTarget, RuntimeFeatureOptions,
};

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
    let identities = target_identities(DiscoTarget::Server);
    // ADR-0017 Phase 3 Slice 8 (Q8): XEP-0397 ISR is advertised ONLY when
    // `clustering.enabled && Postgres` — `isr_token_store().is_some()` is
    // the single source of truth for that gate (see its doc comment), so
    // the advertised capability can never drift from what is actually
    // wired. Corrected premise (FIX 8): before this slice ISR was NOT
    // unadvertised — it was advertised UNCONDITIONALLY and
    // non-conformantly (the wrong namespace, `urn:xmpp:isr:0`, with no
    // gating at all). This slice is what first gates it correctly.
    let isr_available = state
        .deps
        .app_state
        .clustering_claims
        .isr_token_store()
        .is_some();
    let extension_features = extension_features_for_disco(state);
    let features = server_target_features(RuntimeFeatureOptions {
        calls_available: calls_available(state),
        isr_available,
        extension_features: &extension_features,
    });
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
