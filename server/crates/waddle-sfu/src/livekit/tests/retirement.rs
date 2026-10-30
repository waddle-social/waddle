use std::sync::Arc;

use super::{
    captured_registration, fixture_config, fixture_identity, fixture_occupancy_session,
    fixture_participant_sid, observed_sids, CallId, CallTeardownIntentLite, Identity, LiveKitSfu,
    MediaCapabilities, RecordingAdmin, Semaphore, SessionBinding, SfuService, StdDuration,
    TeardownExecution, TeardownTargetLite, UnboundOccupantPolicy,
};
use crate::JoinToken;
use crate::SfuReconciler;

#[tokio::test]
async fn older_poll_cannot_pair_a_newer_sid_with_a_displaced_generation() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(LiveKitSfu::with_admin(
        fixture_config(),
        admin.clone() as Arc<_>,
    ));
    let call = CallId::try_new("stale-generation-poll@muc.waddle.social").expect("call");
    let alice = fixture_identity("alice");
    let first = fixture_occupancy_session();
    let second = fixture_occupancy_session();
    let mut old = observed_sids(Some("RM_live"), Some("PA_old"));
    old.media_generation = Some(first);
    sfu.register_call_participant_observed(&call, &alice, &old);
    admin.live.lock().expect("provider state").insert(
        call.clone(),
        vec![crate::LiveParticipant {
            identity: alice.clone(),
            participant_sid: old.participant_sid,
            media_generation: Some(first),
        }],
    );
    admin.hold_list();
    let polling = Arc::clone(&sfu);
    let poll = tokio::spawn(async move {
        polling
            .reconcile_active_calls(chrono::Duration::zero())
            .await
    });
    admin.await_list_in_flight(StdDuration::from_secs(2)).await;
    let mut current = observed_sids(Some("RM_live"), Some("PA_current"));
    current.media_generation = Some(second);
    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &current),
        super::SidObservationDisposition::Applied
    );
    assert_eq!(
        sfu.participant_occupant_session(&call, &alice),
        None,
        "this is provider-restored media, not logical MUC ownership"
    );
    admin.release_list();
    poll.await.expect("older poll finishes");

    let retained = sfu
        .participant_media_evidence(&call, &alice)
        .expect("observation");
    assert_eq!(retained.participant_sid, current.participant_sid);
    assert_eq!(
        retained.media_generation,
        Some(second),
        "the SID and generation must remain one freshness-fenced observation"
    );

    // Lose the current leave webhook. A later ghost sweep must carry g2 to
    // the room actor, rather than leaving its Muji state stranded with g1.
    admin.list_gate.lock().expect("probe gate").take();
    admin.set_live(&call, Vec::new());
    assert!(sfu
        .reconcile_active_calls(chrono::Duration::zero())
        .await
        .swept
        .is_empty());
    let swept = sfu.reconcile_active_calls(chrono::Duration::zero()).await;
    assert_eq!(swept.swept.len(), 1);
    assert_eq!(
        swept.swept[0].observed_sids.participant_sid,
        current.participant_sid
    );
    assert_eq!(swept.swept[0].observed_sids.media_generation, Some(second));
}

#[tokio::test]
async fn fresh_poll_can_fill_missing_generation_for_the_same_sid_without_granting_occupancy() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = LiveKitSfu::with_admin(fixture_config(), admin.clone() as Arc<_>);
    let call = CallId::try_new("fill-media-evidence@muc.waddle.social").expect("call");
    let alice = fixture_identity("alice");
    let generation = fixture_occupancy_session();
    let observation = observed_sids(Some("RM_live"), Some("PA_same"));
    sfu.register_call_participant_observed(&call, &alice, &observation);
    admin.live.lock().expect("provider state").insert(
        call.clone(),
        vec![crate::LiveParticipant {
            identity: alice.clone(),
            participant_sid: observation.participant_sid.clone(),
            media_generation: Some(generation),
        }],
    );
    sfu.reconcile_active_calls(chrono::Duration::zero()).await;
    let retained = sfu
        .participant_media_evidence(&call, &alice)
        .expect("evidence");
    assert_eq!(retained.participant_sid, observation.participant_sid);
    assert_eq!(retained.media_generation, Some(generation));
    assert_eq!(sfu.participant_occupant_session(&call, &alice), None);
}

