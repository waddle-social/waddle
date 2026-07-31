//! Unit tests for the [`super::LiveKitSfu`] bridge.
//!
//! A child module, so it keeps access to the parent's private state
//! (`grant_locks`, `desired_grants`, `issued`) that several invariants
//! here assert on directly.

use super::*;
use crate::config::{ApiKey, ApiSecret, TurnSharedSecret};
use chrono::Duration;
use jid::FullJid;
use std::sync::atomic::{AtomicUsize, Ordering};
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

fn applied_state(disposition: TeardownDisposition) -> CallState {
    match disposition {
        TeardownDisposition::Applied(state) => state,
        TeardownDisposition::StaleSid => panic!("expected applied teardown, got stale sid"),
    }
}

fn fixture_room_sid(value: &str) -> RoomSid {
    RoomSid::new(value).expect("valid room sid")
}

fn fixture_participant_sid(value: &str) -> ParticipantSid {
    ParticipantSid::new(value).expect("valid participant sid")
}

fn observed_sids(room_sid: Option<&str>, participant_sid: Option<&str>) -> ObservedCallSids {
    ObservedCallSids::new(
        room_sid.map(fixture_room_sid),
        participant_sid.map(fixture_participant_sid),
    )
}

#[test]
fn observed_join_registers_after_restart_and_defers_conflicting_room_sid() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("test SFU");
    let call = CallId::new("restart@muc.waddle.social").expect("call id");
    let alice = fixture_identity("alice");
    let current = observed_sids(Some("RM_current"), Some("PA_current"));

    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &current),
        SidObservationDisposition::Applied
    );
    assert!(sfu.has_call_participant(&call, &alice));
    assert_eq!(stored_room_sid(&sfu, &call), current.room_sid);
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        current.participant_sid
    );

    let bob = fixture_identity("bob");
    let stale = observed_sids(Some("RM_old"), Some("PA_old"));
    assert_eq!(
        sfu.register_call_participant_observed(&call, &bob, &stale),
        SidObservationDisposition::RoomRotationPending
    );
    assert!(
        !sfu.has_call_participant(&call, &bob),
        "a join from an old room incarnation must not be registered"
    );
    assert_eq!(sfu.participant_count(&call), 1);
}

fn stored_generation(sfu: &LiveKitSfu, call_id: &CallId) -> Option<CallGeneration> {
    sfu.calls
        .get(call_id)
        .map(|entry| entry.generation)
        .or_else(|| {
            sfu.call_generations
                .get(call_id)
                .and_then(|entry| entry.current_generation())
        })
}

fn stored_generation_tombstone_cleared_at(
    sfu: &LiveKitSfu,
    call_id: &CallId,
) -> Option<DateTime<Utc>> {
    sfu.call_generations
        .get(call_id)
        .and_then(|entry| entry.last_cleared_at)
}

fn stored_room_sid(sfu: &LiveKitSfu, call_id: &CallId) -> Option<RoomSid> {
    sfu.calls
        .get(call_id)
        .and_then(|entry| entry.room_sid.clone())
}

fn stored_participant_sid(
    sfu: &LiveKitSfu,
    call_id: &CallId,
    identity: &Identity,
) -> Option<ParticipantSid> {
    sfu.calls.get(call_id).and_then(|entry| {
        entry
            .participants
            .get(identity)
            .and_then(|state| state.participant_sid.clone())
    })
}

#[test]
fn participant_registered_at_keeps_the_first_current_registration_timestamp() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("test SFU");
    let call = CallId::new("c-first-registered-at").expect("call id");
    let alice = fixture_identity("alice");
    let observed = observed_sids(Some("RM_current"), Some("PA_current"));
    let key = (call.clone(), alice.clone());

    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &observed),
        SidObservationDisposition::Applied
    );
    let first_registered_at = sfu
        .participant_registered_at(&call, &alice)
        .expect("participant registration timestamp");
    sfu.registered_at.insert(
        key.clone(),
        first_registered_at - chrono::Duration::seconds(60),
    );

    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &observed),
        SidObservationDisposition::Applied
    );

    let refreshed_registered_at = *sfu
        .registered_at
        .get(&key)
        .expect("refreshed grace timestamp")
        .value();
    assert!(
        refreshed_registered_at > first_registered_at - chrono::Duration::seconds(60),
        "repeat sighting must refresh the grace timestamp"
    );
    assert_eq!(
        sfu.participant_registered_at(&call, &alice),
        Some(first_registered_at),
        "supersession fencing must keep the first absent->present timestamp"
    );
    assert_eq!(
        sfu.calls
            .get(&call)
            .expect("call entry")
            .participants
            .get(&alice)
            .expect("participant state")
            .first_registered_at,
        first_registered_at
    );
}

#[test]
fn participant_sid_advance_on_join_restamps_the_current_registration() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("test SFU");
    let call = CallId::new("c-participant-sid-advance").expect("call id");
    let alice = fixture_identity("alice");
    let original = observed_sids(Some("RM_current"), Some("PA_original"));
    let rejoined = observed_sids(Some("RM_current"), Some("PA_rejoined"));

    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &original),
        SidObservationDisposition::Applied
    );
    let first_registered_at = sfu
        .participant_registered_at(&call, &alice)
        .expect("first registration timestamp");

    std::thread::sleep(std::time::Duration::from_millis(20));

    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&rejoined),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );

    let restamped_registered_at = sfu
        .participant_registered_at(&call, &alice)
        .expect("restamped registration timestamp");
    assert!(
        restamped_registered_at > first_registered_at,
        "a new participant sid must start a new current-registration fence"
    );
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        rejoined.participant_sid
    );
    assert!(
        sfu.calls
            .get(&call)
            .expect("call entry")
            .participants
            .get(&alice)
            .expect("participant state")
            .registered_without_mint,
        "a rejoined participant stays pending until the first local mint of the new incarnation"
    );
}

#[test]
fn delayed_old_leave_with_a_superseded_participant_sid_is_stale() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("test SFU");
    let call = CallId::new("c-old-leave-stale").expect("call id");
    let alice = fixture_identity("alice");
    let original = observed_sids(Some("RM_current"), Some("PA_original"));
    let rejoined = observed_sids(Some("RM_current"), Some("PA_rejoined"));

    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &original),
        SidObservationDisposition::Applied
    );
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&rejoined),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );

    assert_eq!(
        sfu.note_participant_left(&call, &alice, Some(&original)),
        TeardownDisposition::StaleSid
    );
    assert!(sfu.has_call_participant(&call, &alice));
    assert_eq!(sfu.participant_count(&call), 1);
}

#[test]
fn leave_from_a_different_room_incarnation_remains_stale() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("test SFU");
    let call = CallId::new("c-old-room-leave-stale").expect("call id");
    let alice = fixture_identity("alice");
    let current = observed_sids(Some("RM_current"), Some("PA_current"));
    let old_room = observed_sids(Some("RM_old"), Some("PA_old"));

    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &current),
        SidObservationDisposition::Applied
    );
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&old_room),
            SidObservationDirection::Leave,
        ),
        SidObservationDisposition::StaleSid
    );
    assert_eq!(stored_room_sid(&sfu, &call), current.room_sid);
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        current.participant_sid
    );
}

#[test]
fn current_leave_with_the_latest_participant_sid_tears_down() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("test SFU");
    let call = CallId::new("c-current-leave").expect("call id");
    let alice = fixture_identity("alice");
    let original = observed_sids(Some("RM_current"), Some("PA_original"));
    let rejoined = observed_sids(Some("RM_current"), Some("PA_rejoined"));

    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &original),
        SidObservationDisposition::Applied
    );
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&rejoined),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );

    assert_eq!(
        sfu.note_participant_left(&call, &alice, Some(&rejoined)),
        TeardownDisposition::Applied(CallState::Ended)
    );
    assert_eq!(sfu.participant_count(&call), 0);
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

    match applied_state(sfu.unregister_call_participant(&call, &a, None)) {
        CallState::Active { remaining } => assert_eq!(remaining, 1),
        CallState::Ended => panic!("call should still be active"),
    }

    match applied_state(sfu.unregister_call_participant(&call, &b, None)) {
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
fn participant_last_minted_at_tracks_the_current_registration() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("test SFU");
    let call = CallId::new("c-last-minted-at").expect("call id");
    let alice = fixture_identity("alice");
    let observed = observed_sids(Some("RM_observed"), Some("PA_observed"));

    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &observed),
        SidObservationDisposition::Applied
    );
    assert_eq!(sfu.participant_last_minted_at(&call, &alice), None);

    let token = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .expect("token issued");

    assert!(
        sfu.participant_last_minted_at(&call, &alice)
            .is_some_and(|minted_at| minted_at <= token.expires_at),
        "the last-minted timestamp must be recorded for the current registration"
    );
    assert!(
        !sfu.calls
            .get(&call)
            .expect("call entry")
            .participants
            .get(&alice)
            .expect("participant state")
            .registered_without_mint,
        "the first local mint clears the pending restored-without-mint marker"
    );

    assert_eq!(
        sfu.note_participant_left(&call, &alice, Some(&observed)),
        TeardownDisposition::Applied(CallState::Ended)
    );
    assert_eq!(sfu.participant_last_minted_at(&call, &alice), None);
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
    let _ = sfu.unregister_call_participant(&call, &alice, None);

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
    let _ = sfu.unregister_call_participant(&call, &alice, None);

    // Alice's hangup must not revoke bob's still-active token.
    assert!(sfu.is_revoked(&alice_token.jti));
    assert!(!sfu.is_revoked(&bob_token.jti));
}

