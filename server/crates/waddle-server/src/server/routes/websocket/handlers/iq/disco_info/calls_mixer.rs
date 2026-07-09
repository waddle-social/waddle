//! XEP-0030 disco#info handler for the XEP-0272 Muji SFU mixer
//! JID (`calls.<server-domain>`).
//!
//! Required so a strict client can discover the mixer's identity
//! and feature set BEFORE blindly sending a session-initiate.
//! Mirrors the XEP-0272 / av-conferences ProtoXEP convention:
//! `<identity category='conference' type='audio-video'/>` plus the
//! Muji + Jingle + RTP + LiveKit-transport features the server
//! understands.

use super::*;
use crate::server::disco_targets::{
    calls_available, calls_mixer_target_features, target_identities, DiscoTarget,
};

/// Handle a disco#info query targeted at `calls.<server-domain>`.
/// Returns `None` for queries to any other JID so the dispatcher
/// chain falls through to the next handler.
pub(super) fn handle_calls_mixer_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
) -> Option<DiscoInfoResponse<'a>> {
    let expected = format!("calls.{}", req.domain);
    if req.target_to != Some(expected.as_str()) {
        return None;
    }

    if !calls_available(state) {
        return Some(DiscoInfoResponse::error(
            req.id,
            req.response_from,
            req.response_to,
            service_unavailable_iq_error("Calling is not available."),
        ));
    }

    let identities = target_identities(DiscoTarget::CallsMixer);
    let features = calls_mixer_target_features();

    let response = build_disco_info_response(req.request_iq, &identities, &features, None);
    Some(DiscoInfoResponse::iq(response))
}
