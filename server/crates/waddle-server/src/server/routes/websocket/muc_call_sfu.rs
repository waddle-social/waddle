//! Side-effect glue from MUC presence-unavailable / disconnect cleanup
//! into the LiveKit SFU registry.
//!
//! Without this, a session's SFU participant outlives its XMPP
//! presence: if a user closes the tab or their stream-management
//! session expires, the room actor processes a `LeaveByRealJid` but
//! the SFU never hears about it until LiveKit's own (long) timeout.
//! Meanwhile the channel UI keeps showing the user as "in call" and
//! the previously-issued JWT remains replayable.
//!
//! The XEP-0045 leave path is the authoritative XMPP signal that a
//! user is no longer in the room; mirroring it onto
//! [`waddle_sfu::SfuService::unregister_call_participant`] keeps the
//! SFU's view of the world in lock-step with XMPP. The SFU call
//! registry is keyed by the room JID (the call-id) and the user's
//! full JID (the identity), and `unregister_call_participant` is
//! idempotent — calling it for rooms the user never had a media
//! session in is a benign no-op.

use jid::{BareJid, FullJid};
use waddle_sfu::{CallId, Identity, MediaCapabilities};
use waddle_xmpp_core::types::Role;

use super::state::WebSocketState;

/// Tear down `jid`'s SFU participant in `room_jid`, if a SFU is
/// configured for this deployment.
///
/// No-op when:
/// - `LIVEKIT_*` env vars were not set (then `state.deps.protocol.sfu`
///   is `None` and there is no SFU to update),
/// - the room JID cannot be converted into a valid call-id (the
///   call-id grammar is stricter than the JID grammar, so this can
///   theoretically reject some MUC bare JIDs — those are rooms with
///   no calling capability and there is nothing on the SFU to undo
///   for them anyway).
pub(crate) fn unregister_participant_from_room(
    state: &WebSocketState,
    room_jid: &BareJid,
    jid: &FullJid,
) {
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        return;
    };
    unregister_participant_via_sfu(sfu, room_jid, jid);
}

/// SFU-handle variant of [`unregister_participant_from_room`] for
/// callers that don't hold a `WebSocketState` (the admin V2 command
/// handlers receive the SFU as an explicit dependency).
pub(crate) fn unregister_participant_via_sfu(
    sfu: &std::sync::Arc<dyn waddle_sfu::SfuService>,
    room_jid: &BareJid,
    jid: &FullJid,
) {
    let Ok(call_id) = CallId::new(room_jid.to_string()) else {
        // Room JID couldn't round-trip into a CallId, which means it
        // could never have been used to mint a join token either —
        // there's nothing to undo.
        return;
    };
    let identity = Identity::from_jid(jid.clone());
    let _ = sfu.unregister_call_participant(&call_id, &identity);
}

/// Converge `jid`'s live SFU media grants with their new XEP-0045
/// role after a non-removal role change (voice grant/revoke,
/// moderator grant/revoke). The SFU layer no-ops when `jid` is not a
/// current call participant, revokes outstanding join tokens on a
/// downgrade to listen-only, and pushes the new permission to the
/// live participant fire-and-forget — moderation IQ handling is
/// never blocked on LiveKit.
///
/// Same no-op conditions as [`unregister_participant_from_room`]
/// (no SFU configured, room JID not a valid call-id).
pub(crate) fn apply_role_grants_for_room(
    state: &WebSocketState,
    room_jid: &BareJid,
    jid: &FullJid,
    role: Role,
) {
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        return;
    };
    let Ok(call_id) = CallId::new(room_jid.to_string()) else {
        return;
    };
    let identity = Identity::from_jid(jid.clone());
    sfu.update_participant_capabilities(
        &call_id,
        &identity,
        MediaCapabilities::from_muc_role(role),
    );
}

/// Local-only teardown variant for the LiveKit webhook bridge. The
/// SFU's `participant_left` event is the acknowledgement that
/// LiveKit already removed the participant on its side — invoking
/// the full `unregister_participant_from_room` here would loop a
/// redundant `RemoveParticipant` admin call back to LiveKit (which
/// returns `not_found`, mapped to success, but the round-trip is
/// wasted and amplifies the race with quick rejoins). This helper
/// runs only the bookkeeping side.
pub(crate) fn note_participant_left_from_webhook(
    state: &WebSocketState,
    room_jid: &BareJid,
    jid: &FullJid,
) {
    let Ok(call_id) = CallId::new(room_jid.to_string()) else {
        return;
    };
    note_participant_left_by_call_id(state, &call_id, jid);
}

/// Raw-`CallId` variant of [`note_participant_left_from_webhook`] for
/// call ids that are NOT MUC room JIDs (#1128): 1:1 scoped ids
/// (`<initiator-bare>::<sid>`) deliberately fail the `BareJid` parse,
/// but their SFU registry entries and un-revoked JTIs still must be
/// cleaned when LiveKit reports the participant gone — otherwise a
/// crashed 1:1 peer lingers until reconciliation.
pub(crate) fn note_participant_left_by_call_id(
    state: &WebSocketState,
    call_id: &CallId,
    jid: &FullJid,
) {
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        return;
    };
    let identity = Identity::from_jid(jid.clone());
    sfu.note_participant_left(call_id, &identity);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state, create_test_websocket_state_with_sfu, RecordingSfu,
    };
    use std::sync::Arc;

    #[tokio::test]
    async fn unregister_is_no_op_when_no_sfu_is_configured() {
        // Pre-LiveKit deployments: protocol.sfu is None. The cleanup
        // path must remain a no-op so legacy installs aren't broken
        // by the teardown hook.
        let state = create_test_websocket_state().await;
        assert!(state.deps.protocol.sfu.is_none());
        let room: BareJid = "room@muc.example.com".parse().unwrap();
        let alice: FullJid = "alice@example.com/web".parse().unwrap();
        // Should not panic, not write anywhere.
        unregister_participant_from_room(&state, &room, &alice);
    }

    #[tokio::test]
    async fn unregister_records_call_id_and_identity_on_the_sfu() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let room: BareJid = "room@muc.example.com".parse().unwrap();
        let alice: FullJid = "alice@example.com/web".parse().unwrap();

        unregister_participant_from_room(&state, &room, &alice);

        let recorded = recorder.snapshot();
        assert_eq!(recorded.len(), 1, "exactly one teardown call expected");
        let (call_id, identity) = &recorded[0];
        assert_eq!(call_id.as_str(), "room@muc.example.com");
        assert_eq!(identity.as_livekit_identity(), "alice@example.com/web");
    }

    #[tokio::test]
    async fn unregister_is_idempotent_across_repeated_calls() {
        // Disconnect cleanup and graceful presence-unavailable may
        // both fire for the same `(room, jid)` if a tab close races
        // with an explicit leave. SfuService::unregister is
        // documented as idempotent — verify the helper passes
        // through the second call rather than short-circuiting,
        // since silently skipping would mask bugs in the SFU layer.
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let room: BareJid = "room@muc.example.com".parse().unwrap();
        let alice: FullJid = "alice@example.com/web".parse().unwrap();

        unregister_participant_from_room(&state, &room, &alice);
        unregister_participant_from_room(&state, &room, &alice);

        let recorded = recorder.snapshot();
        assert_eq!(recorded.len(), 2, "both teardown calls reach the SFU");
        assert_eq!(recorded[0], recorded[1]);
    }
}