#[test]
fn takeover_persists_retirement_before_the_displaced_sid_is_known() {
    let reported = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&reported);
    let sfu = LiveKitSfu::try_new(fixture_config())
        .expect("SFU")
        .with_teardown_failure_sink(Arc::new(move |intent| {
            sink.lock().expect("sink").push(intent);
            Box::pin(async {})
        }));
    let call = CallId::try_new("late-sid@muc.waddle.social").expect("call");
    let alice = fixture_identity("alice");
    let displaced = fixture_occupancy_session();
    let publish = |generation| {
        sfu.issue_join_token_with_session(
            &call,
            &alice,
            MediaCapabilities::direct_call_peer(),
            &SessionBinding::try_new("session").expect("session"),
            generation,
            captured_registration(&sfu, &call, &alice),
        )
        .expect("mint")
        .expect("publish")
    };
    let old_token = publish(displaced);
    let successor_token = publish(fixture_occupancy_session());
    let intents = reported.lock().expect("intents");
    assert_eq!(
        intents.len(),
        1,
        "unknown SID must not lose durable retirement responsibility"
    );
    assert_eq!(intents[0].occupant_session, Some(displaced));
    assert_eq!(
        intents[0].target,
        TeardownTargetLite::DisplacedParticipant {
            identity: alice,
            participant_sid: None,
        }
    );
    assert!(sfu.is_revoked(&old_token.jti));
    assert!(!sfu.is_revoked(&successor_token.jti));
}

#[test]
fn tagged_join_requests_canonical_validation_even_without_a_cached_occupancy() {
    let reported = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&reported);
    let sfu = LiveKitSfu::try_new(fixture_config())
        .expect("SFU")
        .with_teardown_failure_sink(Arc::new(move |intent| {
            sink.lock().expect("sink").push(intent);
            Box::pin(async {})
        }));
    let call = CallId::try_new("late-replay-after-restart@muc.waddle.social").expect("call");
    let alice = fixture_identity("alice");
    let generation = fixture_occupancy_session();
    let mut observation = observed_sids(Some("RM_live"), Some("PA_replayed"));
    observation.media_generation = Some(generation);
    sfu.register_call_participant_observed(&call, &alice, &observation);
    assert_eq!(sfu.participant_occupant_session(&call, &alice), None);
    let intents = reported.lock().expect("intents");
    assert_eq!(
        intents.len(),
        1,
        "only canonical room ownership can classify a restored join"
    );
    assert_eq!(intents[0].occupant_session, Some(generation));
}

#[test]
fn tagged_restoration_does_not_authorize_unconfirmed_keep_cleanup() {
    let sfu = LiveKitSfu::try_new(fixture_config()).expect("SFU");
    let call = CallId::try_new("restored-unconfirmed-cleanup@muc.waddle.social").expect("call");
    let alice = fixture_identity("alice");
    let generation = fixture_occupancy_session();
    let mut observation = observed_sids(Some("RM_live"), None);
    observation.media_generation = Some(generation);
    sfu.register_call_participant_observed(&call, &alice, &observation);
    assert_eq!(
        sfu.unregister_call_participant_if_occupant_matches(
            &call,
            &alice,
            generation,
            UnboundOccupantPolicy::Keep,
            crate::SidEvidence::NotSignaling,
            None,
        ),
        super::SessionScopedTeardown::SessionMismatch,
        "a provider tag cannot promote an unconfirmed cleanup into logical occupancy authority"
    );
    assert!(sfu.has_call_participant(&call, &alice));
}

