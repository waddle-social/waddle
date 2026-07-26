//! Unit tests for the [`super::LiveKitSfu`] bridge.
//!
//! A child module, so it keeps access to the parent's private state
//! (`grant_locks`, `desired_grants`, `issued`) that several invariants
//! here assert on directly.

use super::*;
use crate::config::{ApiKey, ApiSecret, TurnSharedSecret};
use chrono::Duration;
use jid::FullJid;
use url::Url;

fn fixture_config() -> SfuConfig {
    SfuConfig {
        api_key: ApiKey::new("APIxxxxxxxx"),
        api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length"),
        webhook_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length"),
        ws_url: WebsocketUrl::new(Url::parse("wss://livekit.waddle.social").unwrap()).unwrap(),
        turn_host: TurnHost::new("turn.waddle.social"),
        turn_tls_port: 443,
        turn_udp_port: 3478,
        turn_shared_secret: TurnSharedSecret::from_text("turn-shared-secret-value"),
        token_ttl: Duration::seconds(3600),
        turn_ttl: Duration::seconds(3600),
    }
}

fn fixture_identity(name: &str) -> Identity {
    let jid: FullJid = format!("{name}@waddle.social/desktop")
        .parse()
        .expect("jid");
    Identity::from_jid(jid)
}

#[test]
fn registry_tracks_participants_per_call() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("r1").unwrap();
    let a = fixture_identity("alice");
    let b = fixture_identity("bob");

    sfu.register_call_participant(&call, &a);
    sfu.register_call_participant(&call, &b);
    assert_eq!(sfu.participant_count(&call), 2);

    match sfu.unregister_call_participant(&call, &a) {
        CallState::Active { remaining } => assert_eq!(remaining, 1),
        CallState::Ended => panic!("call should still be active"),
    }

    match sfu.unregister_call_participant(&call, &b) {
        CallState::Ended => {}
        CallState::Active { .. } => panic!("call should end with no participants"),
    }
    assert_eq!(sfu.participant_count(&call), 0);
}

#[test]
fn issue_join_token_returns_room_scoped_jwt() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("c1").unwrap();
    let identity = fixture_identity("alice");

    let token = sfu
        .issue_join_token(&call, &identity, MediaCapabilities::direct_call_peer())
        .expect("token issued");
    assert_eq!(token.room, call);
    assert!(!token.jwt.as_str().is_empty());
}

#[test]
fn issue_turn_credentials_yields_time_limited_pair() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let identity = fixture_identity("alice");
    let cred = sfu.issue_turn_credentials(&identity).expect("cred issued");
    assert!(cred.expires_at > chrono::Utc::now());
    assert!(cred
        .username
        .as_str()
        .contains("alice@waddle.social/desktop"));
}

#[test]
fn unregister_revokes_every_jti_issued_to_the_participant() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("c-revoke").unwrap();
    let alice = fixture_identity("alice");

    let t1 = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .unwrap();
    let t2 = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .unwrap();
    assert!(!sfu.is_revoked(&t1.jti));
    assert!(!sfu.is_revoked(&t2.jti));

    // Register + unregister: every previously-issued jti must
    // be revoked once the participant has left the call.
    sfu.register_call_participant(&call, &alice);
    sfu.unregister_call_participant(&call, &alice);

    assert!(sfu.is_revoked(&t1.jti));
    assert!(sfu.is_revoked(&t2.jti));
}

#[test]
fn revocation_is_scoped_per_participant() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("c-scope").unwrap();
    let alice = fixture_identity("alice");
    let bob = fixture_identity("bob");

    let alice_token = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .unwrap();
    let bob_token = sfu
        .issue_join_token(&call, &bob, MediaCapabilities::direct_call_peer())
        .unwrap();

    sfu.register_call_participant(&call, &alice);
    sfu.register_call_participant(&call, &bob);
    sfu.unregister_call_participant(&call, &alice);

    // Alice's hangup must not revoke bob's still-active token.
    assert!(sfu.is_revoked(&alice_token.jti));
    assert!(!sfu.is_revoked(&bob_token.jti));
}

