use super::super::{fixture_config, fixture_identity, fixture_occupancy_session, observed_sids};
use super::RetirementFixture;
use crate::{
    CallId, CallState, LiveKitSfu, SessionScopedTeardown, SfuReconciler, SfuService, SidEvidence,
    SidObservationDirection, SidObservationDisposition, TeardownDisposition, UnboundOccupantPolicy,
};

#[tokio::test]
async fn sid_bearing_untagged_old_leave_cannot_retire_an_unobserved_successor() {
    let fixture = RetirementFixture::new(Some("PA_old"));
    // The queued departure preserved the old SID but lost its provider tag.
    let old_leave = observed_sids(Some("RM_live"), Some("PA_old"));
    assert_eq!(
        fixture.sfu.observe_call_participant_sids(
            &fixture.call,
            &fixture.alice,
            Some(&old_leave),
            SidObservationDirection::Leave,
        ),
        SidObservationDisposition::StaleSid,
    );
    assert_eq!(
        fixture
            .sfu
            .note_participant_left(&fixture.call, &fixture.alice, Some(&old_leave)),
        TeardownDisposition::StaleSid,
    );
    fixture.assert_successor_survives();
    assert!(!fixture
        .sfu
        .has_pending_revocation_eject(&fixture.call, &fixture.alice));

    let mut joined = observed_sids(Some("RM_live"), Some("PA_successor"));
    joined.media_generation = Some(fixture.successor_generation);
    assert_eq!(
        fixture
            .sfu
            .register_call_participant_observed(&fixture.call, &fixture.alice, &joined),
        SidObservationDisposition::Applied,
    );
    fixture.assert_successor_survives();
    assert!(!fixture
        .sfu
        .has_pending_revocation_eject(&fixture.call, &fixture.alice));
}

#[tokio::test]
async fn legitimate_unobserved_successor_leave_defers_to_confirmed_ghost_cleanup() {
    let fixture = RetirementFixture::new(Some("PA_successor"));
    let mut unobserved_leave = observed_sids(Some("RM_live"), Some("PA_successor"));
    unobserved_leave.media_generation = Some(fixture.successor_generation);
    assert_eq!(
        fixture
            .sfu
            .note_participant_left(&fixture.call, &fixture.alice, Some(&unobserved_leave)),
        TeardownDisposition::StaleSid,
        "a leave cannot establish the missing first provider SID, even for the real successor",
    );
    fixture.assert_successor_survives();
    fixture.admin.set_live(&fixture.call, Vec::new());
    assert!(fixture
        .sfu
        .reconcile_active_calls(chrono::Duration::zero())
        .await
        .swept
        .is_empty());
    let confirmed = fixture
        .sfu
        .reconcile_active_calls(chrono::Duration::zero())
        .await;
    assert_eq!(
        confirmed.swept,
        vec![(fixture.call.clone(), fixture.alice.clone())]
    );
    assert!(!fixture
        .sfu
        .has_call_participant(&fixture.call, &fixture.alice));
    assert!(fixture.sfu.is_revoked(&fixture.successor_token.jti));
}

#[tokio::test]
async fn caller_scoped_hangup_still_retires_an_unobserved_bound_registration() {
    for occupant_scoped in [false, true] {
        let fixture = RetirementFixture::new(Some("PA_successor"));
        let mut leave = observed_sids(Some("RM_live"), Some("PA_successor"));
        leave.media_generation = Some(fixture.successor_generation);
        let session = fixture
            .sfu
            .participant_session_binding(&fixture.call, &fixture.alice)
            .expect("session");
        let outcome = if occupant_scoped {
            fixture.sfu.note_participant_left_if_occupant_matches(
                &fixture.call,
                &fixture.alice,
                Some(&leave),
                fixture.successor_generation,
                UnboundOccupantPolicy::Keep,
                SidEvidence::Presented(Some(&session)),
            )
        } else {
            fixture.sfu.note_participant_left_if_session_matches(
                &fixture.call,
                &fixture.alice,
                Some(&leave),
                Some(&session),
            )
        };
        assert_eq!(
            outcome,
            SessionScopedTeardown::Applied(TeardownDisposition::Applied(CallState::Ended))
        );
        assert!(!fixture
            .sfu
            .has_call_participant(&fixture.call, &fixture.alice));
        assert!(fixture.sfu.is_revoked(&fixture.successor_token.jti));
    }
}

#[test]
fn unbound_legacy_registration_still_accepts_its_real_sid_leave() {
    let sfu = LiveKitSfu::try_new(fixture_config()).expect("SFU");
    let call = CallId::try_new("legacy-real-leave@muc.example.com").expect("call");
    let alice = fixture_identity("alice");
    let observation = observed_sids(Some("RM_legacy"), Some("PA_legacy"));
    assert_eq!(
        sfu.register_call_participant_observed(&call, &alice, &observation),
        SidObservationDisposition::Applied
    );
    assert_eq!(sfu.participant_occupant_session(&call, &alice), None);
    assert_eq!(
        sfu.note_participant_left(&call, &alice, Some(&observation)),
        TeardownDisposition::Applied(CallState::Ended)
    );
    assert!(!sfu.has_call_participant(&call, &alice));
    // A provider-restored tag also remains media evidence rather than a logical seat.
    let mut tagged = observation;
    tagged.media_generation = Some(fixture_occupancy_session());
    sfu.register_call_participant_observed(&call, &alice, &tagged);
    assert_eq!(sfu.participant_occupant_session(&call, &alice), None);
    assert_eq!(
        sfu.note_participant_left(&call, &alice, Some(&tagged)),
        TeardownDisposition::Applied(CallState::Ended)
    );
}