#[test]
fn revoke_issued_token_ignores_a_jti_the_server_never_minted() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("c-unknown-jti").unwrap();
    let alice = fixture_identity("alice");

    let minted = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .unwrap();

    // A jti from an unverified claim in a crafted stanza must not
    // grow the revocation set — only provably minted issuances are
    // recorded (see the SfuService::revoke_issued_token contract).
    let forged = Jti::new();
    sfu.revoke_issued_token(&call, &alice, &forged);

    assert!(!sfu.is_revoked(&forged));
    assert_eq!(sfu.revoked_count(), 0);
    assert!(!sfu.is_revoked(&minted.jti));
    assert_eq!(sfu.issued_count(&call, &alice), 1);
}

#[test]
fn revoke_issued_token_drops_the_emptied_issued_bucket() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("c-empty-bucket").unwrap();
    let alice = fixture_identity("alice");

    let minted = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .unwrap();
    sfu.revoke_issued_token(&call, &alice, &minted.jti);

    assert!(sfu.is_revoked(&minted.jti));
    // The common mint-then-immediately-revoke bounce case must not
    // leave an empty per-(call, identity) bucket behind.
    assert!(
        !sfu.issued.contains_key(&(call.clone(), alice.clone())),
        "emptied issued bucket must be removed"
    );
    assert!(
        !sfu.last_minted_at
            .contains_key(&(call.clone(), alice.clone())),
        "emptying the issued bucket must also drop the last-minted tracker entry"
    );

    // A pair with another live issuance keeps its bucket.
    let t1 = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .unwrap();
    let t2 = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .unwrap();
    sfu.revoke_issued_token(&call, &alice, &t2.jti);
    assert!(!sfu.is_revoked(&t1.jti));
    assert_eq!(sfu.issued_count(&call, &alice), 1);
}

#[test]
fn revoke_issued_token_reports_a_guarded_eject_intent() {
    let admin = Arc::new(RecordingAdmin::default());
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink_values = Arc::clone(&reported);
    let sfu = LiveKitSfu::with_admin(fixture_config(), admin).with_teardown_failure_sink(Arc::new(
        move |intent| {
            sink_values.lock().expect("sink lock").push(intent);
            Box::pin(async {})
        },
    ));
    let call = CallId::new("c-bounce-eject").expect("call id");
    let alice = fixture_identity("alice");
    let observed = observed_sids(Some("RM_revoked"), Some("PA_revoked"));
    sfu.register_call_participant(&call, &alice);
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&observed),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );
    let minted = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .expect("token");

    sfu.revoke_issued_token(&call, &alice, &minted.jti);

    assert!(sfu.is_revoked(&minted.jti));
    assert!(
        sfu.has_call_participant(&call, &alice),
        "targeted rollback revokes one issuance, not the registry entry"
    );
    let intents = reported.lock().expect("sink lock");
    assert_eq!(intents.len(), 1, "only RemoveParticipant should be queued");
    assert_eq!(intents[0].call_id, call);
    assert_eq!(intents[0].generation, stored_generation(&sfu, &call));
    assert_eq!(intents[0].room_sid, observed.room_sid);
    match &intents[0].target {
        TeardownTargetLite::Participant {
            identity,
            participant_sid,
        } => {
            assert_eq!(identity, &alice);
            assert_eq!(participant_sid, &observed.participant_sid);
        }
        TeardownTargetLite::Room => panic!("targeted rollback must not enqueue DeleteRoom"),
    }
}

#[test]
fn targeted_revocation_requeues_eject_when_the_holder_joins_late() {
    let admin = Arc::new(RecordingAdmin::default());
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink_values = Arc::clone(&reported);
    let sfu = LiveKitSfu::with_admin(fixture_config(), admin).with_teardown_failure_sink(Arc::new(
        move |intent| {
            sink_values.lock().expect("sink lock").push(intent);
            Box::pin(async {})
        },
    ));
    let call = CallId::new("c-bounce-late-join").expect("call id");
    let alice = fixture_identity("alice");
    let minted = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .expect("token");

    sfu.revoke_issued_token(&call, &alice, &minted.jti);
    let observed = observed_sids(Some("RM_late"), Some("PA_late"));
    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &observed),
        SidObservationDisposition::Applied
    );

    let intents = reported.lock().expect("sink lock");
    assert_eq!(
        intents.len(),
        2,
        "the pending eject must fire again once the revoked holder becomes observable"
    );
    assert_eq!(intents[0].generation, None);
    assert_eq!(intents[0].room_sid, None);
    assert_eq!(intents[1].generation, stored_generation(&sfu, &call));
    assert_eq!(intents[1].room_sid, observed.room_sid);
    match &intents[1].target {
        TeardownTargetLite::Participant {
            identity,
            participant_sid,
        } => {
            assert_eq!(identity, &alice);
            assert_eq!(participant_sid, &observed.participant_sid);
        }
        TeardownTargetLite::Room => panic!("late join convergence must only enqueue eviction"),
    }
}

#[test]
fn targeted_revocation_keeps_other_live_issuances_authorized() {
    let admin = Arc::new(RecordingAdmin::default());
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink_values = Arc::clone(&reported);
    let sfu = LiveKitSfu::with_admin(fixture_config(), admin).with_teardown_failure_sink(Arc::new(
        move |intent| {
            sink_values.lock().expect("sink lock").push(intent);
            Box::pin(async {})
        },
    ));
    let call = CallId::new("c-targeted-authorized").expect("call id");
    let alice = fixture_identity("alice");
    let first = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .expect("token");
    let second = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .expect("token");

    sfu.revoke_issued_token(&call, &alice, &second.jti);

    assert!(
        !sfu.is_revoked(&first.jti),
        "revoking the bounced issuance must not downgrade another live token to nothing"
    );
    assert_eq!(
        reported.lock().expect("sink lock").len(),
        0,
        "another outstanding issuance keeps the participant authorized, so no eject is queued"
    );
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
    let _ = sfu.unregister_call_participant(&call, &alice, None);
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

#[test]
fn generation_increments_only_when_a_call_is_recreated() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("alice@waddle.social::reused-client-sid").unwrap();
    let alice = fixture_identity("alice");
    let bob = fixture_identity("bob");

    sfu.register_call_participant(&call, &alice);
    let first_generation = stored_generation(&sfu, &call).expect("generation stored");

    sfu.register_call_participant(&call, &bob);
    assert_eq!(
        stored_generation(&sfu, &call),
        Some(first_generation),
        "same-call membership churn must keep the generation stable"
    );

    let _ = sfu.unregister_call_participant(&call, &alice, None);
    assert_eq!(
        stored_generation(&sfu, &call),
        Some(first_generation),
        "removing a non-last participant must not advance the generation"
    );

    let _ = sfu.unregister_call_participant(&call, &bob, None);
    assert!(
        !sfu.calls.contains_key(&call),
        "empty calls must still be removed from the live registry"
    );
    assert_eq!(
        stored_generation(&sfu, &call),
        Some(first_generation),
        "the final clear must retain the last generation as a tombstone fence"
    );

    sfu.register_call_participant(&call, &alice);
    let second_generation = stored_generation(&sfu, &call).expect("new generation stored");
    assert!(
        second_generation > first_generation,
        "re-registering after the call emptied must advance the generation"
    );
    assert_ne!(
        second_generation.as_u64(),
        1,
        "direct-call reuse must never reset the generation counter to one"
    );
}

#[test]
fn sid_learning_stores_the_first_sid_and_defers_join_conflicts() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("c-sid-learning").unwrap();
    let alice = fixture_identity("alice");
    let first = observed_sids(Some("RM_first"), Some("PA_first"));
    let conflicting = observed_sids(Some("RM_other"), Some("PA_other"));

    sfu.register_call_participant(&call, &alice);
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&first),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );
    assert_eq!(stored_room_sid(&sfu, &call), first.room_sid);
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        first.participant_sid
    );

    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&conflicting),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::RoomRotationPending
    );
    assert_eq!(stored_room_sid(&sfu, &call), first.room_sid);
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        first.participant_sid
    );
}