#[test]
fn issued_jti_vec_is_capped_per_participant() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("c-cap").unwrap();
    let alice = fixture_identity("alice");

    // Mint well past the cap; every fresh token should slot in,
    // but the per-participant vec must never exceed it.
    for _ in 0..(MAX_ISSUED_PER_PARTICIPANT * 3) {
        sfu.issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
            .expect("token issued");
        assert!(
            sfu.issued_count(&call, &alice) <= MAX_ISSUED_PER_PARTICIPANT,
            "issued vec must stay <= MAX_ISSUED_PER_PARTICIPANT"
        );
    }
    assert_eq!(
        sfu.issued_count(&call, &alice),
        MAX_ISSUED_PER_PARTICIPANT,
        "issued vec must saturate exactly at the cap"
    );
}

#[test]
fn revoked_entries_are_swept_once_past_expiry() {
    use chrono::Duration as ChronoDuration;
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");

    // Seed the revoked map directly with a past-exp entry so
    // the test does not depend on real-time tickdown of the
    // token TTL.
    let stale_jti = Jti::new();
    let fresh_jti = Jti::new();
    sfu.revoked
        .insert(stale_jti.clone(), Utc::now() - ChronoDuration::seconds(60));
    sfu.revoked
        .insert(fresh_jti.clone(), Utc::now() + ChronoDuration::seconds(60));

    // Reading the stale jti must return false (the token can
    // no longer be replayed regardless) AND drop the entry.
    assert!(!sfu.is_revoked(&stale_jti));
    assert!(sfu.is_revoked(&fresh_jti));
    assert_eq!(sfu.revoked_count(), 1);

    // Running the unregister-path sweep clears any other stale
    // entries that piled up since the last sweep.
    sfu.revoked
        .insert(Jti::new(), Utc::now() - ChronoDuration::seconds(1));
    let alice = fixture_identity("alice");
    let call = CallId::new("c-sweep").unwrap();
    sfu.register_call_participant(&call, &alice);
    sfu.unregister_call_participant(&call, &alice);
    assert_eq!(
        sfu.revoked_count(),
        1,
        "unregister sweep must clear past-exp entries; one fresh entry should remain"
    );
}

#[test]
fn register_is_idempotent() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("c1").unwrap();
    let identity = fixture_identity("alice");

    sfu.register_call_participant(&call, &identity);
    sfu.register_call_participant(&call, &identity);
    assert_eq!(sfu.participant_count(&call), 1);
}

// -------- Admin-evict path (tokio runtime present) --------

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use crate::admin::LiveKitAdmin;

#[derive(Default)]
struct RecordingAdmin {
    remove_calls: Mutex<Vec<(CallId, Identity)>>,
    delete_calls: Mutex<Vec<CallId>>,
    update_calls: Mutex<Vec<(CallId, Identity, MediaCapabilities)>>,
    /// Per-call artificial latency for `update_participant`,
    /// consumed in call order. Ordering tests queue a SLOW first
    /// delay and a FAST second one so that, without per-key
    /// serialization, the older push would complete last.
    update_delays: Mutex<std::collections::VecDeque<StdDuration>>,
    /// What LiveKit "reports" as connected per call. A call absent
    /// from the map lists as empty (room not found). Drives the
    /// reconciliation tests.
    live: Mutex<std::collections::HashMap<CallId, Vec<Identity>>>,
    /// When set, `list_participant_identities` errors instead of
    /// returning a set — used to assert reconcile skips a call it
    /// can't confirm rather than sweeping it.
    list_errors: Mutex<bool>,
}

impl RecordingAdmin {
    fn remove_snapshot(&self) -> Vec<(CallId, Identity)> {
        self.remove_calls.lock().expect("recording lock").clone()
    }

    fn delete_snapshot(&self) -> Vec<CallId> {
        self.delete_calls.lock().expect("recording lock").clone()
    }

    fn update_snapshot(&self) -> Vec<(CallId, Identity, MediaCapabilities)> {
        self.update_calls.lock().expect("recording lock").clone()
    }

    fn queue_update_delays(&self, delays: impl IntoIterator<Item = StdDuration>) {
        *self.update_delays.lock().expect("recording lock") = delays.into_iter().collect();
    }

