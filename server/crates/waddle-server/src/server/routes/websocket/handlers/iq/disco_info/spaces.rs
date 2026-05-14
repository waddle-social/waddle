use super::*;

pub(super) async fn handle_spaces_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
    authenticated_session: Option<&Session>,
) -> Option<DiscoInfoResponse<'a>> {
    if req.target_to != Some(req.spaces_domain) {
        return None;
    }

    if let Some(node) = req.node {
        let Ok(spaces_jid) = spaces_service_bare_jid(req.spaces_domain) else {
            return Some(DiscoInfoResponse::error(
                req.id,
                None,
                None,
                internal_server_error_iq_error("Internal server error."),
            ));
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
                return Some(DiscoInfoResponse::error(
                    req.id,
                    None,
                    None,
                    item_not_found_iq_error("Requested item not found."),
                ));
            }
            Err(error) => {
                warn!(node, error = %error, "Failed to resolve Spaces node info");
                return Some(DiscoInfoResponse::error(
                    req.id,
                    None,
                    None,
                    item_not_found_iq_error("Requested item not found."),
                ));
            }
        };

        let space = space_details_from_node(&space_node);
        let requester_affiliation =
            space_affiliation_for_requester(state, authenticated_session, node).await;
        let identities = vec![Identity::pubsub_leaf(Some(&space.name))];
        let features = vec![
            Feature::disco_info(),
            Feature::pubsub(),
            Feature::pubsub_retrieve_items(),
            Feature::spaces(),
        ];
        let metadata = build_spaces_metadata_form_for_requester(&space, requester_affiliation);
        let response = build_disco_info_response_with_extensions(
            req.request_iq,
            &identities,
            &features,
            Some(node),
            &[metadata],
        );
        return Some(DiscoInfoResponse::iq(response));
    }

    let identities = vec![Identity::spaces_service(Some("Spaces"))];
    let features = spaces_service_features();
    let response = build_disco_info_response(req.request_iq, &identities, &features, None);
    Some(DiscoInfoResponse::iq(response))
}
