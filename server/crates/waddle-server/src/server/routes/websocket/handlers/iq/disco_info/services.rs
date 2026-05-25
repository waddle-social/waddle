use super::*;

pub(super) fn handle_upload_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
) -> Option<DiscoInfoResponse<'a>> {
    if req.target_to != Some(req.upload_domain) {
        return None;
    }

    let identities = vec![Identity::upload_service(Some("HTTP File Upload"))];
    let features = upload_service_features();
    let response = build_disco_info_response(req.request_iq, &identities, &features, None);
    Some(DiscoInfoResponse::iq(response))
}

pub(super) async fn handle_push_service_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
) -> Option<DiscoInfoResponse<'a>> {
    if req.target_to != Some(req.push_domain) {
        return None;
    }

    if let Some(node) = req.node {
        let Some(owner_bare_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
            return Some(DiscoInfoResponse::error(
                req.id,
                req.response_from,
                req.response_to,
                item_not_found_iq_error("Requested Push Service node not found."),
            ));
        };
        match state
            .deps
            .protocol
            .push_service
            .get_node_for_owner(&owner_bare_jid, node)
            .await
        {
            Ok(Some(_push_node)) => {
                let identities = vec![Identity::pubsub_leaf(Some("Push Node"))];
                let features = vec![
                    Feature::disco_info(),
                    Feature::pubsub(),
                    Feature::pubsub_publish(),
                    Feature::pubsub_access_whitelist(),
                    Feature::pubsub_publish_only_affiliation(),
                    Feature::push(),
                ];
                let response =
                    build_disco_info_response(req.request_iq, &identities, &features, Some(node));
                return Some(DiscoInfoResponse::iq(response));
            }
            Ok(None) => {
                return Some(DiscoInfoResponse::error(
                    req.id,
                    req.response_from,
                    req.response_to,
                    item_not_found_iq_error("Requested Push Service node not found."),
                ));
            }
            Err(error) => {
                warn!(error = %error, node, "Failed to load Push Service node disco info");
                return Some(DiscoInfoResponse::error(
                    req.id,
                    req.response_from,
                    req.response_to,
                    internal_server_error_iq_error("Internal server error."),
                ));
            }
        }
    }

    let identities = vec![Identity::pubsub_push(Some("Push Service"))];
    let features = push_service_features();
    // XEP-0128 extension: advertise the active VAPID public key + kid so
    // chat clients can subscribe without a build-time embedded key. The
    // form is suppressed when no VapidSigner is installed (legacy test
    // boot, integration runs without a configured push provider) — the
    // chat side treats an absent form as "Web Push not available on this
    // server" rather than silently degrading.
    let vapid_form = state
        .deps
        .protocol
        .push_service
        .vapid_advertisement()
        .map(|advertisement| waddle_xmpp::push::disco::push_vapid_disco_form(&advertisement));
    let response = if let Some(form) = vapid_form.as_ref() {
        build_disco_info_response_with_extensions(
            req.request_iq,
            &identities,
            &features,
            None,
            std::slice::from_ref(form),
        )
    } else {
        build_disco_info_response(req.request_iq, &identities, &features, None)
    };
    Some(DiscoInfoResponse::iq(response))
}
