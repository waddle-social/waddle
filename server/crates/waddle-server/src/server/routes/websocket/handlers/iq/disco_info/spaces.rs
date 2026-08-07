use super::*;
use crate::server::routes::websocket::ResolvedPrincipal;

pub(super) async fn handle_spaces_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
    principal: Option<ResolvedPrincipal<'_>>,
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

        let Some(mut space) = space_details_from_node(&space_node) else {
            warn!(
                node,
                access_model = %space_node.config.access_model,
                "Spaces node has unsupported access model"
            );
            return Some(DiscoInfoResponse::error(
                req.id,
                None,
                None,
                internal_server_error_iq_error("Internal server error."),
            ));
        };
        if let Ok(Some(metadata)) = state
            .deps
            .app_state
            .spaces_metadata_store
            .get_by_node(&crate::space_identity::SpaceNode::from(node))
            .await
        {
            space.name = metadata.name;
            space.description = metadata.description;
            space.icon_url = metadata.icon_url;
            if let Some(created_at) =
                chrono::DateTime::<chrono::Utc>::from_timestamp(metadata.created_at, 0)
            {
                space.created_at = created_at.to_rfc3339();
            }
        }
        let requester_affiliation = space_affiliation_for_requester(state, principal, node).await;
        let owner_jids = state
            .deps
            .protocol
            .pubsub_storage
            .list_node_affiliations(&spaces_jid, node)
            .await
            .map(|rows| {
                rows.into_iter()
                    .filter_map(|(jid, affiliation)| {
                        (affiliation == waddle_xmpp::pubsub::Affiliation::Owner)
                            .then(|| jid.to_string())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let identities = vec![Identity::pubsub_leaf(Some(&space.name))];
        let features = vec![
            Feature::disco_info(),
            Feature::pubsub(),
            Feature::pubsub_retrieve_items(),
            Feature::spaces(),
        ];
        let metadata = build_spaces_metadata_form_for_requester_with_owners(
            &space,
            requester_affiliation,
            &owner_jids,
        );
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