#[test]
fn observe_path_requeues_pending_revocation_eject_without_deadlocking() {
    let admin = Arc::new(RecordingAdmin::default());
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink_values = Arc::clone(&reported);
    let sfu = LiveKitSfu::with_admin(fixture_config(), admin).with_teardown_failure_sink(Arc::new(
        move |intent| {
            sink_values.lock().expect("sink lock").push(intent);
            Box::pin(async {})
        },
    ));
    let call = CallId::new("c-observe-pending-eject").expect("call id");
    let alice = fixture_identity("alice");
    let minted = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .expect("token");

    sfu.revoke_issued_token(&call, &alice, &minted.jti);
    sfu.register_call_participant(&call, &alice);
    let observed = observed_sids(Some("RM_observed"), Some("PA_observed"));
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&observed),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );

    let intents = reported.lock().expect("sink lock");
    assert_eq!(intents.len(), 2);
    assert_eq!(intents[1].generation, stored_generation(&sfu, &call));
    assert_eq!(intents[1].room_sid, observed.room_sid);
    match &intents[1].target {
        TeardownTargetLite::Participant {
            identity,
            participant_sid,
        } => {
            assert_eq!(identity, &alice);
            assert_eq!(participant_sid, &observed.participant_sid);
        }
        TeardownTargetLite::Room => panic!("observe-path convergence must only enqueue eviction"),
    }
}

#[test]
fn join_side_participant_sid_advance_can_learn_the_first_room_sid() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("c-atomic-sid-learning").unwrap();
    let alice = fixture_identity("alice");
    let participant_only = observed_sids(None, Some("PA_current"));
    let rejoined = observed_sids(Some("RM_current"), Some("PA_rejoined"));

    sfu.register_call_participant(&call, &alice);
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&participant_only),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&rejoined),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );
    assert_eq!(
        stored_room_sid(&sfu, &call),
        rejoined.room_sid,
        "a join-side participant-sid advance may also learn the first room sid for the current incarnation"
    );
}

#[test]
fn unknown_identity_teardown_does_not_teach_room_sid() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("c-unknown-identity-sid").unwrap();
    let alice = fixture_identity("alice");
    let ghost = fixture_identity("mallory");
    let stale = observed_sids(Some("RM_stale"), Some("PA_stale"));
    let current = observed_sids(Some("RM_current"), Some("PA_current"));

    sfu.register_call_participant(&call, &alice);
    assert!(matches!(
        sfu.note_participant_left(&call, &ghost, Some(&stale)),
        TeardownDisposition::Applied(CallState::Active { remaining: 1 })
    ));
    assert_eq!(stored_room_sid(&sfu, &call), None);
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&current),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );
    assert_eq!(stored_room_sid(&sfu, &call), current.room_sid);
}

// -------- Admin-evict path (tokio runtime present) --------

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

use crate::admin::LiveKitAdmin;

type ListedParticipants = Vec<(Identity, Option<ParticipantSid>)>;

#[derive(Default)]
struct RecordingAdmin {
    rooms: Mutex<Vec<crate::admin::ListedRoom>>,
    list_rooms_errors: Mutex<bool>,
    remove_calls: Mutex<Vec<(CallId, Identity)>>,
    delete_calls: Mutex<Vec<CallId>>,
    remove_errors: Mutex<bool>,
    delete_errors: Mutex<bool>,
    remove_gate: Mutex<Option<Arc<Semaphore>>>,
    update_calls: Mutex<Vec<(CallId, Identity, MediaCapabilities)>>,
    /// Per-call artificial latency for `update_participant`,
    /// consumed in call order. Ordering tests queue a SLOW first
    /// delay and a FAST second one so that, without per-key
    /// serialization, the older push would complete last.
    update_delays: Mutex<std::collections::VecDeque<StdDuration>>,
    /// What LiveKit "reports" as connected per call. A call absent
    /// from the map lists as empty (room not found). Drives the
    /// reconciliation tests.
    live: Mutex<std::collections::HashMap<CallId, ListedParticipants>>,
    /// How many NON-Waddle participants LiveKit reports per call
    /// (an egress recorder, a SIP participant): occupancy that can
    /// never be a registry ghost but must still block DeleteRoom.
    foreign_live: Mutex<std::collections::HashMap<CallId, usize>>,
    /// When set, `room_occupancy` errors instead of returning a set —
    /// used to assert reconcile skips a call it can't confirm rather
    /// than sweeping it.
    list_errors: Mutex<bool>,
    /// Parks `room_occupancy` until released, so a test can register a
    /// participant while the probe is genuinely in flight.
    list_gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    /// Signalled once a parked `room_occupancy` call has been entered.
    list_entered: Mutex<Option<Arc<tokio::sync::Notify>>>,
    occupancy_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    occupancy_calls: AtomicUsize,
    occupancy_in_flight: AtomicUsize,
    max_occupancy_in_flight: AtomicUsize,
}

impl RecordingAdmin {
    fn fail_remove(&self) {
        *self.remove_errors.lock().expect("recording lock") = true;
    }

    fn fail_delete(&self) {
        *self.delete_errors.lock().expect("recording lock") = true;
    }

    fn block_removes_on(&self, gate: Arc<Semaphore>) {
        *self.remove_gate.lock().expect("recording lock") = Some(gate);
    }

    fn set_rooms(&self, rooms: Vec<crate::admin::ListedRoom>) {
        *self.rooms.lock().expect("recording lock") = rooms;
    }

    fn fail_list_rooms(&self) {
        *self.list_rooms_errors.lock().expect("recording lock") = true;
    }

    fn hold_all_occupancy(&self) {
        *self.occupancy_gate.lock().expect("recording lock") =
            Some(Arc::new(tokio::sync::Semaphore::new(0)));
    }

    fn release_occupancy(&self, permits: usize) {
        if let Some(gate) = self.occupancy_gate.lock().expect("recording lock").as_ref() {
            gate.add_permits(permits);
        }
    }

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
        self.live.lock().expect("recording lock").insert(
            call.clone(),
            identities
                .into_iter()
                .map(|identity| (identity, None))
                .collect(),
        );
    }

    fn set_live_with_sids(
        &self,
        call: &CallId,
        participants: Vec<(Identity, Option<ParticipantSid>)>,
    ) {
        self.live
            .lock()
            .expect("recording lock")
            .insert(call.clone(), participants);
    }

    fn set_foreign_live(&self, call: &CallId, count: usize) {
        self.foreign_live
            .lock()
            .expect("recording lock")
            .insert(call.clone(), count);
    }

    fn max_occupancy_in_flight(&self) -> usize {
        self.max_occupancy_in_flight.load(Ordering::SeqCst)
    }

    async fn await_occupancy_calls(&self, expected: usize, within: StdDuration) {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let observed = self.occupancy_calls.load(Ordering::Acquire);
            if observed >= expected || tokio::time::Instant::now() >= deadline {
                assert!(
                    observed >= expected,
                    "expected occupancy probes to reach {expected} before timeout, saw {observed}"
                );
                return;
            }
            tokio::time::sleep(StdDuration::from_millis(1)).await;
        }
    }

    /// Park the next `room_occupancy` call until [`Self::release_list`].
    fn hold_list(&self) {
        *self.list_gate.lock().expect("recording lock") =
            Some(Arc::new(tokio::sync::Notify::new()));
        *self.list_entered.lock().expect("recording lock") =
            Some(Arc::new(tokio::sync::Notify::new()));
    }

    fn release_list(&self) {
        let gate = self.list_gate.lock().expect("recording lock").clone();
        if let Some(gate) = gate {
            // `notify_one`, not `notify_waiters`: the latter drops the
            // signal when no waiter is registered yet, and both signals
            // in this handshake are one-shot events whose ordering
            // against the other task's registration is not guaranteed
            // on a multi-threaded runtime. A stored permit makes them
            // ordering-independent.
            gate.notify_one();
        }
    }

    /// Wait until a parked `room_occupancy` call has actually been
    /// entered, so the test's registration lands mid-probe rather than
    /// racing the spawn.
    ///
    /// Both sides of the handshake use `notify_one` so neither signal
    /// can be lost to a registration race. Panics if the probe never
    /// starts. Swallowing that timeout made
    /// the post-probe-registry test vacuous: `release_list` would
    /// notify before the probe installed its waiter, the teardown would
    /// stay parked forever, and the assertion that no `DeleteRoom`
    /// fired would pass without the guard ever being exercised.
    async fn await_list_in_flight(&self, within: StdDuration) {
        let entered = self
            .list_entered
            .lock()
            .expect("recording lock")
            .clone()
            .expect("hold_list must be called before awaiting the probe");
        tokio::time::timeout(within, entered.notified())
            .await
            .expect("the occupancy probe must actually start, or this test proves nothing");
    }

    fn fail_list(&self) {
        self.set_list_failing(true);
    }

    fn set_list_failing(&self, failing: bool) {
        *self.list_errors.lock().expect("recording lock") = failing;
    }
}