#[test]
fn restored_media_is_retired_without_promoting_its_generation_to_muc_authority() {
    let reported = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&reported);
    let sfu = LiveKitSfu::try_new(fixture_config())
        .expect("SFU")
        .with_teardown_failure_sink(Arc::new(move |intent| {
            sink.lock().expect("sink").push(intent);
            Box::pin(async {})
        }));
    let call = CallId::try_new("restored-replacement@muc.waddle.social").expect("call");
    let alice = fixture_identity("alice");
    let displaced = fixture_occupancy_session();
    let successor = fixture_occupancy_session();
    let mut observation = observed_sids(Some("RM_live"), Some("PA_restored"));
    observation.media_generation = Some(displaced);
    sfu.register_call_participant_observed(&call, &alice, &observation);
    assert_eq!(
        sfu.participant_occupant_session(&call, &alice),
        None,
        "provider metadata is not MUC authority"
    );
    let token = sfu
        .issue_join_token_with_session(
            &call,
            &alice,
            MediaCapabilities::direct_call_peer(),
            &SessionBinding::try_new("successor").expect("session"),
            successor,
            captured_registration(&sfu, &call, &alice),
        )
        .expect("mint")
        .expect("publish");
    let intents = reported.lock().expect("intents");
    assert!(!intents.is_empty());
    let retirement = intents.last().expect("replacement retirement");
    assert_eq!(retirement.occupant_session, Some(displaced));
    assert_eq!(
        retirement.target,
        TeardownTargetLite::DisplacedParticipant {
            identity: alice.clone(),
            participant_sid: observation.participant_sid.clone(),
        }
    );
    assert_eq!(
        sfu.note_participant_left(&call, &alice, Some(&observation)),
        super::TeardownDisposition::StaleSid
    );
    assert_eq!(
        sfu.participant_occupant_session(&call, &alice),
        Some(successor)
    );
    assert!(!sfu.is_revoked(&token.jti));
}

#[test]
fn restored_matching_generation_preserves_genuine_resume() {
    let reported = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&reported);
    let sfu = LiveKitSfu::try_new(fixture_config())
        .expect("SFU")
        .with_teardown_failure_sink(Arc::new(move |intent| {
            sink.lock().expect("sink").push(intent);
            Box::pin(async {})
        }));
    let call = CallId::try_new("restored-resume@muc.waddle.social").expect("call");
    let alice = fixture_identity("alice");
    let generation = fixture_occupancy_session();
    let mut observation = observed_sids(Some("RM_live"), Some("PA_preserved"));
    observation.media_generation = Some(generation);
    sfu.register_call_participant_observed(&call, &alice, &observation);
    let before = reported.lock().expect("intents").len();
    let token = sfu
        .issue_join_token_with_session(
            &call,
            &alice,
            MediaCapabilities::direct_call_peer(),
            &SessionBinding::try_new("resumed").expect("session"),
            generation,
            captured_registration(&sfu, &call, &alice),
        )
        .expect("mint")
        .expect("publish");
    assert_eq!(
        reported.lock().expect("intents").len(),
        before,
        "genuine resume creates no new displacement"
    );
    assert_eq!(
        sfu.participant_occupant_session(&call, &alice),
        Some(generation)
    );
    assert!(!sfu.is_revoked(&token.jti));
    assert!(
        matches!(
            sfu.note_participant_left(&call, &alice, Some(&observation)),
            super::TeardownDisposition::Applied(super::CallState::Ended)
        ),
        "the preserved live SID remains authoritative for this generation"
    );
}

#[test]
fn matching_generation_reconnects_have_no_permanent_retired_sid_mode() {
    let sfu = LiveKitSfu::try_new(fixture_config()).expect("SFU");
    let call = CallId::try_new("ordinary-after-replacement@muc.waddle.social").expect("call");
    let alice = fixture_identity("alice");
    let publish = |generation| {
        sfu.issue_join_token_with_session(
            &call,
            &alice,
            MediaCapabilities::direct_call_peer(),
            &SessionBinding::try_new("session").expect("session"),
            generation,
            captured_registration(&sfu, &call, &alice),
        )
        .expect("mint")
        .expect("publish")
    };
    let first = fixture_occupancy_session();
    publish(first);
    let mut old = observed_sids(Some("RM_live"), Some("PA_old"));
    old.media_generation = Some(first);
    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &old),
        super::SidObservationDisposition::Applied
    );
    let successor = fixture_occupancy_session();
    let token = publish(successor);
    for sid in ["PA_successor", "PA_reconnected"] {
        let mut current = observed_sids(Some("RM_live"), Some(sid));
        current.media_generation = Some(successor);
        assert_eq!(
            sfu.register_call_participant_observed(&call, &alice, &current),
            super::SidObservationDisposition::Applied
        );
    }
    assert_eq!(
        sfu.note_participant_left(&call, &alice, Some(&old)),
        super::TeardownDisposition::StaleSid
    );
    assert!(!sfu.is_revoked(&token.jti));
    let mut current = observed_sids(Some("RM_live"), Some("PA_reconnected"));
    current.media_generation = Some(successor);
    assert!(matches!(
        sfu.note_participant_left(&call, &alice, Some(&current)),
        super::TeardownDisposition::Applied(super::CallState::Ended)
    ));
}

