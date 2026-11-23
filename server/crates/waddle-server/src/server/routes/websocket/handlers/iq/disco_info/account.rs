use super::super::service_unavailable_iq_error;
use super::{
    build_disco_info_response, build_pep_identity, pep_features, BareJid, ConnectionPhase,
    DiscoInfoRequest, DiscoInfoResponse, Feature, Identity, WebSocketState,
};
use crate::server::routes::websocket::s2s_accounts::{account_exists, shares_presence_with};

/// Whether `requester` may discover the account `target`: the account itself,
/// or an entity holding a presence subscription of type `from` or `both` to
/// it (RFC 6121 §8; XEP-0030 Security Considerations). A nonexistent account
/// and an unauthorized requester are not told apart: both are `false`, and
/// the caller answers both with the same `<service-unavailable/>`.
pub(in crate::server::routes::websocket::handlers::iq) async fn requester_may_discover_account(
    state: &WebSocketState,
    target: &BareJid,
    requester: Option<&jid::FullJid>,
) -> bool {
    let Some(requester) = requester else {
        return false;
    };
    let requester = requester.to_bare();
    if requester == *target {
        return true;
    }
    shares_presence_with(state, target, &requester).await && account_exists(state, target).await
}

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

    if !requester_may_discover_account(state, &target_bare, Some(bound_jid)).await {
        return Some(DiscoInfoResponse::error(
            req.id,
            req.response_from,
            req.response_to,
            service_unavailable_iq_error("Service unavailable at this address."),
        ));
    }

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