    fn set_live(&self, call: &CallId, identities: Vec<Identity>) {
        self.live
            .lock()
            .expect("recording lock")
            .insert(call.clone(), identities);
    }

    fn fail_list(&self) {
        self.set_list_failing(true);
    }

    fn set_list_failing(&self, failing: bool) {
        *self.list_errors.lock().expect("recording lock") = failing;
    }
}

impl LiveKitAdmin for RecordingAdmin {
    fn remove_participant<'a>(
        &'a self,
        room: &'a CallId,
        identity: &'a Identity,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        let room = room.clone();
        let identity = identity.clone();
        Box::pin(async move {
            self.remove_calls
                .lock()
                .expect("recording lock")
                .push((room, identity));
            Ok(())
        })
    }

    fn delete_room<'a>(
        &'a self,
        room: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        let room = room.clone();
        Box::pin(async move {
            self.delete_calls.lock().expect("recording lock").push(room);
            Ok(())
        })
    }

    fn update_participant<'a>(
        &'a self,
        room: &'a CallId,
        identity: &'a Identity,
        capabilities: MediaCapabilities,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        let room = room.clone();
        let identity = identity.clone();
        let delay = self
            .update_delays
            .lock()
            .expect("recording lock")
            .pop_front();
        Box::pin(async move {
            // Honour the configured latency so ordering tests
            // produce genuinely overlapping in-flight requests.
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            self.update_calls
                .lock()
                .expect("recording lock")
                .push((room, identity, capabilities));
            Ok(())
        })
    }

    fn list_participant_identities<'a>(
        &'a self,
        room: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Identity>, SfuError>> + Send + 'a>> {
        let room = room.clone();
        Box::pin(async move {
            if *self.list_errors.lock().expect("recording lock") {
                return Err(SfuError::InvalidCallId("simulated list failure".into()));
            }
            Ok(self
                .live
                .lock()
                .expect("recording lock")
                .get(&room)
                .cloned()
                .unwrap_or_default())
        })
    }
}

/// Yield enough times for any spawned admin task on the current
/// runtime to make progress. The spawned future does a couple of
/// `Mutex` operations and returns, so two yields are more than
/// sufficient; tighten or loosen if `RecordingAdmin` grows steps.
async fn drain_admin_tasks() {
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn update_capabilities_pushes_permission_for_registered_participant() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-grants").unwrap();
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);

    let caps = MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted);
    sfu.update_participant_capabilities(&call, &alice, caps);
    drain_admin_tasks().await;

    let updates = admin.update_snapshot();
    assert_eq!(updates.len(), 1, "UpdateParticipant fires exactly once");
    assert_eq!(&updates[0].0, &call);
    assert_eq!(
        updates[0].1.as_livekit_identity(),
        alice.as_livekit_identity()
    );
    assert_eq!(updates[0].2, caps);
    assert!(
        sfu.has_call_participant(&call, &alice),
        "a grant update must not unregister the participant"
    );
    assert!(
        admin.remove_snapshot().is_empty(),
        "a grant update must not evict"
    );
}

/// A downgrade must NOT be gated on local registration. Our
/// per-process registry can legitimately have lost a participant
/// LiveKit still holds (reconnect after `participant_left`, room
/// actor migrated between cluster nodes, reconcile sweep), and
/// skipping the push for them would let a de-voiced occupant keep
/// publishing — a fail-open in the one direction that must never
/// fail open. Mirrors `unregister_call_participant`'s
/// always-run `RemoveParticipant`.
#[tokio::test]
async fn downgrade_pushes_even_when_the_local_registry_lost_the_participant() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-grants-ghost").unwrap();
    let alice = fixture_identity("alice");
    assert!(
        !sfu.has_call_participant(&call, &alice),
        "fixture models a participant absent from the local registry"
    );

    sfu.update_participant_capabilities(
        &call,
        &alice,
        MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted),
    );
    drain_admin_tasks().await;

    let updates = admin.update_snapshot();
    assert_eq!(
        updates.len(),
        1,
        "the downgrade must still reach LiveKit: {updates:?}"
    );
    assert!(updates[0].2.is_listen_only());
}

