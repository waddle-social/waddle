//! Room administration asked by a principal at another server (ADR-0020
//! D10, with D1 and D8 at the effect boundary): XEP-0045 `muc#admin` and
//! `muc#owner`, and XEP-0425 moderation.
//!
//! The requests run through the handlers a session's requests run through.
//! What differs is who the requester is to every decision they take: no
//! session of this deployment, and the evidence of the inbound S2S stream
//! that carried the request in place of none. A room an enrolled scope
//! manages decides from its canonical grants as honoured for that
//! evidence, behind the revocation fence; any other room from the standing
//! it holds for the requester, honoured as the effect boundary says. A
//! remote principal therefore administers a room only from a trusted home
//! on a `pkix` stream, and everyone else is refused as XEP-0045 says
//! (`forbidden`).

use jid::FullJid;
use xmpp_parsers::iq::Iq;

use super::dispatch::IqHandlerContext;
use super::muc_owner_moderation::{handle_muc_owner_and_moderation_iq_as, RoomRequester};
use super::{handle_muc_admin_iq, ResponseBatch, WebSocketState};
use crate::permissions::host_grant::PrincipalEvidence;

/// The addresses a request is answered between: from the address it was
/// sent to, to its sender.
fn answered_between(iq: &Iq) -> (Option<String>, Option<String>) {
    (
        iq.to().map(ToString::to_string),
        iq.from().map(ToString::to_string),
    )
}

/// A `muc#admin` request of `requester`, decided on `evidence`.
pub(crate) async fn muc_admin_for_remote(
    state: &WebSocketState,
    iq: &Iq,
    requester: &FullJid,
    evidence: &PrincipalEvidence,
) -> ResponseBatch {
    let (to, from) = answered_between(iq);
    handle_muc_admin_iq(
        iq,
        state.deps.service_domains.muc.as_str(),
        state,
        Some(requester),
        evidence,
        to.as_deref(),
        from.as_deref(),
    )
    .await
}

/// A `muc#owner` request (configuration or destruction) or a XEP-0425
/// moderation request of `requester`, decided on `evidence`. The batch is
/// empty for an IQ that is neither.
pub(crate) async fn muc_owner_or_moderation_for_remote(
    state: &WebSocketState,
    iq: &Iq,
    requester: &FullJid,
    evidence: &PrincipalEvidence,
) -> ResponseBatch {
    let (to, from) = answered_between(iq);
    let (payload_ns, has_destroy) = match iq {
        Iq::Get { payload, .. } => (payload.ns(), false),
        Iq::Set { payload, .. } => (
            payload.ns(),
            payload
                .get_child("destroy", waddle_xmpp::muc::NS_MUC_OWNER)
                .is_some(),
        ),
        Iq::Result { .. } | Iq::Error { .. } => return ResponseBatch::default(),
    };
    let domains = &state.deps.service_domains;
    let context = IqHandlerContext {
        iq,
        id: iq.id(),
        payload_ns: payload_ns.as_str(),
        target_to: to.as_deref(),
        has_destroy,
        domain: state.deps.auth_state.xmpp_domain.as_str(),
        muc_domain: domains.muc.as_str(),
        upload_domain: domains.upload.as_str(),
        spaces_domain: domains.spaces.as_str(),
        community_domain: domains.community.as_str(),
        extensions_domain: domains.extensions.as_str(),
        push_domain: domains.push.as_str(),
        response_from: to.as_deref(),
        response_to: from.as_deref(),
    };
    handle_muc_owner_and_moderation_iq_as(
        context,
        state,
        RoomRequester {
            jid: Some(requester),
            // A remote principal holds no session here, and nothing a
            // session's request writes for its account is written for it.
            session: None,
            evidence: *evidence,
        },
    )
    .await
}
