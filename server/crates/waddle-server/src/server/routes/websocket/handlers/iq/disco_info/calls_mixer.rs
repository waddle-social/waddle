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

/// Handle a disco#info query targeted at `calls.<server-domain>`.
/// Returns `None` for queries to any other JID so the dispatcher
/// chain falls through to the next handler.
pub(super) fn handle_calls_mixer_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
) -> Option<DiscoInfoResponse<'a>> {
    let expected = format!("calls.{}", req.domain);
    if req.target_to != Some(expected.as_str()) {
        return None;
    }

    // Identity matches the av-conferences ProtoXEP convention
    // (`category='conference' type='audio-video'`). Lets a strict
    // peer disco the mixer and confirm it's a focus before
    // session-initiate.
    let identities = vec![Identity::audio_video_conference(Some(
        "Waddle Group Call Mixer",
    ))];

    // Features the mixer accepts on the wire. `Feature::muji()` is
    // the load-bearing one (XEP-0272); the rest document the
    // shape of the Jingle session-initiate the mixer accepts.
    let features = vec![
        Feature::disco_info(),
        Feature::muji(),
        Feature::jingle(),
        Feature::jingle_rtp(),
        Feature::jingle_rtp_audio(),
        Feature::jingle_rtp_video(),
        Feature::waddle_livekit_transport(),
    ];

    let response = build_disco_info_response(req.request_iq, &identities, &features, None);
    Some(DiscoInfoResponse::iq(response))
}