#[tokio::test]
async fn downgrade_to_listen_only_revokes_outstanding_tokens_but_keeps_participant() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-demote").unwrap();
    let alice = fixture_identity("alice");
    let token = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .expect("token");
    sfu.register_call_participant(&call, &alice);

    sfu.update_participant_capabilities(
        &call,
        &alice,
        MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted),
    );
    drain_admin_tasks().await;

    assert!(
        sfu.is_revoked(&token.jti),
        "a not-yet-used pre-demotion token must not be replayable with stale publish rights"
    );
    assert_eq!(sfu.issued_count(&call, &alice), 0);
    assert!(
        sfu.has_call_participant(&call, &alice),
        "the demoted participant stays in the call as a listener"
    );
    assert_eq!(admin.update_snapshot().len(), 1);
}

#[tokio::test]
async fn upgrade_to_voice_does_not_revoke_tokens() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-promote").unwrap();
    let alice = fixture_identity("alice");
    let token = sfu
        .issue_join_token(
            &call,
            &alice,
            MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted),
        )
        .expect("token");
    sfu.register_call_participant(&call, &alice);

    sfu.update_participant_capabilities(
        &call,
        &alice,
        MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Voiced),
    );
    drain_admin_tasks().await;

    assert!(
        !sfu.is_revoked(&token.jti),
        "a promotion widens grants; existing tokens stay valid"
    );
    assert_eq!(admin.update_snapshot().len(), 1);
    assert!(admin.update_snapshot()[0].2.can_publish);
}

/// A slow push must never overwrite a newer one's grants. The
/// first update is delayed inside the admin client so, without
/// per-key serialization, its (publish-enabling) request would be
/// in flight when the newer demotion is issued and could land
/// last. Only the newest grants may ever be the final state.
#[tokio::test]
async fn a_superseded_grant_push_never_lands_after_a_newer_one() {
    let admin = Arc::new(RecordingAdmin::default());
    // Make every UpdateParticipant take a real await point so the
    // two pushes genuinely overlap in time.
    // The FIRST push is slow, the second fast: without per-key
    // serialization the stale promotion would therefore complete
    // AFTER the newer revoke, which is precisely the failure this
    // test must catch.
    admin.queue_update_delays([StdDuration::from_millis(300), StdDuration::from_millis(10)]);
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-order").unwrap();
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);

    let voiced = MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Voiced);
    let muted = MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted);

    // Stale promotion, then — after it is already spawned and
    // (thanks to the delay) plausibly in flight — the demotion
    // that supersedes it.
    sfu.update_participant_capabilities(&call, &alice, voiced);
    tokio::task::yield_now().await;
    sfu.update_participant_capabilities(&call, &alice, muted);

    tokio::time::sleep(StdDuration::from_millis(400)).await;

    let updates = admin.update_snapshot();
    assert!(
        !updates.is_empty(),
        "the newest grants must reach LiveKit at least once"
    );
    assert_eq!(
        updates.last().expect("non-empty").2,
        muted,
        "the FINAL state on LiveKit must be the newest grants: {updates:?}"
    );
    // The safety property: once a revoke has been applied, no
    // publish-enabling push may land afterwards. A promotion
    // already in flight before the revoke existed is fine; one
    // arriving after it is the failure mode.
    let first_revoke = updates
        .iter()
        .position(|(_, _, caps)| caps.is_listen_only())
        .expect("the revoke reached LiveKit");
    assert!(
        updates[first_revoke..]
            .iter()
            .all(|(_, _, caps)| caps.is_listen_only()),
        "no publish-enabling push may land after a revoke: {updates:?}"
    );
}

/// The per-key lock must not wedge later pushes for the same
/// participant: a second, independent change after the first has
/// settled still converges.
#[tokio::test]
async fn sequential_grant_pushes_each_reach_livekit() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-sequential").unwrap();
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);

    let muted = MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Muted);
    let voiced = MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Voiced);

    sfu.update_participant_capabilities(&call, &alice, muted);
    drain_admin_tasks().await;
    sfu.update_participant_capabilities(&call, &alice, voiced);
    drain_admin_tasks().await;

    let updates = admin.update_snapshot();
    assert_eq!(updates.len(), 2, "both settled changes push: {updates:?}");
    assert_eq!(updates[0].2, muted);
    assert_eq!(updates[1].2, voiced);
    // The per-key side tables must not leak once every push has
    // drained — grant pushes are deliberately ungated on local
    // registration, so these keys never reach `clear_local_state`.
    assert_eq!(
        sfu.grant_locks.len(),
        0,
        "grant locks must be reaped by the last task to release them"
    );
    assert_eq!(sfu.desired_grants.len(), 0, "desired intents are consumed");
}

