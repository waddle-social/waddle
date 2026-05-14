use super::*;

pub(super) async fn handle_account_disco_info(
    req: &DiscoInfoRequest<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
) -> Option<Vec<String>> {
    let (Some(target), Some(bound_jid)) = (req.target_to, phase.bound_jid()) else {
        return None;
    };
    let target_bare = target.parse::<BareJid>().ok()?;

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
        let response = build_disco_info_response(req.request_iq, &identities, &features, None);
        return Some(vec![iq_to_xml(response)]);
    }

    if target_bare.domain().as_str() != req.domain || target_bare.node().is_none() {
        return None;
    }

    let Some(localpart) = target_bare.node() else {
        return Some(vec![build_iq_error_xml_typed(
            req.id,
            req.response_from,
            req.response_to,
            item_not_found_iq_error("Requested item not found."),
        )]);
    };

    match local_xmpp_account_exists(state, localpart.as_str(), req.domain).await {
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
            Some(vec![iq_to_xml(response)])
        }
        Ok(false) => Some(vec![build_iq_error_xml_typed(
            req.id,
            req.response_from,
            req.response_to,
            item_not_found_iq_error("Requested item not found."),
        )]),
        Err(error) => {
            warn!(target = %target_bare, error = %error, "Failed to resolve PEP disco target");
            Some(vec![build_iq_error_xml_typed(
                req.id,
                req.response_from,
                req.response_to,
                internal_server_error_iq_error("Internal server error."),
            )])
        }
    }
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
