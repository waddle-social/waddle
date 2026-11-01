use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex, Weak};

use super::{
    captured_registration, fixture_config, fixture_identity, fixture_occupancy_session, CallId,
    Identity, LiveKitSfu, MediaCapabilities, RecordingAdmin, SessionBinding, SfuService,
    StdDuration, TeardownTargetLite,
};
use crate::{JoinToken, PendingMediaRetirement, TeardownFailureSink};
use waddle_xmpp_core::OccupancySessionGeneration;

struct ReceiptFixture {
    sfu: Arc<LiveKitSfu>,
    call: CallId,
    identity: Identity,
}

impl ReceiptFixture {
    fn new(sink: Option<TeardownFailureSink>) -> Self {
        let mut sfu = LiveKitSfu::with_admin(
            fixture_config(),
            Arc::new(RecordingAdmin::default()) as Arc<_>,
        );
        if let Some(sink) = sink {
            sfu = sfu.with_teardown_failure_sink(sink);
        }
        Self {
            sfu: Arc::new(sfu),
            call: CallId::try_new("retirement-receipts@muc.waddle.social").expect("call"),
            identity: fixture_identity("alice"),
        }
    }

    fn publish(&self, generation: OccupancySessionGeneration) -> JoinToken {
        self.sfu
            .issue_join_token_with_session(
                &self.call,
                &self.identity,
                MediaCapabilities::direct_call_peer(),
                &SessionBinding::try_new("receipt-session").expect("session"),
                generation,
                captured_registration(&self.sfu, &self.call, &self.identity),
            )
            .expect("mint")
            .expect("publish")
    }

    fn receipts(&self) -> Vec<PendingMediaRetirement> {
        self.sfu
            .pending_media_retirements(&self.call, &self.identity)
    }

    async fn wait_until_acknowledged(&self) {
        tokio::time::timeout(StdDuration::from_secs(2), async {
            while !self.receipts().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("completed persistence must acknowledge its receipt");
    }
}

#[test]
fn takeover_receipt_remains_visible_without_a_sink_until_explicitly_acknowledged() {
    let fixture = ReceiptFixture::new(None);
    let displaced = fixture_occupancy_session();
    fixture.publish(displaced);
    assert!(fixture.receipts().is_empty());
    fixture.publish(fixture_occupancy_session());
    let first = fixture.receipts();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].intent.occupant_session, Some(displaced));
    assert_eq!(
        fixture.receipts(),
        first,
        "snapshotting never consumes work"
    );
    assert!(fixture
        .sfu
        .pending_media_retirements(&fixture.call, &fixture_identity("bob"))
        .is_empty());
    fixture.sfu.acknowledge_media_retirement(&first[0]);
    assert!(fixture.receipts().is_empty());
}

#[test]
fn dropping_an_unpolled_sink_future_never_acknowledges_persistence() {
    let polled = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&polled);
    let fixture = ReceiptFixture::new(Some(Arc::new(move |_| {
        let observed = Arc::clone(&observed);
        Box::pin(async move {
            observed.store(true, Ordering::Release);
        })
    })));
    fixture.publish(fixture_occupancy_session());
    fixture.publish(fixture_occupancy_session());
    assert!(!polled.load(Ordering::Acquire));
    assert_eq!(fixture.receipts().len(), 1);
}

#[tokio::test]
async fn an_in_flight_sink_does_not_hide_a_retirement_from_the_reply_barrier() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let sink_entered = Arc::clone(&entered);
    let sink_gate = Arc::clone(&gate);
    let fixture = ReceiptFixture::new(Some(Arc::new(move |_| {
        let entered = Arc::clone(&sink_entered);
        let gate = Arc::clone(&sink_gate);
        Box::pin(async move {
            entered.notify_one();
            gate.acquire().await.expect("test gate").forget();
        })
    })));
    fixture.publish(fixture_occupancy_session());
    fixture.publish(fixture_occupancy_session());
    tokio::time::timeout(StdDuration::from_secs(2), entered.notified())
        .await
        .expect("persistence must start");
    let receipts = fixture.receipts();
    assert_eq!(receipts.len(), 1);
    assert_eq!(fixture.receipts(), receipts);
    gate.add_permits(1);
    fixture.wait_until_acknowledged().await;
}