/// `live_participants` must answer from LiveKit, not from the
/// process-local registry. The voice-reconciliation backstop runs on
/// whichever node claims a room, which is frequently NOT the node
/// that registered the participant — resolving from the local
/// registry there skips the participant and fails open.
#[tokio::test]
async fn live_participants_reports_livekit_truth_not_the_local_registry() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-authoritative").unwrap();
    let alice = fixture_identity("alice");
    // LiveKit holds alice; this process's registry does not (the
    // cross-node case).
    admin.set_live(&call, vec![alice.clone()]);
    assert!(
        !sfu.has_call_participant(&call, &alice),
        "fixture models an empty local registry"
    );

    let live = crate::SfuReconciler::live_participants(&sfu, &call)
        .await
        .expect("LiveKit reachable");
    assert_eq!(
        live.iter()
            .map(|identity| identity.as_livekit_identity())
            .collect::<Vec<_>>(),
        vec![alice.as_livekit_identity()],
        "must report the SFU's participants, not the local registry's"
    );
}

/// A `ListParticipants` failure must report `None`, not an empty
/// vec: an outage must never be indistinguishable from "nobody is
/// connected", or it silently disables the caller's convergence.
#[tokio::test]
async fn live_participants_reports_unknown_when_livekit_cannot_be_reached() {
    let admin = Arc::new(RecordingAdmin::default());
    admin.fail_list();
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-list-fails").unwrap();

    assert!(
        crate::SfuReconciler::live_participants(&sfu, &call)
            .await
            .is_none(),
        "an unreachable SFU must be reported as unknown, not as empty"
    );
}

#[tokio::test]
async fn unregister_schedules_remove_participant_on_the_admin_client() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-evict").unwrap();
    let alice = fixture_identity("alice");
    let bob = fixture_identity("bob");

    sfu.register_call_participant(&call, &alice);
    sfu.register_call_participant(&call, &bob);

    // Alice leaves: RemoveParticipant must fire; DeleteRoom must
    // NOT fire because bob is still in the call.
    let state = sfu.unregister_call_participant(&call, &alice);
    assert!(matches!(state, CallState::Active { remaining: 1 }));
    drain_admin_tasks().await;

    let removes = admin.remove_snapshot();
    assert_eq!(
        removes.len(),
        1,
        "RemoveParticipant should fire exactly once"
    );
    assert_eq!(&removes[0].0, &call);
    assert_eq!(
        removes[0].1.as_livekit_identity(),
        alice.as_livekit_identity()
    );
    assert!(
        admin.delete_snapshot().is_empty(),
        "DeleteRoom must not fire while the call still has participants"
    );
}

#[tokio::test]
async fn unregister_last_participant_also_schedules_delete_room() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-empty").unwrap();
    let alice = fixture_identity("alice");

    sfu.register_call_participant(&call, &alice);
    let state = sfu.unregister_call_participant(&call, &alice);
    assert_eq!(state, CallState::Ended);
    drain_admin_tasks().await;

    let deletes = admin.delete_snapshot();
    assert_eq!(deletes.len(), 1, "DeleteRoom must fire on last participant");
    assert_eq!(&deletes[0], &call);

    let removes = admin.remove_snapshot();
    assert_eq!(
        removes.len(),
        1,
        "RemoveParticipant still fires for the last leaver"
    );
    assert_eq!(&removes[0].0, &call);
}

