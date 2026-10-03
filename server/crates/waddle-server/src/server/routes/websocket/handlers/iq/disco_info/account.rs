use super::*;

pub(super) async fn handle_account_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
) -> Option<DiscoInfoResponse<'a>> {
    let (Some(target), Some(bound_jid)) = (req.target_to, phase.bound_jid()) else {
        return None;
    };
    let target_bare = target.parse::<BareJid>().ok()?;

    if target_bare == bound_jid.to_bare() {
        let identities = vec![
            Identity::server(Some("Personal Archive")),
            build_pep_identity(),
        ];
        // `urn:xmpp:mam:2` (+`#extended`) come from `pep_features()`;
        // do NOT add them again here — duplicate `<feature/>` vars make
        // the response ill-formed per XEP-0115 §5.4 (#1259).
        let mut features = vec![
            Feature::disco_info(),
            Feature::fulltext_mam(),
            Feature::threads_query(),
        ];
        features.extend(pep_features());
        let response = build_disco_info_response(req.request_iq, &identities, &features, None);
        return Some(DiscoInfoResponse::iq(response));
    }

    if target_bare.domain().as_str() != req.domain || target_bare.node().is_none() {
        return None;
    }

    let Some(localpart) = target_bare.node() else {
        return Some(DiscoInfoResponse::error(
            req.id,
            req.response_from,
            req.response_to,
            item_not_found_iq_error("Requested item not found."),
        ));
    };

    match crate::auth::local_account_exists(
        state.deps.app_state.db_pool.global_actor(),
        localpart.as_str(),
        req.domain,
    )
    .await
    {
        Ok(true) => {
            let identities = vec![build_pep_identity()];
            let mut features = vec![Feature::disco_info()];
            features.extend(pep_features().into_iter().filter(|feature| {
                !matches!(
                    feature.0.as_str(),
                    "urn:xmpp:mam:2" | "urn:xmpp:mam:2#extended"
                )
            }));
            let response = build_disco_info_response(req.request_iq, &identities, &features, None);
            Some(DiscoInfoResponse::iq(response))
        }
        Ok(false) => Some(DiscoInfoResponse::error(
            req.id,
            req.response_from,
            req.response_to,
            item_not_found_iq_error("Requested item not found."),
        )),
        Err(error) => {
            warn!(target = %target_bare, error = %error, "Failed to resolve PEP disco target");
            Some(DiscoInfoResponse::error(
                req.id,
                req.response_from,
                req.response_to,
                internal_server_error_iq_error("Internal server error."),
            ))
        }
    }
}
