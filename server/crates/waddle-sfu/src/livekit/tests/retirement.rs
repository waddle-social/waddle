use std::sync::Arc;

use super::{
    captured_registration, fixture_config, fixture_identity, fixture_occupancy_session,
    fixture_participant_sid, observed_sids, CallId, CallTeardownIntentLite, Identity, LiveKitSfu,
    MediaCapabilities, RecordingAdmin, Semaphore, SessionBinding, SfuService, StdDuration,
    TeardownExecution, TeardownTargetLite, UnboundOccupantPolicy,
};
use crate::JoinToken;
use crate::SfuReconciler;

#[test]
fn three_generations_cannot_restore_a_forgotten_participant_sid_from_an_unordered_webhook() {
    let sfu = LiveKitSfu::try_new(fixture_config()).expect("SFU");
    let call = CallId::try_new("three-generations@muc.waddle.social").expect("call");
    let alice = fixture_identity("alice");
    let publish = || {
        sfu.issue_join_token_with_session(
            &call,
            &alice,
            MediaCapabilities::direct_call_peer(),
            &SessionBinding::try_new("session").expect("session"),
            fixture_occupancy_session(),
            captured_registration(&sfu, &call, &alice),
        )
        .expect("mint")
        .expect("publish")
    };
    publish();
    let mut first = observed_sids(Some("RM_live"), Some("PA_first"));
    first.observed_event_at = Some(super::Utc::now());
    sfu.register_call_participant_observed(&call, &alice, &first);
    publish();
    let mut second = observed_sids(Some("RM_live"), Some("PA_second"));
    second.observed_event_at = first
        .observed_event_at
        .map(|at| at + chrono::Duration::seconds(1));
    sfu.register_call_participant_observed(&call, &alice, &second);
    let token = publish();
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
        let old = observed_sids(Some("RM_live"), Some("PA_old"));
        sfu.register_call_participant_observed(&call, &alice, &old);
        let successor_token = sfu
            .issue_join_token_with_session(
                &call,
                &alice,
                MediaCapabilities::direct_call_peer(),
                &SessionBinding::try_new("new-session").expect("session"),
                fixture_occupancy_session(),
                captured_registration(&sfu, &call, &alice),
            )
            .expect("successor mint")
            .expect("successor publication");
        admin.set_live_with_sids(
            &call,
            vec![(alice.clone(), live_sid.map(fixture_participant_sid))],
        );
        let intent = CallTeardownIntentLite {
            call_id: call.clone(),
            target: TeardownTargetLite::DisplacedParticipant {
                identity: alice.clone(),
                participant_sid: fixture_participant_sid("PA_old"),
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
            .execute(&fixture.intent)
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
            .execute(&fixture.intent)
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
            .execute(&fixture.intent)
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
        .execute(&fixture.intent)
        .await
        .is_err());
    assert!(fixture.admin.remove_snapshot().is_empty());
    fixture.admin.set_list_failing(false);
    assert_eq!(
        fixture
            .sfu
            .teardown_executor()
            .execute(&fixture.intent)
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
            .execute(&fixture.intent)
            .await
            .expect("execute"),
        TeardownExecution::Executed
    );
    assert!(fixture.admin.remove_snapshot().is_empty());
    fixture.assert_successor_survives();
}

#[tokio::test]
async fn retirement_defers_an_unordered_successor_observation_during_a_probe() {
    let fixture = RetirementFixture::new(Some("PA_old"));
    fixture.admin.hold_list();
    let executor = fixture.sfu.teardown_executor();
    let intent = fixture.intent.clone();
    let execution = tokio::spawn(async move { executor.execute(&intent).await });
    fixture
        .admin
        .await_list_in_flight(StdDuration::from_secs(2))
        .await;
    // The probe still returns PA_old, but a newer local observation wins.
    fixture.sfu.register_call_participant_observed(
        &fixture.call,
        &fixture.alice,
        &observed_sids(Some("RM_live"), Some("PA_successor")),
    );
    fixture.admin.release_list();
    assert_eq!(
        execution.await.expect("task").expect("execute"),
        TeardownExecution::Occupied
    );
    assert!(fixture.admin.remove_snapshot().is_empty());
    fixture.assert_successor_survives();
}

#[tokio::test]
async fn reconciliation_resolves_an_unordered_join_when_the_retired_participant_remains_live() {
    let fixture = RetirementFixture::new(Some("PA_old"));
    fixture.sfu.register_call_participant_observed(
        &fixture.call,
        &fixture.alice,
        &observed_sids(Some("RM_live"), Some("PA_unordered")),
    );
    assert_eq!(
        fixture
            .sfu
            .teardown_executor()
            .execute(&fixture.intent)
            .await
            .expect("defer"),
        TeardownExecution::Occupied
    );
    fixture
        .sfu
        .reconcile_active_calls(chrono::Duration::seconds(120))
        .await;
    assert_eq!(
        fixture
            .sfu
            .teardown_executor()
            .execute(&fixture.intent)
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