#[tokio::test]
async fn unregister_of_unknown_identity_fires_remove_participant_but_not_delete_room() {
    // Edge case: a session-terminate arrives without a matching
    // register (e.g. server-side state was lost, a client races
    // a re-init, a replayed terminate from a long-dead session).
    // `RemoveParticipant` must still fire because LiveKit may
    // hold the participant via a separate path. `DeleteRoom`
    // MUST NOT fire — we don't know the call's true state, and
    // tearing it down could evict participants we never tracked.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-ghost").unwrap();
    let ghost = fixture_identity("mallory");

    let state = sfu.unregister_call_participant(&call, &ghost);
    assert!(
        matches!(state, CallState::Active { remaining: 0 }),
        "ghost unregister must NOT report CallState::Ended; got {state:?}",
    );
    drain_admin_tasks().await;

    let removes = admin.remove_snapshot();
    assert_eq!(removes.len(), 1);
    assert_eq!(
        removes[0].1.as_livekit_identity(),
        ghost.as_livekit_identity()
    );
    assert!(
        admin.delete_snapshot().is_empty(),
        "DeleteRoom must not fire when we never tracked the participant",
    );
}

#[tokio::test]
async fn note_participant_left_clears_local_state_without_admin_call() {
    // The LiveKit webhook bridge calls this path when LiveKit's
    // `participant_left` fires. Doing a back-channel admin
    // RemoveParticipant here would amplify the wire traffic (LK
    // would 404 our redundant call) and racily kick fresh
    // rejoiners. The trait contract forbids it; assert the
    // production impl honours it.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-webhook").unwrap();
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);

    sfu.note_participant_left(&call, &alice);
    drain_admin_tasks().await;

    assert_eq!(sfu.participant_count(&call), 0, "registry must be cleared");
    assert!(
        admin.remove_snapshot().is_empty(),
        "note_participant_left must NOT spawn RemoveParticipant",
    );
    assert!(
        admin.delete_snapshot().is_empty(),
        "note_participant_left must NOT spawn DeleteRoom",
    );
}

#[tokio::test]
async fn last_participant_delete_room_skipped_when_someone_rejoins() {
    // Race: Alice hangs up (clearing local state + scheduling
    // teardown), Bob joins the same MUC call before the spawn
    // gets to its DeleteRoom step. The re-check inside the
    // spawn must observe Bob's registration and suppress
    // DeleteRoom so Bob's session is not evicted. We simulate
    // the rejoin by registering Bob immediately after Alice's
    // unregister returns, before yielding to the spawn.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-rejoin").unwrap();
    let alice = fixture_identity("alice");
    let bob = fixture_identity("bob");

    sfu.register_call_participant(&call, &alice);
    let state = sfu.unregister_call_participant(&call, &alice);
    assert_eq!(state, CallState::Ended);

    // Bob rejoins before the spawned future polls. With a single-
    // threaded current-thread runtime this synchronous register
    // is guaranteed to land before any `yield_now`-scheduled
    // continuation observes the registry.
    sfu.register_call_participant(&call, &bob);

    drain_admin_tasks().await;

    let removes = admin.remove_snapshot();
    assert_eq!(
        removes.len(),
        1,
        "RemoveParticipant for Alice must still fire"
    );
    assert!(
        admin.delete_snapshot().is_empty(),
        "DeleteRoom must be suppressed by the rejoin re-check; got {:?}",
        admin.delete_snapshot(),
    );
}

// -------- Reconciliation backstop --------

use crate::SfuReconciler;

#[tokio::test]
async fn reconcile_sweeps_ghost_absent_from_livekit() {
    // Alice + Bob registered; LiveKit reports only Alice connected
    // (Bob's participant_left webhook was lost). With a zero grace
    // window Bob must be swept — after TWO consecutive absent
    // passes (#1127) — and returned for presence cleanup; Alice
    // must remain. No admin remove/delete is fired — the ghost is
    // already gone from LiveKit.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("general@muc.waddle.social").unwrap();
    let alice = fixture_identity("alice");
    let bob = fixture_identity("bob");
    sfu.register_call_participant(&call, &alice);
    sfu.register_call_participant(&call, &bob);
    admin.set_live(&call, vec![alice.clone()]);

    let first_pass = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert!(
        first_pass.is_empty(),
        "one absent observation must not sweep (#1127): {first_pass:?}"
    );
    assert!(
        sfu.has_call_participant(&call, &bob),
        "Bob must survive the first absent pass"
    );

    let swept = sfu.reconcile_active_calls(ChronoDuration::zero()).await;

    assert_eq!(swept, vec![(call.clone(), bob.clone())]);
    assert!(sfu.has_call_participant(&call, &alice), "Alice must remain");
    assert!(
        !sfu.has_call_participant(&call, &bob),
        "Bob must be swept from the registry"
    );
    assert_eq!(sfu.participant_count(&call), 1);
    assert!(
        admin.remove_snapshot().is_empty() && admin.delete_snapshot().is_empty(),
        "reconcile must not fire admin RemoveParticipant/DeleteRoom for already-gone ghosts"
    );
}