impl LiveKitAdmin for RecordingAdmin {
    fn list_rooms(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<crate::admin::ListedRoom>, SfuError>> + Send + '_>>
    {
        Box::pin(async move {
            if *self.list_rooms_errors.lock().expect("recording lock") {
                return Err(SfuError::InvalidCallId(
                    "simulated ListRooms failure".into(),
                ));
            }
            Ok(self.rooms.lock().expect("recording lock").clone())
        })
    }

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
            let gate = self.remove_gate.lock().expect("recording lock").clone();
            if let Some(gate) = gate {
                let _permit = gate.acquire().await.expect("test gate remains open");
            }
            if *self.remove_errors.lock().expect("recording lock") {
                return Err(SfuError::InvalidCallId("simulated remove failure".into()));
            }
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
            if *self.delete_errors.lock().expect("recording lock") {
                return Err(SfuError::InvalidCallId("simulated delete failure".into()));
            }
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

    fn room_occupancy<'a>(
        &'a self,
        room: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<crate::admin::RoomOccupancy, SfuError>> + Send + 'a>>
    {
        let room = room.clone();
        Box::pin(async move {
            self.occupancy_calls.fetch_add(1, Ordering::SeqCst);
            let in_flight = self.occupancy_in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_occupancy_in_flight
                .fetch_max(in_flight, Ordering::SeqCst);
            let occupancy_gate = self.occupancy_gate.lock().expect("recording lock").clone();
            if let Some(gate) = occupancy_gate {
                let permit = gate
                    .acquire_owned()
                    .await
                    .expect("test occupancy semaphore stays open");
                permit.forget();
            }
            if *self.list_errors.lock().expect("recording lock") {
                self.occupancy_in_flight.fetch_sub(1, Ordering::SeqCst);
                return Err(SfuError::InvalidCallId("simulated list failure".into()));
            }
            let gate = self.list_gate.lock().expect("recording lock").clone();
            if let Some(gate) = gate {
                let waiter = gate.notified();
                if let Some(entered) = self.list_entered.lock().expect("recording lock").clone() {
                    entered.notify_one();
                }
                waiter.await;
            }
            let occupancy = crate::admin::RoomOccupancy {
                waddle: self
                    .live
                    .lock()
                    .expect("recording lock")
                    .get(&room)
                    .cloned()
                    .unwrap_or_default(),
                foreign: self
                    .foreign_live
                    .lock()
                    .expect("recording lock")
                    .get(&room)
                    .copied()
                    .unwrap_or(0),
            };
            self.occupancy_in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(occupancy)
        })
    }
}

/// Wait until `admin` has recorded at least `expected` grant updates, or
/// the deadline elapses. Returns on the first observation at or above
/// `expected` so callers assert against settled state without assuming
/// how long the spawned tasks take.
async fn await_update_count(admin: &RecordingAdmin, expected: usize, within: StdDuration) -> usize {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let observed = admin.update_snapshot().len();
        if observed >= expected || tokio::time::Instant::now() >= deadline {
            return observed;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
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

#[test]
fn teardown_without_runtime_reports_typed_intents() {
    let admin = Arc::new(RecordingAdmin::default());
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink_values = Arc::clone(&reported);
    let sfu = LiveKitSfu::with_admin(fixture_config(), admin).with_teardown_failure_sink(Arc::new(
        move |intent| {
            sink_values.lock().expect("sink lock").push(intent);
            Box::pin(async {})
        },
    ));
    let call = CallId::new("r-no-runtime").expect("call id");
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);

    let _ = sfu.unregister_call_participant(&call, &alice, None);

    let intents = reported.lock().expect("sink lock");
    assert_eq!(intents.len(), 2);
    assert!(matches!(
        intents[0].target,
        TeardownTargetLite::Participant { .. }
    ));
    assert!(matches!(intents[1].target, TeardownTargetLite::Room));
    assert!(intents.iter().all(|intent| intent.generation.is_some()));
}

#[tokio::test]
async fn failed_admin_effects_are_independently_reported() {
    let admin = Arc::new(RecordingAdmin::default());
    admin.fail_remove();
    admin.fail_delete();
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink_values = Arc::clone(&reported);
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>)
        .with_teardown_failure_sink(Arc::new(move |intent| {
            sink_values.lock().expect("sink lock").push(intent);
            Box::pin(async {})
        }));
    let call = CallId::new("r-admin-fail").expect("call id");
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);

    let _ = sfu.unregister_call_participant(&call, &alice, None);
    for _ in 0..100 {
        if reported.lock().expect("sink lock").len() == 2 {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(5)).await;
    }

    let intents = reported.lock().expect("sink lock");
    assert_eq!(intents.len(), 2);
    assert!(intents
        .iter()
        .any(|intent| matches!(intent.target, TeardownTargetLite::Participant { .. })));
    assert!(intents
        .iter()
        .any(|intent| matches!(intent.target, TeardownTargetLite::Room)));
}

#[tokio::test]
async fn saturated_teardown_gate_reports_without_spawning_admin_work() {
    let admin = Arc::new(RecordingAdmin::default());
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink_values = Arc::clone(&reported);
    let mut sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>)
        .with_teardown_failure_sink(Arc::new(move |intent| {
            sink_values.lock().expect("sink lock").push(intent);
            Box::pin(async {})
        }));
    sfu.admin_permits = Arc::new(Semaphore::new(0));
    let call = CallId::new("r-saturated").expect("call id");
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);

    let _ = sfu.unregister_call_participant(&call, &alice, None);

    for _ in 0..100 {
        if reported.lock().expect("sink lock").len() == 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(reported.lock().expect("sink lock").len(), 2);
    assert!(admin.remove_snapshot().is_empty());
    assert!(admin.delete_snapshot().is_empty());
}

#[tokio::test]
async fn teardown_burst_reserves_before_spawn_and_defers_excess_work() {
    let admin = Arc::new(RecordingAdmin::default());
    let gate = Arc::new(Semaphore::new(0));
    admin.block_removes_on(Arc::clone(&gate));
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink_values = Arc::clone(&reported);
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>)
        .with_teardown_failure_sink(Arc::new(move |intent| {
            sink_values.lock().expect("sink lock").push(intent);
            Box::pin(async {})
        }));
    let excess = 32;
    for index in 0..(ADMIN_CONCURRENCY + excess) {
        let call = CallId::new(format!("r-burst-{index}")).expect("call id");
        let alice = fixture_identity("alice");
        sfu.register_call_participant(&call, &alice);
        let _ = sfu.unregister_call_participant(&call, &alice, None);
    }
    for _ in 0..100 {
        if admin.remove_snapshot().len() == ADMIN_CONCURRENCY {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(5)).await;
    }

    assert_eq!(admin.remove_snapshot().len(), ADMIN_CONCURRENCY);
    assert_eq!(
        reported.lock().expect("sink lock").len(),
        (ADMIN_CONCURRENCY + excess) * 2,
        "every last-participant teardown pre-reports participant and room intents, whether admitted or saturated"
    );
    gate.add_permits(ADMIN_CONCURRENCY);
}

#[tokio::test]
async fn saturated_teardown_reports_use_one_supervised_persistence_task() {
    let admin = Arc::new(RecordingAdmin::default());
    let gate = Arc::new(Semaphore::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let sink_gate = Arc::clone(&gate);
    let sink_started = Arc::clone(&started);
    let mut sfu = LiveKitSfu::with_admin(fixture_config(), admin).with_teardown_failure_sink(
        Arc::new(move |_| {
            let gate = Arc::clone(&sink_gate);
            let started = Arc::clone(&sink_started);
            Box::pin(async move {
                started.fetch_add(1, Ordering::SeqCst);
                let _permit = gate.acquire().await.expect("report gate remains open");
            })
        }),
    );
    sfu.admin_permits = Arc::new(Semaphore::new(0));

    let calls = 32;
    for index in 0..calls {
        let call = CallId::new(format!("r-persist-burst-{index}")).expect("call id");
        let alice = fixture_identity("alice");
        sfu.register_call_participant(&call, &alice);
        let _ = sfu.unregister_call_participant(&call, &alice, None);
    }
    for _ in 0..100 {
        if started.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(started.load(Ordering::SeqCst), 1);

    gate.add_permits(calls * 2);
    for _ in 0..100 {
        if started.load(Ordering::SeqCst) == calls * 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(started.load(Ordering::SeqCst), calls * 2);
}

#[tokio::test]
async fn teardown_executor_sid_guard_skips_a_new_room_incarnation() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-sid-guard").expect("call id");
    let alice = fixture_identity("alice");
    let current = observed_sids(Some("RM_current"), Some("PA_current"));
    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &current),
        SidObservationDisposition::Applied
    );
    let stale = CallTeardownIntentLite {
        call_id: call,
        target: TeardownTargetLite::Participant {
            identity: alice,
            participant_sid: Some(fixture_participant_sid("PA_old")),
        },
        generation: None,
        room_sid: Some(fixture_room_sid("RM_old")),
    };

    assert_eq!(
        sfu.teardown_executor()
            .execute(&stale)
            .await
            .expect("typed no-op"),
        TeardownExecution::StaleGeneration
    );
    assert!(admin.remove_snapshot().is_empty());
}