#[tokio::test]
async fn an_old_acknowledgement_cannot_clear_a_new_report_of_the_same_intent() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let sink_entered = Arc::clone(&entered);
    let sink_gate = Arc::clone(&gate);
    let fixture = ReceiptFixture::new(Some(Arc::new(move |_| {
        let entered = Arc::clone(&sink_entered);
        let gate = Arc::clone(&sink_gate);
        Box::pin(async move {
            entered.notify_one();
            gate.acquire().await.expect("test gate").forget();
        })
    })));
    fixture.publish(fixture_occupancy_session());
    fixture.publish(fixture_occupancy_session());
    tokio::time::timeout(StdDuration::from_secs(2), entered.notified())
        .await
        .expect("first persistence must start");
    let first = fixture.receipts().pop().expect("first receipt");
    fixture
        .sfu
        .teardown_reporter
        .report(std::iter::once(first.intent.clone()));
    let second = fixture.receipts().pop().expect("new receipt");
    assert_ne!(first, second);
    fixture.sfu.acknowledge_media_retirement(&first);
    assert_eq!(fixture.receipts(), vec![second.clone()]);

    gate.add_permits(1);
    tokio::time::timeout(StdDuration::from_secs(2), entered.notified())
        .await
        .expect("new report must be persisted separately");
    assert_eq!(
        fixture.receipts(),
        vec![second],
        "completion of the older sink must not acknowledge the newer report"
    );
    gate.add_permits(1);
    fixture.wait_until_acknowledged().await;
}

#[test]
fn failed_publication_cas_does_not_create_or_replace_receipts() {
    let fixture = ReceiptFixture::new(None);
    fixture.publish(fixture_occupancy_session());
    let stale = captured_registration(&fixture.sfu, &fixture.call, &fixture.identity);
    fixture.publish(fixture_occupancy_session());
    let before = fixture.receipts();
    assert_eq!(before.len(), 1);
    assert!(fixture
        .sfu
        .issue_join_token_with_session(
            &fixture.call,
            &fixture.identity,
            MediaCapabilities::direct_call_peer(),
            &SessionBinding::try_new("stale-session").expect("session"),
            fixture_occupancy_session(),
            stale,
        )
        .expect("conditional publication")
        .is_none());
    assert_eq!(fixture.receipts(), before);
}

#[test]
fn retirement_sink_runs_after_call_and_token_guards_are_released() {
    let holder = Arc::new(Mutex::new(None::<Weak<LiveKitSfu>>));
    let sink_holder = Arc::clone(&holder);
    let called = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&called);
    let fixture = ReceiptFixture::new(Some(Arc::new(move |intent| {
        let sfu = sink_holder
            .lock()
            .expect("fixture holder")
            .as_ref()
            .and_then(Weak::upgrade)
            .expect("live SFU");
        let TeardownTargetLite::DisplacedParticipant { identity, .. } = &intent.target else {
            panic!("expected retirement");
        };
        assert!(!matches!(
            sfu.calls.try_get(&intent.call_id),
            dashmap::try_result::TryResult::Locked
        ));
        assert!(!matches!(
            sfu.issued
                .try_get(&(intent.call_id.clone(), identity.clone())),
            dashmap::try_result::TryResult::Locked
        ));
        assert_eq!(
            sfu.pending_media_retirements(&intent.call_id, identity)
                .len(),
            1,
            "data staging precedes dispatch without holding the reporter mutex"
        );
        observed.store(true, Ordering::Release);
        Box::pin(async {})
    })));
    *holder.lock().expect("fixture holder") = Some(Arc::downgrade(&fixture.sfu));
    fixture.publish(fixture_occupancy_session());
    fixture.publish(fixture_occupancy_session());
    assert!(called.load(Ordering::Acquire));
    assert_eq!(fixture.receipts().len(), 1);
}

#[test]
fn a_following_publication_cannot_observe_registration_without_its_receipt() {
    let fixture = Arc::new(ReceiptFixture::new(None));
    let first = fixture_occupancy_session();
    let second = fixture_occupancy_session();
    let third = fixture_occupancy_session();
    fixture.publish(first);
    let start = Arc::new(Barrier::new(3));
    let publishing = Arc::clone(&fixture);
    let publish_start = Arc::clone(&start);
    let publisher = std::thread::spawn(move || {
        publish_start.wait();
        publishing.publish(second);
    });
    let following = Arc::clone(&fixture);
    let follow_start = Arc::clone(&start);
    let follower = std::thread::spawn(move || {
        follow_start.wait();
        let deadline = std::time::Instant::now() + StdDuration::from_secs(2);
        while following
            .sfu
            .participant_occupant_session(&following.call, &following.identity)
            != Some(second)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "publication must complete"
            );
            std::thread::yield_now();
        }
        assert!(following
            .receipts()
            .iter()
            .any(|receipt| receipt.intent.occupant_session == Some(first)));
        following.publish(third);
    });
    start.wait();
    publisher.join().expect("first publication");
    follower.join().expect("following publication");
    let receipts = fixture.receipts();
    assert_eq!(receipts.len(), 2);
    for generation in [first, second] {
        assert!(receipts
            .iter()
            .any(|receipt| receipt.intent.occupant_session == Some(generation)));
    }
}
