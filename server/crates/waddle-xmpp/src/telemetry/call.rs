//! Typed counters for the `waddle.call.sfu_token.*` family, shared by
//! the protocol-layer Jingle handler and the server-side Muji gate so
//! one family is never emitted from divergent macro sites.

use super::attributes::SfuDenialReason;

/// Count a minted LiveKit SFU token.
pub fn increment_sfu_token_minted() {
    crate::counter_add!(
        "waddle.call.sfu_token.minted",
        "1",
        "LiveKit SFU tokens minted.",
        1,
    );
}

/// Count an SFU token denial by reason.
pub fn increment_sfu_token_denied(reason: SfuDenialReason) {
    crate::counter_add!(
        "waddle.call.sfu_token.denied",
        "1",
        "SFU token requests denied by reason.",
        1,
        reason,
    );
}