#[tokio::test]
async fn reconcile_respects_registration_grace_window() {
    // A just-registered participant LiveKit hasn't seen yet (still
    // ringing/connecting) must NOT be swept while inside the grace
    // window — sweeping here would tear down a call coming up.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("room@muc.waddle.social").unwrap();
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);
    // LiveKit reports nobody (room not yet created / mid-connect).
    admin.set_live(&call, vec![]);

    let swept = sfu
        .reconcile_active_calls(ChronoDuration::seconds(3600))
        .await;

    assert!(
        swept.is_empty(),
        "a participant inside the grace window must not be swept"
    );
    assert_eq!(sfu.participant_count(&call), 1);
}

#[tokio::test]
async fn reconcile_keeps_genuinely_connected_participants() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("room2@muc.waddle.social").unwrap();
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);
    admin.set_live(&call, vec![alice.clone()]);

    let swept = sfu.reconcile_active_calls(ChronoDuration::zero()).await;

    assert!(swept.is_empty(), "connected participant must not be swept");
    assert!(sfu.has_call_participant(&call, &alice));
}

#[tokio::test]
async fn reconcile_skips_calls_it_cannot_confirm() {
    // If ListParticipants fails for a call, absence cannot be
    // confirmed; nothing is swept and the next pass retries.
    let admin = Arc::new(RecordingAdmin::default());
    admin.fail_list();
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("room3@muc.waddle.social").unwrap();
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);

    let swept = sfu.reconcile_active_calls(ChronoDuration::zero()).await;

    assert!(
        swept.is_empty(),
        "a call whose participant list could not be fetched must not be swept"
    );
    assert_eq!(sfu.participant_count(&call), 1);
}

#[tokio::test]
async fn reconcile_livekit_restart_does_not_mass_terminate_live_calls() {
    // #1127: a LiveKit pod restart makes one pass report every
    // room as not-found (empty participant list). Clients silently
    // rejoin before the next pass. Nothing may be swept.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call_a = CallId::new("standup@muc.waddle.social").unwrap();
    let call_b = CallId::new("alice@waddle.social::dm-1").unwrap();
    let alice = fixture_identity("alice");
    let bob = fixture_identity("bob");
    sfu.register_call_participant(&call_a, &alice);
    sfu.register_call_participant(&call_b, &bob);

    // Pass 1: restart — LiveKit knows no rooms (both list empty).
    let pass1 = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert!(pass1.is_empty(), "restart pass must not sweep: {pass1:?}");
    assert_eq!(sfu.participant_count(&call_a), 1);
    assert_eq!(sfu.participant_count(&call_b), 1);

    // Clients reconnected before pass 2.
    admin.set_live(&call_a, vec![alice.clone()]);
    admin.set_live(&call_b, vec![bob.clone()]);
    let pass2 = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert!(pass2.is_empty(), "reconnected clients must not be swept");

    // Pass 3: streaks were reset by the connected observation, so
    // a later single absent blip still does not sweep.
    admin.set_live(&call_a, vec![]);
    let pass3 = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert!(
        pass3.is_empty(),
        "streak must have been reset by the connected pass: {pass3:?}"
    );
    assert!(sfu.has_call_participant(&call_a, &alice));
}