#[tokio::test]
async fn teardown_executor_defers_when_live_entry_has_not_learned_persisted_sids() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-sid-unresolved").expect("call id");
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);
    let persisted = CallTeardownIntentLite {
        call_id: call,
        target: TeardownTargetLite::Participant {
            identity: alice,
            participant_sid: Some(fixture_participant_sid("PA_old")),
        },
        generation: None,
        room_sid: Some(fixture_room_sid("RM_old")),
    };

    assert_eq!(
        sfu.teardown_executor()
            .execute(&persisted)
            .await
            .expect("typed defer"),
        TeardownExecution::Occupied
    );
    assert!(admin.remove_snapshot().is_empty());
}

#[tokio::test]
async fn teardown_executor_requeues_fenced_missing_calls_until_a_reconcile_pass_completes() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-restart-fence").expect("call id");
    let alice = fixture_identity("alice");
    let intent = CallTeardownIntentLite {
        call_id: call.clone(),
        target: TeardownTargetLite::Participant {
            identity: alice.clone(),
            participant_sid: Some(fixture_participant_sid("PA_restart")),
        },
        generation: Some(CallGeneration::new(1)),
        room_sid: Some(fixture_room_sid("RM_restart")),
    };

    assert_eq!(
        sfu.teardown_executor()
            .execute(&intent)
            .await
            .expect("typed defer"),
        TeardownExecution::Occupied
    );
    assert!(
        admin.remove_snapshot().is_empty(),
        "pre-reconcile restart drain must requeue fenced teardown rows"
    );

    sfu.reconcile_pass_completed.store(true, Ordering::Release);

    assert_eq!(
        sfu.teardown_executor()
            .execute(&intent)
            .await
            .expect("typed execution"),
        TeardownExecution::Executed
    );
    assert_eq!(admin.remove_snapshot(), vec![(call, alice)]);
}

#[tokio::test]
async fn inline_teardown_reports_participant_and_room_intents_before_admin_work_completes() {
    let admin = Arc::new(RecordingAdmin::default());
    let gate = Arc::new(Semaphore::new(0));
    admin.block_removes_on(Arc::clone(&gate));
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink_values = Arc::clone(&reported);
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>)
        .with_teardown_failure_sink(Arc::new(move |intent| {
            sink_values.lock().expect("sink lock").push(intent);
            Box::pin(async {})
        }));
    let call = CallId::new("r-inline-report-first").expect("call id");
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);

    let _ = sfu.unregister_call_participant(&call, &alice, None);
    for _ in 0..100 {
        if reported.lock().expect("sink lock").len() == 2 {
            break;
        }
        tokio::task::yield_now().await;
    }

    {
        let intents = reported.lock().expect("sink lock");
        assert_eq!(
            intents.len(),
            2,
            "participant and room intents must be durably reported before the blocked inline remove finishes"
        );
        assert!(intents
            .iter()
            .any(|intent| matches!(intent.target, TeardownTargetLite::Participant { .. })));
        assert!(intents
            .iter()
            .any(|intent| matches!(intent.target, TeardownTargetLite::Room)));
    }
    assert_eq!(admin.remove_snapshot().len(), 1);

    gate.add_permits(1);
    drain_admin_tasks().await;
    assert_eq!(
        reported.lock().expect("sink lock").len(),
        2,
        "successful inline teardown must not enqueue duplicate participant or room intents"
    );
    assert_eq!(
        admin.delete_snapshot(),
        vec![call],
        "the pre-reported room intent must coexist safely with a successful inline delete"
    );
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

    // Poll to a bounded deadline instead of sleeping a fixed budget: a
    // fixed wall-clock wait is exactly the kind of assumption that turns
    // into an intermittent CI failure on a loaded runner.
    await_update_count(&admin, 2, StdDuration::from_secs(10)).await;

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
    let state = applied_state(sfu.unregister_call_participant(&call, &alice, None));
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
    let state = applied_state(sfu.unregister_call_participant(&call, &alice, None));
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
async fn unregister_of_unknown_identity_does_not_schedule_admin_work() {
    // Edge case: a session-terminate arrives without a matching
    // register (e.g. server-side state was lost, a client races
    // a re-init, a replayed terminate from a long-dead session).
    // With no registry evidence we must not schedule any admin work:
    // the call may belong to a newer incarnation or another path,
    // and the targeted revocation/eject flow now owns the "identity
    // may still be live" convergence instead.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-ghost").unwrap();
    let ghost = fixture_identity("mallory");

    let state = applied_state(sfu.unregister_call_participant(&call, &ghost, None));
    assert!(
        matches!(state, CallState::Active { remaining: 0 }),
        "ghost unregister must NOT report CallState::Ended; got {state:?}",
    );
    drain_admin_tasks().await;

    assert!(admin.remove_snapshot().is_empty());
    assert!(
        admin.delete_snapshot().is_empty(),
        "DeleteRoom must not fire when we never tracked the participant",
    );
}

#[test]
fn unregister_requeues_eject_when_a_revoked_holder_rejoins_late() {
    let admin = Arc::new(RecordingAdmin::default());
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink_values = Arc::clone(&reported);
    let sfu = LiveKitSfu::with_admin(fixture_config(), admin).with_teardown_failure_sink(Arc::new(
        move |intent| {
            sink_values.lock().expect("sink lock").push(intent);
            Box::pin(async {})
        },
    ));
    let call = CallId::new("r-late-rejoin").expect("call id");
    let alice = fixture_identity("alice");
    let first_join = observed_sids(Some("RM_first"), Some("PA_first"));
    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &first_join),
        SidObservationDisposition::Applied
    );
    let minted = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .expect("token");

    let state = applied_state(sfu.unregister_call_participant(&call, &alice, Some(&first_join)));
    assert_eq!(state, CallState::Ended);
    assert!(sfu.is_revoked(&minted.jti));

    let second_join = observed_sids(Some("RM_second"), Some("PA_second"));
    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &second_join),
        SidObservationDisposition::Applied
    );

    let intents = reported.lock().expect("sink lock");
    assert_eq!(
        intents.len(),
        3,
        "late rejoin should append a fresh RemoveParticipant after the original participant+room intents"
    );
    assert!(matches!(
        intents[0].target,
        TeardownTargetLite::Participant { .. }
    ));
    assert!(matches!(intents[1].target, TeardownTargetLite::Room));
    assert_eq!(intents[2].generation, stored_generation(&sfu, &call));
    assert_eq!(intents[2].room_sid, second_join.room_sid);
    match &intents[2].target {
        TeardownTargetLite::Participant {
            identity,
            participant_sid,
        } => {
            assert_eq!(identity, &alice);
            assert_eq!(participant_sid, &second_join.participant_sid);
        }
        TeardownTargetLite::Room => panic!("late rejoin convergence must only enqueue eviction"),
    }
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

    let disposition = sfu.note_participant_left(&call, &alice, None);
    assert!(matches!(disposition, TeardownDisposition::Applied(_)));
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
async fn stale_sid_webhook_is_dropped_after_same_call_id_is_reused() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-stale-room").unwrap();
    let alice = fixture_identity("alice");
    let call_a = observed_sids(Some("RM_A"), Some("PA_A"));
    let call_b = observed_sids(Some("RM_B"), Some("PA_B"));

    sfu.register_call_participant(&call, &alice);
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&call_a),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );
    assert!(matches!(
        sfu.note_participant_left(&call, &alice, Some(&call_a)),
        TeardownDisposition::Applied(CallState::Ended)
    ));

    sfu.register_call_participant(&call, &alice);
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&call_b),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );

    let disposition = sfu.note_participant_left(&call, &alice, Some(&call_a));
    drain_admin_tasks().await;

    assert_eq!(disposition, TeardownDisposition::StaleSid);
    assert!(sfu.has_call_participant(&call, &alice));
    assert_eq!(stored_room_sid(&sfu, &call), call_b.room_sid);
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        call_b.participant_sid
    );
    assert!(
        admin.remove_snapshot().is_empty() && admin.delete_snapshot().is_empty(),
        "stale webhook must not trigger admin teardown"
    );
}

#[tokio::test]
async fn matching_sid_webhook_still_tears_down() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-match").unwrap();
    let alice = fixture_identity("alice");
    let observed = observed_sids(Some("RM_match"), Some("PA_match"));

    sfu.register_call_participant(&call, &alice);
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&observed),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );

    let disposition = sfu.note_participant_left(&call, &alice, Some(&observed));

    assert_eq!(disposition, TeardownDisposition::Applied(CallState::Ended));
    assert_eq!(sfu.participant_count(&call), 0);
}

