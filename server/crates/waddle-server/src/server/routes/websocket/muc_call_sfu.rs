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
use waddle_sfu::{CallId, Identity};

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
    let Ok(call_id) = CallId::new(room_jid.to_string()) else {
        // Room JID couldn't round-trip into a CallId, which means it
        // could never have been used to mint a join token either —
        // there's nothing to undo.
        return;
    };
    let identity = Identity::from_jid(jid.clone());
    let _ = sfu.unregister_call_participant(&call_id, &identity);
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
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        return;
    };
    let Ok(call_id) = CallId::new(room_jid.to_string()) else {
        return;
    };
    let identity = Identity::from_jid(jid.clone());
    sfu.note_participant_left(&call_id, &identity);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use std::sync::{Arc, Mutex};
    use waddle_sfu::{
        CallState, JoinToken, Jti, MediaCapabilities, SfuError, SfuService, TurnCredential,
        TurnHost, WebsocketUrl,
    };

    /// Recording fake: captures every `(call_id, identity)` passed to
    /// `unregister_call_participant`. The other trait methods are
    /// unimplemented because the production code path under test only
    /// touches the unregister method.
    #[derive(Default)]
    struct RecordingSfu {
        calls: Mutex<Vec<(CallId, Identity)>>,
    }

    impl RecordingSfu {
        fn snapshot(&self) -> Vec<(CallId, Identity)> {
            self.calls.lock().expect("recording lock").clone()
        }
    }

    impl SfuService for RecordingSfu {
        fn issue_join_token(
            &self,
            _: &CallId,
            _: &Identity,
            _: MediaCapabilities,
        ) -> Result<JoinToken, SfuError> {
            unimplemented!("not exercised by these tests")
        }

        fn issue_turn_credentials(&self, _: &Identity) -> Result<TurnCredential, SfuError> {
            unimplemented!("not exercised by these tests")
        }

        fn register_call_participant(&self, _: &CallId, _: &Identity) {}

        fn has_call_participant(&self, _: &CallId, _: &Identity) -> bool {
            false
        }

        fn unregister_call_participant(&self, call_id: &CallId, identity: &Identity) -> CallState {
            self.calls
                .lock()
                .expect("recording lock")
                .push((call_id.clone(), identity.clone()));
            CallState::Ended
        }

        fn note_participant_left(&self, call_id: &CallId, identity: &Identity) {
            // The fake records `note_participant_left` calls into the
            // same vec — these tests only assert that the helper
            // dispatches *some* SFU teardown for the room/identity.
            // The fixture's lifecycle distinction (webhook-driven vs
            // graceful-unavailable) is exercised in `waddle-sfu`'s
            // own unit tests.
            self.calls
                .lock()
                .expect("recording lock")
                .push((call_id.clone(), identity.clone()));
        }

        fn is_revoked(&self, _: &Jti) -> bool {
            false
        }

        fn ws_url(&self) -> &WebsocketUrl {
            unimplemented!("not exercised by these tests")
        }

        fn turn_host(&self) -> &TurnHost {
            unimplemented!("not exercised by these tests")
        }

        fn webhook_secret(&self) -> &waddle_sfu::ApiSecret {
            unimplemented!("not exercised by these tests")
        }

        fn participants_for_call(&self, _: &CallId) -> Vec<Identity> {
            Vec::new()
        }
    }

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

    /// Build a test state with a pluggable `SfuService`. The canonical
    /// fixture (`create_test_websocket_state`) sets `sfu: None` so
    /// non-call tests don't exercise SFU code paths; this helper
    /// rebuilds the protocol services with the SFU slot filled.
    async fn state_with_sfu(sfu: Arc<RecordingSfu>) -> Arc<WebSocketState> {
        let base = create_test_websocket_state().await;
        // `Arc::clone` + restamp the field by constructing a new
        // WebSocketState wrapping the same deps. The `ProtocolServices`
        // is owned by an `Arc<WebSocketState>`, so we have to clone
        // each field — they're all `Arc`-shareable except the
        // SFU slot we want to fill.
        let deps = &base.deps;
        Arc::new(WebSocketState {
            deps: super::super::state::WebSocketDeps {
                app_state: Arc::clone(&deps.app_state),
                auth_state: Arc::clone(&deps.auth_state),
                service_domains: deps.service_domains.clone(),
                protocol: super::super::state::ProtocolServices {
                    connection_registry: Arc::clone(&deps.protocol.connection_registry),
                    room_registry: deps.protocol.room_registry.clone(),
                    mam_storage: Arc::clone(&deps.protocol.mam_storage),
                    inbox_storage: Arc::clone(&deps.protocol.inbox_storage),
                    threads_storage: Arc::clone(&deps.protocol.threads_storage),
                    blocking_storage: Arc::clone(&deps.protocol.blocking_storage),
                    pending_delivery_storage: Arc::clone(&deps.protocol.pending_delivery_storage),
                    command_registry: Arc::clone(&deps.protocol.command_registry),
                    extension_manager: Arc::clone(&deps.protocol.extension_manager),
                    dispatcher: Arc::clone(&deps.protocol.dispatcher),
                    pubsub_storage: Arc::clone(&deps.protocol.pubsub_storage),
                    push_store: Arc::clone(&deps.protocol.push_store),
                    push_service: Arc::clone(&deps.protocol.push_service),
                    notification_outbox: Arc::clone(&deps.protocol.notification_outbox),
                    notification_settings_projection: Arc::clone(
                        &deps.protocol.notification_settings_projection,
                    ),
                    dnd_projection: Arc::clone(&deps.protocol.dnd_projection),
                    dnd_reader: Arc::clone(&deps.protocol.dnd_reader),
                    notification_activity: Arc::clone(&deps.protocol.notification_activity),
                    isr_token_store: Arc::clone(&deps.protocol.isr_token_store),
                    sm_session_registry: Arc::clone(&deps.protocol.sm_session_registry),
                    resumable_sessions: Arc::clone(&deps.protocol.resumable_sessions),
                    caps_resolver: Arc::clone(&deps.protocol.caps_resolver),
                    avatar_source_locks: Arc::clone(&deps.protocol.avatar_source_locks),
                    profile_publish_tracker: deps.protocol.profile_publish_tracker.clone(),
                    pep_feed_bridge: Arc::clone(&deps.protocol.pep_feed_bridge),
                    sfu: Some(sfu),
                },
                occupant_id_secret: deps.occupant_id_secret.clone(),
                provider_ingress: Arc::clone(&deps.provider_ingress),
                provider_dispatch_tasks: deps.provider_dispatch_tasks.clone(),
            },
        })
    }

    #[tokio::test]
    async fn unregister_records_call_id_and_identity_on_the_sfu() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = state_with_sfu(Arc::clone(&recorder)).await;
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
        let state = state_with_sfu(Arc::clone(&recorder)).await;
        let room: BareJid = "room@muc.example.com".parse().unwrap();
        let alice: FullJid = "alice@example.com/web".parse().unwrap();

        unregister_participant_from_room(&state, &room, &alice);
        unregister_participant_from_room(&state, &room, &alice);

        let recorded = recorder.snapshot();
        assert_eq!(recorded.len(), 2, "both teardown calls reach the SFU");
        assert_eq!(recorded[0], recorded[1]);
    }
}