#[tokio::test]
async fn reconcile_failed_pass_resets_absence_streak() {
    // #1127 AC: the absence tracker resets on a failed pass — two
    // absent observations separated by a ListParticipants failure
    // are not "consecutive".
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("room@muc.waddle.social").unwrap();
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);
    admin.set_live(&call, vec![]);

    // Absent pass 1 → streak 1.
    assert!(sfu
        .reconcile_active_calls(ChronoDuration::zero())
        .await
        .is_empty());
    // Failed pass → streak reset.
    admin.set_list_failing(true);
    assert!(sfu
        .reconcile_active_calls(ChronoDuration::zero())
        .await
        .is_empty());
    admin.set_list_failing(false);
    // Absent pass again → streak restarts at 1, still no sweep.
    let third = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert!(
        third.is_empty(),
        "failed pass must reset the streak: {third:?}"
    );
    assert_eq!(sfu.participant_count(&call), 1);
    // Second CONSECUTIVE absent pass → swept.
    let fourth = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert_eq!(fourth, vec![(call.clone(), alice)]);
    assert_eq!(sfu.participant_count(&call), 0);
}

#[tokio::test]
async fn reconcile_streak_resets_on_reregistration() {
    // A participant re-registering (fresh session-initiate /
    // rejoin) invalidates absence observed against the previous
    // attempt.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("room@muc.waddle.social").unwrap();
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);
    admin.set_live(&call, vec![]);

    assert!(sfu
        .reconcile_active_calls(ChronoDuration::zero())
        .await
        .is_empty());
    // Rejoin between passes.
    sfu.register_call_participant(&call, &alice);
    // This absent pass is the FIRST of the new registration.
    assert!(
        sfu.reconcile_active_calls(ChronoDuration::zero())
            .await
            .is_empty(),
        "re-registration must reset the absence streak"
    );
    assert_eq!(sfu.participant_count(&call), 1);
}

// -------- #1129 teardown/join race --------

#[test]
fn concurrent_join_during_teardown_is_never_clobbered() {
    // #1129: `clear_local_state` used to compute `remaining == 0`
    // under the entry guard, drop it, then unconditionally remove
    // the call entry — deleting a joiner who registered in the
    // window. The atomic `remove_if` closes that: after BOTH an
    // unregister(alice) and a register(bob) have completed, bob
    // must always be present in the registry, whatever the
    // interleaving. Run many racing iterations to exercise the
    // window.
    let sfu =
        Arc::new(LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test (no runtime)"));
    let alice = fixture_identity("alice");
    let bob = fixture_identity("bob");

    for i in 0..200 {
        let call = CallId::new(format!("race-{i}")).unwrap();
        sfu.register_call_participant(&call, &alice);

        let leaver = {
            let sfu = Arc::clone(&sfu);
            let call = call.clone();
            let alice = alice.clone();
            std::thread::spawn(move || {
                let _ = sfu.unregister_call_participant(&call, &alice);
            })
        };
        sfu.register_call_participant(&call, &bob);
        leaver.join().expect("leaver thread");

        assert!(
            sfu.has_call_participant(&call, &bob),
            "iteration {i}: concurrent joiner was clobbered by teardown (#1129)"
        );
    }
}

#[tokio::test]
async fn delete_room_not_fired_when_joiner_lands_before_conditional_remove() {
    // #1129 second half: when the joiner wins the race, the
    // unregister must report the call as still active (not Ended)
    // so no DeleteRoom is scheduled against the fresh joiner.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-joiner-race").unwrap();
    let alice = fixture_identity("alice");
    let bob = fixture_identity("bob");

    sfu.register_call_participant(&call, &alice);
    // Simulate the joiner landing inside alice's teardown window:
    // remove alice from the set (step 1 of clear_local_state),
    // register bob, then run the full unregister — the conditional
    // removal must observe bob and keep the entry.
    sfu.calls
        .get_mut(&call)
        .expect("entry exists")
        .remove(&alice);
    sfu.register_call_participant(&call, &bob);

    let state = sfu.unregister_call_participant(&call, &alice);
    assert!(
        matches!(state, CallState::Active { remaining: 1 }),
        "joiner present at conditional-remove time must keep the call active; got {state:?}"
    );
    drain_admin_tasks().await;
    assert!(
        admin.delete_snapshot().is_empty(),
        "DeleteRoom must not fire while the fresh joiner is registered"
    );
    assert!(sfu.has_call_participant(&call, &bob));
}