#[tokio::test]
async fn teardown_without_sids_stays_backward_compatible() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-no-sids").unwrap();
    let alice = fixture_identity("alice");
    let learned = observed_sids(Some("RM_backcompat"), Some("PA_backcompat"));

    sfu.register_call_participant(&call, &alice);
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&learned),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );

    let disposition = sfu.note_participant_left(&call, &alice, None);

    assert_eq!(disposition, TeardownDisposition::Applied(CallState::Ended));
    assert_eq!(sfu.participant_count(&call), 0);
}

#[tokio::test]
async fn stale_generation_skips_scheduled_remove_participant() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-stale-generation").unwrap();
    let alice = fixture_identity("alice");
    let call_a = observed_sids(Some("RM_old"), Some("PA_old"));
    let call_b = observed_sids(Some("RM_new"), Some("PA_new"));

    sfu.register_call_participant(&call, &alice);
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&call_a),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );
    assert_eq!(
        applied_state(sfu.unregister_call_participant(&call, &alice, Some(&call_a))),
        CallState::Ended
    );

    sfu.register_call_participant(&call, &alice);
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&call_b),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::Applied
    );
    drain_admin_tasks().await;

    assert!(sfu.has_call_participant(&call, &alice));
    assert!(
        admin.remove_snapshot().is_empty(),
        "queued RemoveParticipant from the older generation must be skipped"
    );
}

#[tokio::test]
async fn no_call_teardown_uses_last_generation_to_protect_a_rejoin() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-no-call-stale-generation").unwrap();
    let alice = fixture_identity("alice");

    sfu.register_call_participant(&call, &alice);
    assert!(matches!(
        sfu.note_participant_left(&call, &alice, None),
        TeardownDisposition::Applied(CallState::Ended)
    ));
    assert!(matches!(
        sfu.unregister_call_participant(&call, &alice, None),
        TeardownDisposition::Applied(CallState::Active { remaining: 0 })
    ));

    sfu.register_call_participant(&call, &alice);
    drain_admin_tasks().await;

    assert!(sfu.has_call_participant(&call, &alice));
    assert!(
        admin.remove_snapshot().is_empty(),
        "an unknown-call teardown queued under the prior generation must not evict the rejoin"
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
    let state = applied_state(sfu.unregister_call_participant(&call, &alice, None));
    assert_eq!(state, CallState::Ended);

    // Bob rejoins before the spawned future polls. With a single-
    // threaded current-thread runtime this synchronous register
    // is guaranteed to land before any `yield_now`-scheduled
    // continuation observes the registry.
    sfu.register_call_participant(&call, &bob);

    drain_admin_tasks().await;

    let removes = admin.remove_snapshot();
    assert!(
        removes.is_empty(),
        "queued RemoveParticipant from the older generation must be skipped"
    );
    assert!(
        admin.delete_snapshot().is_empty(),
        "DeleteRoom must be suppressed by the rejoin re-check; got {:?}",
        admin.delete_snapshot(),
    );
}

#[tokio::test]
async fn delete_room_skipped_when_livekit_reports_another_replicas_participant() {
    // #1445 second-order defect: with two waddle-server replicas the
    // registry is process-local, so "our map just emptied" says
    // nothing about the other replica's participants in the same
    // LiveKit room. The teardown must confirm emptiness against
    // LiveKit itself before DeleteRoom.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-cross-replica").unwrap();
    let alice = fixture_identity("alice");
    let bob = fixture_identity("bob");

    // Only Alice is registered locally; Bob joined via the other
    // replica, so LiveKit sees both.
    sfu.register_call_participant(&call, &alice);
    admin.set_live(&call, vec![alice.clone(), bob.clone()]);

    let state = applied_state(sfu.unregister_call_participant(&call, &alice, None));
    assert_eq!(state, CallState::Ended, "locally the call just emptied");
    drain_admin_tasks().await;

    assert_eq!(
        admin.remove_snapshot().len(),
        1,
        "RemoveParticipant for Alice must still fire"
    );
    assert!(
        admin.delete_snapshot().is_empty(),
        "DeleteRoom must be suppressed while LiveKit reports another \
         replica's participant; got {:?}",
        admin.delete_snapshot(),
    );
}

#[tokio::test]
async fn delete_room_fires_when_livekit_reports_only_the_departing_participant() {
    // LiveKit's participant list can momentarily still contain the
    // participant we just issued RemoveParticipant for. That stale
    // echo of the departing identity must not suppress the delete,
    // or every clean last-leave would leak the room until
    // empty_timeout.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-departing-echo").unwrap();
    let alice = fixture_identity("alice");

    sfu.register_call_participant(&call, &alice);
    admin.set_live(&call, vec![alice.clone()]);

    let state = applied_state(sfu.unregister_call_participant(&call, &alice, None));
    assert_eq!(state, CallState::Ended);
    drain_admin_tasks().await;

    assert_eq!(
        admin.delete_snapshot(),
        vec![call],
        "DeleteRoom must fire when LiveKit reports nobody but the leaver"
    );
}

#[tokio::test]
async fn delete_room_skipped_when_only_a_foreign_participant_remains() {
    // An egress recorder (or SIP/ingress participant) has an identity
    // we never minted, so it can never be a registry ghost — but it IS
    // occupancy. Deleting the room around it would kill the recording.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-egress").unwrap();
    let alice = fixture_identity("alice");

    sfu.register_call_participant(&call, &alice);
    admin.set_live(&call, vec![alice.clone()]);
    admin.set_foreign_live(&call, 1);

    let state = applied_state(sfu.unregister_call_participant(&call, &alice, None));
    assert_eq!(state, CallState::Ended);
    drain_admin_tasks().await;

    assert!(
        admin.delete_snapshot().is_empty(),
        "DeleteRoom must be suppressed while a non-Waddle participant \
         (egress/SIP) is still connected; got {:?}",
        admin.delete_snapshot(),
    );
}

#[tokio::test]
async fn delete_room_skipped_when_a_participant_registers_during_the_occupancy_probe() {
    // The local rejoin re-check happens BEFORE the LiveKit round-trip,
    // so the probe itself is a second window in which a fresh joiner
    // can register locally — and the already-taken LiveKit snapshot
    // cannot see them, because they have not connected yet. The guard
    // must re-check the registry after the probe (#1129 + #1445).
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let call = CallId::new("r-probe-race").unwrap();
    let alice = fixture_identity("alice");
    let bob = fixture_identity("bob");

    sfu.register_call_participant(&call, &alice);
    // LiveKit reports only the departing participant, so the probe
    // says "empty" — the decision then rests entirely on the
    // post-probe local re-check.
    admin.set_live(&call, vec![alice.clone()]);
    admin.hold_list();

    let state = applied_state(sfu.unregister_call_participant(&call, &alice, None));
    assert_eq!(state, CallState::Ended);

    // Bob joins while the probe is parked in flight.
    admin.await_list_in_flight(StdDuration::from_secs(5)).await;
    sfu.register_call_participant(&call, &bob);
    admin.release_list();

    for _ in 0..50 {
        if !admin.delete_snapshot().is_empty() {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }

    assert!(
        admin.delete_snapshot().is_empty(),
        "DeleteRoom must be suppressed by the post-probe re-check; got {:?}",
        admin.delete_snapshot(),
    );
}

#[tokio::test]
async fn delete_room_skipped_when_livekit_occupancy_cannot_be_confirmed() {
    // Fail-safe: if the ListParticipants probe errors we cannot rule
    // out participants on the other replica, so the delete is
    // skipped and the room lapses via LiveKit's own empty_timeout.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("r-list-error").unwrap();
    let alice = fixture_identity("alice");

    sfu.register_call_participant(&call, &alice);
    admin.fail_list();

    let state = applied_state(sfu.unregister_call_participant(&call, &alice, None));
    assert_eq!(state, CallState::Ended);
    drain_admin_tasks().await;

    assert_eq!(
        admin.remove_snapshot().len(),
        1,
        "RemoveParticipant must still fire"
    );
    assert!(
        admin.delete_snapshot().is_empty(),
        "DeleteRoom must be suppressed when occupancy cannot be confirmed"
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
        first_pass.swept.is_empty(),
        "one absent observation must not sweep (#1127): {first_pass:?}"
    );
    assert!(
        sfu.has_call_participant(&call, &bob),
        "Bob must survive the first absent pass"
    );

    let swept = sfu.reconcile_active_calls(ChronoDuration::zero()).await;

    assert_eq!(swept.swept, vec![(call.clone(), bob.clone())]);
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
        swept.swept.is_empty(),
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

    assert!(
        swept.swept.is_empty(),
        "connected participant must not be swept"
    );
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
        swept.swept.is_empty(),
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
    assert!(
        pass1.swept.is_empty(),
        "restart pass must not sweep: {pass1:?}"
    );
    assert_eq!(sfu.participant_count(&call_a), 1);
    assert_eq!(sfu.participant_count(&call_b), 1);

    // Clients reconnected before pass 2.
    admin.set_live(&call_a, vec![alice.clone()]);
    admin.set_live(&call_b, vec![bob.clone()]);
    let pass2 = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert!(
        pass2.swept.is_empty(),
        "reconnected clients must not be swept"
    );

    // Pass 3: streaks were reset by the connected observation, so
    // a later single absent blip still does not sweep.
    admin.set_live(&call_a, vec![]);
    let pass3 = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert!(
        pass3.swept.is_empty(),
        "streak must have been reset by the connected pass: {pass3:?}"
    );
    assert!(sfu.has_call_participant(&call_a, &alice));
}

#[tokio::test]
async fn reconcile_room_sid_rotation_reincarnates_call_and_accepts_new_sid_observation() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("sid-rotation@muc.waddle.social").unwrap();
    let alice = fixture_identity("alice");
    let old_observed = observed_sids(Some("RM_old"), Some("PA_old"));
    let new_observed = observed_sids(Some("RM_new"), Some("PA_new"));

    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &old_observed),
        SidObservationDisposition::Applied
    );
    let old_generation = stored_generation(&sfu, &call).expect("old generation");
    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &new_observed),
        SidObservationDisposition::RoomRotationPending,
        "the first join from a recreated room must remain retryable until reconcile rotates the fence"
    );
    assert_eq!(
        stored_room_sid(&sfu, &call),
        old_observed.room_sid,
        "a pending room rotation must not mutate the stored room fence"
    );
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        old_observed.participant_sid,
        "a pending room rotation must not mutate participant state"
    );
    admin.set_rooms(vec![crate::admin::ListedRoom {
        name: call.to_string(),
        sid: new_observed.room_sid.clone(),
        num_participants: Some(1),
    }]);
    admin.set_live(&call, vec![alice.clone()]);

    let summary = sfu.reconcile_active_calls(ChronoDuration::zero()).await;

    assert!(
        summary.swept.is_empty(),
        "room-sid rotation must not sweep a live participant"
    );
    assert_eq!(stored_room_sid(&sfu, &call), new_observed.room_sid);
    assert!(
        stored_generation(&sfu, &call).expect("new generation") > old_generation,
        "new room sid must advance the generation fence"
    );
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        None,
        "participant sids from the old room incarnation must be cleared"
    );
    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &new_observed),
        SidObservationDisposition::Applied,
        "the redelivered join from the new room sid must apply after reconciliation"
    );
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        new_observed.participant_sid
    );
}

