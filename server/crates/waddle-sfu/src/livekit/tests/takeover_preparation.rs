use std::sync::{Arc, Mutex};

use super::{
    captured_registration, fixture_config, fixture_identity, fixture_occupancy_session,
    fixture_participant_sid, observed_sids, CallId, CallTeardownIntentLite, Identity, LiveKitSfu,
    MediaCapabilities, RecordingAdmin, SessionBinding, SfuService, StdDuration, TeardownTargetLite,
};
use crate::{JoinToken, ParticipantRegistrationExpectation, SfuReconciler};
use waddle_xmpp_core::OccupancySessionGeneration;

struct TakeoverFixture {
    sfu: Arc<LiveKitSfu>,
    admin: Arc<RecordingAdmin>,
    reported: Arc<Mutex<Vec<CallTeardownIntentLite>>>,
    report_signal: Arc<tokio::sync::Notify>,
    call: CallId,
    alice: Identity,
}

impl TakeoverFixture {
    fn new() -> Self {
        let admin = Arc::new(RecordingAdmin::default());
        let reported = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&reported);
        let report_signal = Arc::new(tokio::sync::Notify::new());
        let signal = Arc::clone(&report_signal);
        let sfu = Arc::new(
            LiveKitSfu::with_admin(fixture_config(), admin.clone() as Arc<_>)
                .with_teardown_failure_sink(Arc::new(move |intent| {
                    sink.lock().expect("sink").push(intent);
                    signal.notify_one();
                    Box::pin(async {})
                })),
        );
        let call = CallId::try_new("prepared-takeover@muc.waddle.social").expect("call");
        let alice = fixture_identity("alice");
        admin.set_rooms(vec![crate::ListedRoom {
            name: crate::admin::ListedRoomName::Waddle(call.clone()),
            sid: observed_sids(Some("RM_live"), None).room_sid,
            num_participants: Some(1),
        }]);
        Self {
            sfu,
            admin,
            reported,
            report_signal,
            call,
            alice,
        }
    }

    async fn wait_for_retirements(&self, expected: usize) {
        tokio::time::timeout(StdDuration::from_secs(2), async {
            loop {
                let reported = self.report_signal.notified();
                if self.reported.lock().expect("intents").len() >= expected {
                    break;
                }
                reported.await;
            }
        })
        .await
        .expect("published retirement must reach its durable reporter");
    }

    fn set_media(&self, generation: OccupancySessionGeneration, sid: &str) {
        self.admin.live.lock().expect("provider state").insert(
            self.call.clone(),
            vec![crate::LiveParticipant {
                identity: self.alice.clone(),
                participant_sid: Some(fixture_participant_sid(sid)),
                media_generation: Some(generation),
            }],
        );
    }

    fn publish(
        &self,
        generation: OccupancySessionGeneration,
        expected: ParticipantRegistrationExpectation,
    ) -> JoinToken {
        self.sfu
            .issue_join_token_with_session(
                &self.call,
                &self.alice,
                MediaCapabilities::direct_call_peer(),
                &SessionBinding::try_new("prepared-session").expect("session"),
                generation,
                expected,
            )
            .expect("mint")
            .expect("publish")
    }

    fn start_preparation(
        &self,
    ) -> tokio::task::JoinHandle<Option<ParticipantRegistrationExpectation>> {
        let sfu = Arc::clone(&self.sfu);
        let call = self.call.clone();
        let identity = self.alice.clone();
        tokio::spawn(async move { sfu.prepare_participant_registration(&call, &identity).await })
    }
}