#[test]
fn three_generations_cannot_restore_a_forgotten_participant_sid_from_an_unordered_webhook() {
    let sfu = LiveKitSfu::try_new(fixture_config()).expect("SFU");
    let call = CallId::try_new("three-generations@muc.waddle.social").expect("call");
    let alice = fixture_identity("alice");
    let publish = |generation| {
        sfu.issue_join_token_with_session(
            &call,
            &alice,
            MediaCapabilities::direct_call_peer(),
            &SessionBinding::try_new("session").expect("session"),
            generation,
            captured_registration(&sfu, &call, &alice),
        )
        .expect("mint")
        .expect("publish")
    };
    let first_generation = fixture_occupancy_session();
    publish(first_generation);
    let mut first = observed_sids(Some("RM_live"), Some("PA_first"));
    first.observed_event_at = Some(super::Utc::now());
    first.media_generation = Some(first_generation);
    sfu.register_call_participant_observed(&call, &alice, &first);
    let second_generation = fixture_occupancy_session();
    publish(second_generation);
    let mut second = observed_sids(Some("RM_live"), Some("PA_second"));
    second.media_generation = Some(second_generation);
    second.observed_event_at = first
        .observed_event_at
        .map(|at| at + chrono::Duration::seconds(1));
    sfu.register_call_participant_observed(&call, &alice, &second);
    let token = publish(fixture_occupancy_session());
    first.observed_event_at = None;
    sfu.register_call_participant_observed(&call, &alice, &first);
    assert_eq!(
        sfu.note_participant_left(&call, &alice, Some(&first)),
        super::TeardownDisposition::StaleSid
    );
    assert!(sfu.has_call_participant(&call, &alice));
    assert!(!sfu.is_revoked(&token.jti));
}

#[test]
fn occupancy_replacement_collects_expired_revocation_entries() {
    let mut config = fixture_config();
    config.token_ttl = chrono::Duration::seconds(-1);
    let sfu = LiveKitSfu::try_new(config).expect("SFU");
    let call = CallId::try_new("expired-retirement@muc.waddle.social").expect("call");
    let alice = fixture_identity("alice");
    for _ in 0..3 {
        sfu.issue_join_token_with_session(
            &call,
            &alice,
            MediaCapabilities::direct_call_peer(),
            &SessionBinding::try_new("session").expect("session"),
            fixture_occupancy_session(),
            captured_registration(&sfu, &call, &alice),
        )
        .expect("mint")
        .expect("publish");
        assert_eq!(
            sfu.revoked_count(),
            0,
            "expired credentials must not accumulate during replacement"
        );
    }
}

struct RetirementFixture {
    sfu: LiveKitSfu,
    admin: Arc<RecordingAdmin>,
    call: CallId,
    alice: Identity,
    successor_token: JoinToken,
    successor_generation: waddle_xmpp_core::OccupancySessionGeneration,
    intent: CallTeardownIntentLite,
}