#[tokio::test]
async fn reconcile_listing_learns_a_missing_room_sid_without_rotating_generation() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("sid-learn@muc.waddle.social").unwrap();
    let alice = fixture_identity("alice");
    let learned_room_sid = fixture_room_sid("RM_learned");
    let stale = observed_sids(Some("RM_old"), Some("PA_old"));

    sfu.register_call_participant(&call, &alice);
    let original_generation = stored_generation(&sfu, &call).expect("generation");
    admin.set_rooms(vec![crate::admin::ListedRoom {
        name: call.to_string(),
        sid: Some(learned_room_sid.clone()),
        num_participants: Some(1),
    }]);
    admin.set_live(&call, vec![alice.clone()]);

    let summary = sfu.reconcile_active_calls(ChronoDuration::zero()).await;

    assert!(summary.swept.is_empty());
    assert_eq!(stored_room_sid(&sfu, &call), Some(learned_room_sid.clone()));
    assert_eq!(stored_generation(&sfu, &call), Some(original_generation));
    assert_eq!(
        sfu.observe_call_participant_sids(
            &call,
            &alice,
            Some(&stale),
            SidObservationDirection::Join,
        ),
        SidObservationDisposition::RoomRotationPending,
        "a join-side room mismatch stays retryable until an authoritative listing resolves it"
    );
    assert_eq!(stored_room_sid(&sfu, &call), Some(learned_room_sid));
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
        .swept
        .is_empty());
    // Failed pass → streak reset.
    admin.set_list_failing(true);
    assert!(sfu
        .reconcile_active_calls(ChronoDuration::zero())
        .await
        .swept
        .is_empty());
    admin.set_list_failing(false);
    // Absent pass again → streak restarts at 1, still no sweep.
    let third = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert!(
        third.swept.is_empty(),
        "failed pass must reset the streak: {third:?}"
    );
    assert_eq!(sfu.participant_count(&call), 1);
    // Second CONSECUTIVE absent pass → swept.
    let fourth = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert_eq!(fourth.swept, vec![(call.clone(), alice)]);
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
        .swept
        .is_empty());
    // Rejoin between passes.
    sfu.register_call_participant(&call, &alice);
    // This absent pass is the FIRST of the new registration.
    assert!(
        sfu.reconcile_active_calls(ChronoDuration::zero())
            .await
            .swept
            .is_empty(),
        "re-registration must reset the absence streak"
    );
    assert_eq!(sfu.participant_count(&call), 1);
}

#[tokio::test]
async fn reconcile_discovers_listed_livekit_rooms_and_sweeps_disconnected_after_grace() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("recovered@muc.waddle.social").unwrap();
    let alice = fixture_identity("alice");

    admin.set_rooms(vec![crate::admin::ListedRoom {
        name: call.as_str().to_owned(),
        sid: Some(RoomSid::new("RM_recovered").expect("room sid")),
        num_participants: None,
    }]);
    admin.set_live(&call, vec![alice.clone()]);

    let adopted = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert_eq!(adopted.rooms_examined, 1);
    assert_eq!(adopted.rooms_adopted, 1);
    assert!(adopted.swept.is_empty());
    assert_eq!(adopted.rooms_swept, 0);
    assert!(sfu.has_call_participant(&call, &alice));
    assert_eq!(
        stored_room_sid(&sfu, &call),
        Some(RoomSid::new("RM_recovered").expect("room sid"))
    );

    admin.set_live(&call, vec![]);
    let first_absent = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert_eq!(first_absent.rooms_swept, 0);
    assert!(first_absent.swept.is_empty());
    assert!(sfu.has_call_participant(&call, &alice));

    let second_absent = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert_eq!(second_absent.rooms_swept, 1);
    assert_eq!(second_absent.swept, vec![(call.clone(), alice.clone())]);
    assert!(!sfu.has_call_participant(&call, &alice));
}

#[tokio::test]
async fn reconcile_list_rooms_error_does_not_skip_registry_pass() {
    let admin = Arc::new(RecordingAdmin::default());
    admin.fail_list_rooms();
    admin.set_rooms(vec![crate::admin::ListedRoom {
        name: "listed@muc.waddle.social".to_owned(),
        sid: None,
        num_participants: None,
    }]);
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("recovery@muc.waddle.social").unwrap();
    let alice = fixture_identity("alice");
    sfu.register_call_participant(&call, &alice);

    let first = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert_eq!(first.rooms_examined, 1);
    assert_eq!(first.rooms_adopted, 0);
    assert!(first.swept.is_empty());
    let second = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert_eq!(second.swept, vec![(call.clone(), alice.clone())]);
    assert_eq!(admin.occupancy_calls.load(Ordering::SeqCst), 2);
    assert_eq!(admin.delete_snapshot().len(), 0);
    assert!(!sfu.has_call_participant(&call, &alice));
}

#[tokio::test]
async fn reconcile_limits_concurrent_room_occupancy_probes() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    admin.hold_all_occupancy();

    let room_count = RECONCILE_CONCURRENCY + 4;
    for i in 0..room_count {
        let call = CallId::new(format!("bulk-{i}.muc.waddle.social")).unwrap();
        sfu.register_call_participant(&call, &fixture_identity("alice"));
        admin.set_live(&call, vec![]);
    }

    let reconcile =
        tokio::spawn(async move { sfu.reconcile_active_calls(ChronoDuration::zero()).await });

    admin
        .await_occupancy_calls(RECONCILE_CONCURRENCY, StdDuration::from_secs(1))
        .await;
    admin.release_occupancy(room_count);

    let summary = reconcile
        .await
        .expect("reconcile task must finish once occupancy gate is released");

    assert_eq!(summary.rooms_examined, room_count as u64);
    assert_eq!(
        admin.occupancy_calls.load(Ordering::SeqCst),
        room_count,
        "every room in the backlog must reach room_occupancy"
    );
    assert!(admin.max_occupancy_in_flight() <= RECONCILE_CONCURRENCY);
    assert_eq!(admin.max_occupancy_in_flight(), RECONCILE_CONCURRENCY);
}