#[tokio::test]
async fn takeover_after_empty_cache_restart_discovers_connected_displaced_media() {
    let fixture = TakeoverFixture::new();
    let old_generation = fixture_occupancy_session();
    fixture.set_media(old_generation, "PA_old");
    assert!(!fixture
        .sfu
        .has_call_participant(&fixture.call, &fixture.alice));

    let prepared = fixture
        .sfu
        .prepare_participant_registration(&fixture.call, &fixture.alice)
        .await
        .expect("confirmed provider evidence");
    assert!(fixture.reported.lock().expect("intents").is_empty());
    assert_eq!(
        fixture
            .sfu
            .participant_occupant_session(&fixture.call, &fixture.alice),
        None,
        "provider discovery does not grant MUC occupancy"
    );
    let successor = fixture.publish(fixture_occupancy_session(), prepared);
    fixture.wait_for_retirements(1).await;
    let intents = fixture.reported.lock().expect("intents");
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].occupant_session, Some(old_generation));
    assert_eq!(
        intents[0].target,
        TeardownTargetLite::DisplacedParticipant {
            identity: fixture.alice.clone(),
            participant_sid: Some(fixture_participant_sid("PA_old")),
        }
    );
    assert!(!fixture.sfu.is_revoked(&successor.jti));
}

#[tokio::test]
async fn takeover_targets_observed_media_and_revokes_logical_generation_separately() {
    let fixture = TakeoverFixture::new();
    let media_generation = fixture_occupancy_session();
    let logical_generation = fixture_occupancy_session();
    let old_token = fixture.publish(
        logical_generation,
        captured_registration(&fixture.sfu, &fixture.call, &fixture.alice),
    );
    fixture.set_media(media_generation, "PA_cached");
    let prepared = fixture
        .sfu
        .prepare_participant_registration(&fixture.call, &fixture.alice)
        .await
        .expect("provider snapshot");
    assert_eq!(
        fixture
            .sfu
            .participant_occupant_session(&fixture.call, &fixture.alice),
        Some(logical_generation)
    );
    assert!(fixture.reported.lock().expect("intents").is_empty());
    let successor = fixture.publish(fixture_occupancy_session(), prepared);
    fixture.wait_for_retirements(1).await;
    let intents = fixture.reported.lock().expect("intents");
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].occupant_session, Some(media_generation));
    assert_eq!(
        intents[0].target,
        TeardownTargetLite::DisplacedParticipant {
            identity: fixture.alice.clone(),
            participant_sid: Some(fixture_participant_sid("PA_cached")),
        }
    );
    assert!(fixture.sfu.is_revoked(&old_token.jti));
    assert!(!fixture.sfu.is_revoked(&successor.jti));
}

#[tokio::test]
async fn provider_failure_does_not_prepare_publication_or_change_authority() {
    for fail_room_listing in [false, true] {
        let fixture = TakeoverFixture::new();
        if fail_room_listing {
            fixture.admin.fail_list_rooms();
        } else {
            fixture.admin.fail_list();
        }
        assert!(fixture
            .sfu
            .prepare_participant_registration(&fixture.call, &fixture.alice)
            .await
            .is_none());
        assert!(!fixture
            .sfu
            .has_call_participant(&fixture.call, &fixture.alice));
        assert_eq!(fixture.sfu.issued_count(&fixture.call, &fixture.alice), 0);
        assert_eq!(fixture.sfu.revoked_count(), 0);
        assert!(fixture.reported.lock().expect("intents").is_empty());
    }
}

#[tokio::test]
async fn preparation_cannot_overwrite_a_concurrent_publication() {
    let fixture = TakeoverFixture::new();
    fixture.set_media(fixture_occupancy_session(), "PA_old");
    fixture.admin.hold_list();
    let preparing = fixture.start_preparation();
    fixture
        .admin
        .await_list_in_flight(StdDuration::from_secs(2))
        .await;

    let current_generation = fixture_occupancy_session();
    let current = fixture.publish(
        current_generation,
        captured_registration(&fixture.sfu, &fixture.call, &fixture.alice),
    );
    fixture.admin.release_list();
    assert!(preparing.await.expect("preparation task").is_none());
    assert_eq!(
        fixture
            .sfu
            .participant_occupant_session(&fixture.call, &fixture.alice),
        Some(current_generation)
    );
    assert!(!fixture.sfu.is_revoked(&current.jti));
    assert!(fixture.reported.lock().expect("intents").is_empty());
}