impl RetirementFixture {
    fn new(live_sid: Option<&str>) -> Self {
        let admin = Arc::new(RecordingAdmin::default());
        let mut sfu = LiveKitSfu::with_admin(fixture_config(), admin.clone() as Arc<_>);
        // Exercise the durable executor explicitly; do not race an inline task.
        sfu.admin_permits = Arc::new(Semaphore::new(0));
        let call = CallId::try_new("retirement@muc.waddle.social").expect("call");
        let alice = fixture_identity("alice");
        let old_generation = fixture_occupancy_session();
        let old_session = SessionBinding::try_new("old-session").expect("session");
        sfu.issue_join_token_with_session(
            &call,
            &alice,
            MediaCapabilities::direct_call_peer(),
            &old_session,
            old_generation,
            captured_registration(&sfu, &call, &alice),
        )
        .expect("old mint")
        .expect("old publication");
        let mut old = observed_sids(Some("RM_live"), Some("PA_old"));
        old.media_generation = Some(old_generation);
        sfu.register_call_participant_observed(&call, &alice, &old);
        let successor_generation = fixture_occupancy_session();
        let successor_token = sfu
            .issue_join_token_with_session(
                &call,
                &alice,
                MediaCapabilities::direct_call_peer(),
                &SessionBinding::try_new("new-session").expect("session"),
                successor_generation,
                captured_registration(&sfu, &call, &alice),
            )
            .expect("successor mint")
            .expect("successor publication");
        admin.live.lock().expect("provider state").insert(
            call.clone(),
            vec![crate::LiveParticipant {
                identity: alice.clone(),
                participant_sid: live_sid.map(fixture_participant_sid),
                media_generation: Some(if live_sid == Some("PA_successor") {
                    successor_generation
                } else {
                    old_generation
                }),
            }],
        );
        let intent = CallTeardownIntentLite {
            call_id: call.clone(),
            target: TeardownTargetLite::DisplacedParticipant {
                identity: alice.clone(),
                participant_sid: Some(fixture_participant_sid("PA_old")),
            },
            generation: sfu.teardown_executor().current_generation(&call),
            room_sid: old.room_sid,
            occupant_session: Some(old_generation),
            unbound_occupant: UnboundOccupantPolicy::Keep,
            session: Some(old_session),
        };
        Self {
            sfu,
            admin,
            call,
            alice,
            successor_token,
            successor_generation,
            intent,
        }
    }

    fn assert_successor_survives(&self) {
        assert!(self.sfu.has_call_participant(&self.call, &self.alice));
        assert!(!self.sfu.is_revoked(&self.successor_token.jti));
        assert!(self.admin.delete_snapshot().is_empty());
    }
}

#[tokio::test]
async fn retirement_removes_the_old_media_when_the_successor_never_connects() {
    let fixture = RetirementFixture::new(Some("PA_old"));
    assert_eq!(
        fixture
            .sfu
            .teardown_executor()
            .execute_with_occupancy_authority(&fixture.intent, || std::future::ready(
                crate::OccupancyAuthority::Current(fixture.successor_generation)
            ))
            .await
            .expect("execute"),
        TeardownExecution::Executed
    );
    assert_eq!(
        fixture.admin.remove_snapshot(),
        vec![(fixture.call.clone(), fixture.alice.clone())]
    );
    fixture.assert_successor_survives();
}

#[tokio::test]
async fn retirement_skips_the_connected_successor_even_before_its_webhook() {
    let fixture = RetirementFixture::new(Some("PA_successor"));
    assert_eq!(
        fixture
            .sfu
            .teardown_executor()
            .execute_with_occupancy_authority(&fixture.intent, || std::future::ready(
                crate::OccupancyAuthority::Current(fixture.successor_generation)
            ))
            .await
            .expect("execute"),
        TeardownExecution::StaleGeneration
    );
    assert!(fixture.admin.remove_snapshot().is_empty());
    fixture.assert_successor_survives();
}

#[tokio::test]
async fn retirement_defers_when_live_participant_sid_is_unknown() {
    let fixture = RetirementFixture::new(None);
    assert_eq!(
        fixture
            .sfu
            .teardown_executor()
            .execute_with_occupancy_authority(&fixture.intent, || std::future::ready(
                crate::OccupancyAuthority::Current(fixture.successor_generation)
            ))
            .await
            .expect("execute"),
        TeardownExecution::Occupied
    );
    assert!(fixture.admin.remove_snapshot().is_empty());
    fixture.assert_successor_survives();
}

#[tokio::test]
async fn retirement_retries_a_failed_occupancy_probe_without_removing_by_identity() {
    let fixture = RetirementFixture::new(Some("PA_old"));
    fixture.admin.fail_list();
    assert!(fixture
        .sfu
        .teardown_executor()
        .execute_with_occupancy_authority(&fixture.intent, || std::future::ready(
            crate::OccupancyAuthority::Current(fixture.successor_generation)
        ))
        .await
        .is_err());
    assert!(fixture.admin.remove_snapshot().is_empty());
    fixture.admin.set_list_failing(false);
    assert_eq!(
        fixture
            .sfu
            .teardown_executor()
            .execute_with_occupancy_authority(&fixture.intent, || std::future::ready(
                crate::OccupancyAuthority::Current(fixture.successor_generation)
            ))
            .await
            .expect("retry"),
        TeardownExecution::Executed
    );
    fixture.assert_successor_survives();
}