#[test]
fn restored_participant_skips_the_first_empty_bucket_eject_after_adoption() {
    let admin = Arc::new(RecordingAdmin::default());
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink_values = Arc::clone(&reported);
    let sfu = LiveKitSfu::with_admin(fixture_config(), admin).with_teardown_failure_sink(Arc::new(
        move |intent| {
            sink_values.lock().expect("sink lock").push(intent);
            Box::pin(async {})
        },
    ));
    let call = CallId::new("r-adopted-empty-bucket").expect("call id");
    let alice = fixture_identity("alice");

    assert!(sfu.adopt_discovered_call(
        &call,
        Some(fixture_room_sid("RM_adopted")),
        &[(alice.clone(), None)],
        Utc::now()
    ));
    let first_minted = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .expect("first post-adoption token");

    sfu.revoke_issued_token(&call, &alice, &first_minted.jti);

    assert!(sfu.is_revoked(&first_minted.jti));
    assert!(
        reported.lock().expect("sink lock").is_empty(),
        "the first local mint after adoption must preserve the pre-restart protection"
    );

    let second_minted = sfu
        .issue_join_token(&call, &alice, MediaCapabilities::direct_call_peer())
        .expect("token");
    sfu.revoke_issued_token(&call, &alice, &second_minted.jti);

    let intents = reported.lock().expect("sink lock");
    assert_eq!(intents.len(), 1);
    assert!(matches!(
        intents[0].target,
        TeardownTargetLite::Participant { .. }
    ));
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
                let _ = sfu.unregister_call_participant(&call, &alice, None);
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
    let first_generation = stored_generation(&sfu, &call).expect("first generation");
    // Simulate the joiner landing inside alice's teardown window:
    // remove alice from the set (step 1 of clear_local_state),
    // register bob, then run the full unregister — the conditional
    // removal must observe bob and keep the entry.
    sfu.calls
        .get_mut(&call)
        .expect("entry exists")
        .participants
        .remove(&alice);
    sfu.register_call_participant(&call, &bob);
    assert!(
        stored_generation(&sfu, &call).expect("rejoin generation") > first_generation,
        "registering into an observed-empty entry must advance the generation"
    );

    let state = applied_state(sfu.unregister_call_participant(&call, &alice, None));
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

#[test]
fn final_direct_call_departure_retains_a_generation_tombstone() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("alice@waddle.social::dm-reap").unwrap();
    let alice = fixture_identity("alice");

    sfu.register_call_participant(&call, &alice);
    assert!(
        sfu.call_generations.contains_key(&call),
        "registration seeds the generation tracker"
    );

    assert_eq!(
        applied_state(sfu.unregister_call_participant(&call, &alice, None)),
        CallState::Ended
    );
    assert!(
        sfu.call_generations.contains_key(&call),
        "the last direct-call departure must retain a tombstone generation fence"
    );
    assert!(
        stored_generation_tombstone_cleared_at(&sfu, &call).is_some(),
        "the retained generation entry must record when the call cleared"
    );
}

#[test]
fn final_muc_departure_retains_generation_tracker() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("LiveKitSfu init in test");
    let call = CallId::new("room@muc.waddle.social").unwrap();
    let alice = fixture_identity("alice");

    sfu.register_call_participant(&call, &alice);
    assert!(
        sfu.call_generations.contains_key(&call),
        "registration seeds the generation tracker"
    );

    assert_eq!(
        applied_state(sfu.unregister_call_participant(&call, &alice, None)),
        CallState::Ended
    );
    assert!(
        sfu.call_generations.contains_key(&call),
        "MUC call ids are stable room names, so their generation fence must survive teardown"
    );
    assert!(
        stored_generation_tombstone_cleared_at(&sfu, &call).is_some(),
        "the retained generation entry must also record the clear time for MUC calls"
    );
}

#[tokio::test]
async fn reconcile_prunes_generation_tombstones_older_than_25_hours() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("alice@waddle.social::aged-tombstone").expect("call id");
    let alice = fixture_identity("alice");

    sfu.register_call_participant(&call, &alice);
    assert_eq!(
        applied_state(sfu.unregister_call_participant(&call, &alice, None)),
        CallState::Ended
    );
    sfu.call_generations
        .get_mut(&call)
        .expect("generation tombstone")
        .last_cleared_at = Some(Utc::now() - ChronoDuration::hours(26));

    let _summary = sfu.reconcile_active_calls(ChronoDuration::zero()).await;

    assert!(
        !sfu.call_generations.contains_key(&call),
        "reconcile must reap tombstones once they age past the 25h retention window"
    );
}

/// #1449 codex round 3: a degraded pass (listing failed) must NOT open
/// the startup fence — the fence exists to wait for an authoritative
/// SID/generation inventory.
#[tokio::test]
async fn degraded_reconcile_pass_keeps_the_startup_fence_closed() {
    let admin = Arc::new(RecordingAdmin::default());
    admin.fail_list_rooms();
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);

    let _ = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert!(
        !sfu.reconcile_pass_completed.load(Ordering::Acquire),
        "a pass without an authoritative listing must not open the fence"
    );

    *admin.list_rooms_errors.lock().expect("recording lock") = false;
    let _ = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert!(
        sfu.reconcile_pass_completed.load(Ordering::Acquire),
        "the first authoritative pass opens the fence"
    );
}

/// #1449 codex round 3: when one participant was already re-registered
/// (webhook beat the reconcile pass), the remaining LiveKit-live
/// identities must be merged into the existing entry instead of
/// staying invisible until another join signal.
#[tokio::test]
async fn partially_restored_call_merges_remaining_live_identities() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("merge-room@muc.waddle.social").expect("call id");
    let alice = Identity::from_jid("alice@waddle.social/web".parse().expect("jid"));
    let bob = Identity::from_jid("bob@waddle.social/web".parse().expect("jid"));
    let alice_sid = fixture_participant_sid("PA_alice_reconciled");
    let bob_sid = fixture_participant_sid("PA_bob_merged");

    // Alice's webhook re-registration arrived before the pass.
    sfu.register_call_participant(&call, &alice);
    admin.set_rooms(vec![crate::admin::ListedRoom {
        name: call.to_string(),
        sid: Some(RoomSid::new("RM_merge").expect("room sid")),
        num_participants: Some(2),
    }]);
    admin.set_live_with_sids(
        &call,
        vec![
            (alice.clone(), Some(alice_sid.clone())),
            (bob.clone(), Some(bob_sid.clone())),
        ],
    );

    let pass = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert_eq!(
        pass.rooms_adopted, 0,
        "existing entries are merged, not adopted"
    );
    assert!(sfu.has_call_participant(&call, &alice));
    assert!(
        sfu.has_call_participant(&call, &bob),
        "live identities missing from a partially restored entry must be merged"
    );
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        Some(alice_sid),
        "reconciliation must teach a listed SID to an already-restored participant"
    );
    assert_eq!(
        stored_participant_sid(&sfu, &call, &bob),
        Some(bob_sid),
        "a merged participant must retain the SID returned by ListParticipants"
    );
}

#[test]
fn reconciliation_listing_never_regresses_an_already_known_participant_sid() {
    let sfu = LiveKitSfu::new(fixture_config()).expect("test SFU");
    let call = CallId::new("merge-sid-race@muc.waddle.social").expect("call id");
    let alice = fixture_identity("alice");
    let current = observed_sids(Some("RM_current"), Some("PA_new"));
    let stale_listed_sid = fixture_participant_sid("PA_old");

    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &current),
        SidObservationDisposition::Applied
    );
    assert_eq!(
        sfu.merge_live_identities(
            &call,
            &[(alice.clone(), Some(stale_listed_sid))],
            Utc::now(),
        ),
        0
    );
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        current.participant_sid,
        "an in-flight reconciliation listing must not overwrite a newer join SID"
    );
}

#[tokio::test]
async fn adopted_participant_sid_makes_fenced_removal_decidable() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), Arc::clone(&admin) as Arc<_>);
    let call = CallId::new("adopt-sid@muc.waddle.social").expect("call id");
    let alice = Identity::from_jid("alice@waddle.social/web".parse().expect("jid"));
    let room_sid = fixture_room_sid("RM_adopt_sid");
    let participant_sid = fixture_participant_sid("PA_adopt_sid");

    admin.set_rooms(vec![crate::admin::ListedRoom {
        name: call.to_string(),
        sid: Some(room_sid.clone()),
        num_participants: Some(1),
    }]);
    admin.set_live_with_sids(&call, vec![(alice.clone(), Some(participant_sid.clone()))]);

    let pass = sfu.reconcile_active_calls(ChronoDuration::zero()).await;
    assert_eq!(pass.rooms_adopted, 1);
    assert_eq!(
        stored_participant_sid(&sfu, &call, &alice),
        Some(participant_sid.clone()),
        "adoption must carry the listed participant SID into restored state"
    );

    let intent = CallTeardownIntentLite {
        call_id: call.clone(),
        target: TeardownTargetLite::Participant {
            identity: alice.clone(),
            participant_sid: Some(participant_sid),
        },
        generation: stored_generation(&sfu, &call),
        room_sid: Some(room_sid),
    };
    assert_eq!(
        sfu.teardown_executor()
            .execute(&intent)
            .await
            .expect("SID-fenced removal resolves"),
        TeardownExecution::Executed,
        "the restored SID must prevent the teardown guard from returning Unresolved"
    );
    assert_eq!(admin.remove_snapshot(), vec![(call, alice)]);
}