#[tokio::test]
async fn preparation_cannot_overwrite_a_fresh_same_sid_webhook() {
    let fixture = TakeoverFixture::new();
    let generation = fixture_occupancy_session();
    let mut observation = observed_sids(Some("RM_live"), Some("PA_same"));
    observation.media_generation = Some(generation);
    fixture
        .sfu
        .register_call_participant_observed(&fixture.call, &fixture.alice, &observation);
    fixture.set_media(generation, "PA_same");
    fixture.admin.hold_list();
    let preparing = fixture.start_preparation();
    fixture
        .admin
        .await_list_in_flight(StdDuration::from_secs(2))
        .await;

    assert_eq!(
        fixture
            .sfu
            .register_call_participant_observed(&fixture.call, &fixture.alice, &observation),
        super::SidObservationDisposition::Applied
    );
    fixture.admin.release_list();
    assert!(preparing.await.expect("preparation task").is_none());
    assert_eq!(
        fixture
            .sfu
            .calls
            .get(&fixture.call)
            .and_then(|entry| {
                entry
                    .participants
                    .get(&fixture.alice)
                    .map(|participant| participant.participant_sid.clone())
            })
            .expect("retained media observation"),
        observation.participant_sid
    );
    assert!(fixture.reported.lock().expect("intents").is_empty());
}

#[tokio::test]
async fn ghost_probe_cannot_sweep_a_registration_published_while_it_waited() {
    let fixture = TakeoverFixture::new();
    fixture.publish(
        fixture_occupancy_session(),
        captured_registration(&fixture.sfu, &fixture.call, &fixture.alice),
    );
    assert!(fixture
        .sfu
        .reconcile_active_calls(chrono::Duration::zero())
        .await
        .swept
        .is_empty());
    fixture.admin.hold_list();
    let polling_sfu = Arc::clone(&fixture.sfu);
    let polling = tokio::spawn(async move {
        polling_sfu
            .reconcile_active_calls(chrono::Duration::zero())
            .await
    });
    fixture
        .admin
        .await_list_in_flight(StdDuration::from_secs(2))
        .await;

    let current_generation = fixture_occupancy_session();
    let current = fixture.publish(
        current_generation,
        captured_registration(&fixture.sfu, &fixture.call, &fixture.alice),
    );
    fixture.admin.release_list();
    let summary = polling.await.expect("polling task");
    assert!(
        summary.swept.is_empty(),
        "a provider response started before this registration cannot prove it absent"
    );
    assert!(fixture
        .sfu
        .has_call_participant(&fixture.call, &fixture.alice));
    assert_eq!(
        fixture
            .sfu
            .participant_occupant_session(&fixture.call, &fixture.alice),
        Some(current_generation)
    );
    assert!(!fixture.sfu.is_revoked(&current.jti));
}

#[tokio::test]
async fn preparation_rejects_a_room_rotated_during_its_participant_probe() {
    let fixture = TakeoverFixture::new();
    let generation = fixture_occupancy_session();
    let room = |sid| crate::ListedRoom {
        name: crate::admin::ListedRoomName::Waddle(fixture.call.clone()),
        sid: observed_sids(Some(sid), None).room_sid,
        num_participants: Some(1),
    };
    fixture.admin.set_rooms(vec![room("RM_old")]);
    fixture.set_media(generation, "PA_old");
    fixture.admin.hold_list();
    let preparing = fixture.start_preparation();
    fixture
        .admin
        .await_list_in_flight(StdDuration::from_secs(2))
        .await;

    // ListParticipants is room-name keyed: it may answer for the successor
    // room even though the initial ListRooms described its predecessor.
    fixture.admin.set_rooms(vec![room("RM_new")]);
    fixture.set_media(generation, "PA_new");
    fixture.admin.release_list();
    assert!(preparing.await.expect("preparation task").is_none());
    assert!(
        !fixture
            .sfu
            .has_call_participant(&fixture.call, &fixture.alice),
        "never seed the successor participant under the predecessor room SID"
    );
    assert!(fixture.reported.lock().expect("intents").is_empty());
    assert_eq!(fixture.sfu.issued_count(&fixture.call, &fixture.alice), 0);
}