#[tokio::test]
async fn retirement_completes_without_removal_when_the_old_participant_is_already_gone() {
    let fixture = RetirementFixture::new(Some("PA_old"));
    fixture.admin.set_live(&fixture.call, Vec::new());
    assert_eq!(
        fixture
            .sfu
            .teardown_executor()
            .execute_with_occupancy_authority(&fixture.intent, || std::future::ready(
                crate::OccupancyAuthority::Current(fixture.successor_generation)
            ))
            .await
            .expect("execute"),
        TeardownExecution::Executed
    );
    assert!(fixture.admin.remove_snapshot().is_empty());
    fixture.assert_successor_survives();
}

#[tokio::test]
async fn retirement_rechecks_a_matching_generation_successor_observation_during_a_probe() {
    let fixture = RetirementFixture::new(Some("PA_old"));
    fixture.admin.hold_list();
    let executor = fixture.sfu.teardown_executor();
    let intent = fixture.intent.clone();
    let current = fixture.successor_generation;
    let execution = tokio::spawn(async move {
        executor
            .execute_with_occupancy_authority(&intent, || {
                std::future::ready(crate::OccupancyAuthority::Current(current))
            })
            .await
    });
    fixture
        .admin
        .await_list_in_flight(StdDuration::from_secs(2))
        .await;
    // The probe still returns PA_old, but a newer local observation wins.
    let mut observation = observed_sids(Some("RM_live"), Some("PA_successor"));
    observation.media_generation = Some(fixture.successor_generation);
    fixture
        .sfu
        .register_call_participant_observed(&fixture.call, &fixture.alice, &observation);
    fixture.admin.release_list();
    assert_eq!(
        execution.await.expect("task").expect("execute"),
        TeardownExecution::StaleGeneration
    );
    assert!(fixture.admin.remove_snapshot().is_empty());
    fixture.assert_successor_survives();
}

#[tokio::test]
async fn reconciliation_does_not_adopt_displaced_media_after_an_unattributed_join() {
    let fixture = RetirementFixture::new(Some("PA_old"));
    fixture.sfu.register_call_participant_observed(
        &fixture.call,
        &fixture.alice,
        &observed_sids(Some("RM_live"), Some("PA_unordered")),
    );
    fixture
        .sfu
        .reconcile_active_calls(chrono::Duration::seconds(120))
        .await;
    assert_eq!(
        fixture
            .sfu
            .teardown_executor()
            .execute_with_occupancy_authority(&fixture.intent, || std::future::ready(
                crate::OccupancyAuthority::Current(fixture.successor_generation)
            ))
            .await
            .expect("execute"),
        TeardownExecution::Executed
    );
    assert_eq!(fixture.admin.remove_snapshot().len(), 1);
    fixture.assert_successor_survives();
}

#[tokio::test]
async fn sidless_old_webhook_cannot_clear_the_successor_but_its_own_departure_can() {
    let fixture = RetirementFixture::new(Some("PA_old"));
    let no_participant_sid = observed_sids(Some("RM_live"), None);
    assert_eq!(
        fixture.sfu.observe_call_participant_sids(
            &fixture.call,
            &fixture.alice,
            Some(&no_participant_sid),
            super::SidObservationDirection::Leave,
        ),
        super::SidObservationDisposition::StaleSid
    );
    assert_eq!(
        fixture
            .sfu
            .note_participant_left(&fixture.call, &fixture.alice, Some(&no_participant_sid)),
        super::TeardownDisposition::StaleSid
    );
    fixture.assert_successor_survives();
    let successor = fixture
        .sfu
        .participant_occupant_session(&fixture.call, &fixture.alice)
        .expect("occupant");
    assert!(matches!(
        fixture.sfu.note_participant_left_if_occupant_matches(
            &fixture.call,
            &fixture.alice,
            Some(&no_participant_sid),
            successor,
            UnboundOccupantPolicy::Keep,
            crate::SidEvidence::NotSignaling,
        ),
        super::SessionScopedTeardown::Applied(super::TeardownDisposition::Applied(
            super::CallState::Ended
        ))
    ));
}
