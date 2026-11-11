use super::{lock, Admission, Arc, Custody, EnvelopeId, Offer, OfferReply, Scope, Stage};
use std::sync::Barrier;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

// These bytes are arbitrary test data, not a serialized XMPP value. Only the
// boxed slice's logical length is charged; allocator and model overhead are
// outside this witness. A real typed stanza needs its own proven adapter.
#[derive(Debug)]
struct Sample {
    marker: u8,
    allocation: Box<[u8]>,
}

impl Sample {
    fn new(marker: u8, bytes: usize) -> Self {
        Self {
            marker,
            allocation: vec![marker; bytes].into_boxed_slice(),
        }
    }
}

fn admit(
    owner: &Custody<Sample>,
    generation: &Scope,
    payload: Sample,
) -> Result<EnvelopeId, Sample> {
    let bytes = payload.allocation.len();
    owner.admit(generation, payload, bytes)
}

fn usage(owner: &Custody<Sample>) -> (usize, usize) {
    let usage = lock(&owner.budget.usage);
    (usage.count, usage.bytes)
}

fn assert_refused_original(owner: &Custody<Sample>, generation: &Scope, payload: Sample) {
    let marker = payload.marker;
    let allocation = payload.allocation.as_ptr();
    let length = payload.allocation.len();
    let returned = admit(owner, generation, payload).expect_err("admission must refuse");
    assert_eq!(returned.marker, marker);
    assert_eq!(returned.allocation.as_ptr(), allocation);
    assert_eq!(returned.allocation.len(), length);
    assert!(returned.allocation.iter().all(|byte| *byte == marker));
}

#[test]
fn count_and_byte_saturation_refuse_the_exact_original_allocation() {
    let byte_owner = Custody::new(5, 5);
    let generation = byte_owner.scope();
    admit(&byte_owner, &generation, Sample::new(1, 3)).expect("first fits");
    assert_refused_original(&byte_owner, &generation, Sample::new(2, 3));
    assert_eq!(usage(&byte_owner), (1, 3));
    admit(&byte_owner, &generation, Sample::new(3, 2)).expect("remaining bytes fit");
    assert_eq!(usage(&byte_owner), (2, 5));

    let count_owner = Custody::new(1, 100);
    let generation = count_owner.scope();
    admit(&count_owner, &generation, Sample::new(4, 1)).expect("one slot fits");
    assert_refused_original(&count_owner, &generation, Sample::new(5, 2));
    assert_eq!(usage(&count_owner), (1, 1));
}

#[test]
fn arithmetic_overflow_fails_closed_before_acceptance() {
    let owner = Custody::new(usize::MAX, usize::MAX);
    let generation = owner.scope();

    // No leases exist; these injected near-limit counters exercise paths that
    // cannot be reached with real allocations in a bounded test process.
    lock(&owner.budget.usage).count = usize::MAX;
    assert_refused_original(&owner, &generation, Sample::new(1, 1));
    {
        let mut counters = lock(&owner.budget.usage);
        counters.count = 0;
        counters.bytes = usize::MAX;
    }
    assert_refused_original(&owner, &generation, Sample::new(2, 1));
    lock(&owner.budget.usage).bytes = 0;

    lock(&owner.state).next_id = u64::MAX;
    assert_refused_original(&owner, &generation, Sample::new(3, 1));
    assert_eq!(usage(&owner), (0, 0));

    lock(&owner.state).scope.sequence = u64::MAX;
    let max_scope = owner.scope();
    assert_eq!(owner.replace(&max_scope), None);
    assert_eq!(owner.scope(), max_scope);
    assert_eq!(lock(&owner.state).admission, Admission::Open);
}

#[test]
fn snapshots_and_terminal_transfer_hold_the_same_charge_through_replacement() {
    let owner = Custody::new(1, 4);
    let old = owner.scope();
    let id = admit(&owner, &old, Sample::new(7, 4)).expect("admit");
    let first = owner.snapshot(&id).expect("first snapshot");
    let second = owner.snapshot(&id).expect("second snapshot");
    assert!(Arc::ptr_eq(&first, &second));
    assert!(owner.seal(&old));
    let (terminal, stage) = owner.take_not_enqueued(&id).expect("terminal transfer");
    assert_eq!(stage, Stage::NotAttempted);
    assert!(Arc::ptr_eq(&terminal, &first));
    assert_eq!(terminal.payload().marker, 7);

    let new = owner.replace(&old).expect("new generation");
    assert_refused_original(&owner, &new, Sample::new(8, 1));
    drop(terminal);
    drop(first);
    assert_eq!(usage(&owner), (1, 4));
    assert_refused_original(&owner, &new, Sample::new(9, 1));
    drop(second);
    assert_eq!(usage(&owner), (0, 0));
    admit(&owner, &new, Sample::new(10, 4)).expect("last drop releases the budget");
}

#[test]
fn seal_close_and_replacement_fence_admission_and_queued_attempts() {
    let owner = Custody::new(3, 9);
    let old = owner.scope();
    let id = admit(&owner, &old, Sample::new(1, 1)).expect("admit");
    assert!(owner.seal(&old));
    assert_eq!(owner.stage(&id), Some(Stage::NotAttempted));
    assert!(owner.begin_offer(&old, &id).is_none());
    assert_refused_original(&owner, &old, Sample::new(2, 1));
    assert!(owner.close(&old));
    assert!(!owner.seal(&old));

    let new = owner
        .replace(&old)
        .expect("replacement opens a new generation");
    assert_ne!(old, new);
    assert!(!owner.close(&old));
    assert!(!owner.seal(&old));
    assert_eq!(owner.replace(&old), None);
    assert_refused_original(&owner, &old, Sample::new(3, 1));
    let new_id = admit(&owner, &new, Sample::new(4, 1)).expect("new generation admitted");
    assert!(owner.begin_offer(&old, &new_id).is_none());
    assert!(owner.close(&new));
    assert_eq!(owner.stage(&new_id), Some(Stage::NotAttempted));
    assert_refused_original(&owner, &new, Sample::new(5, 1));
}

#[test]
fn equal_sequence_from_another_owner_or_binding_cannot_cross_the_scope() {
    let first = Custody::new(2, 4);
    let second = Custody::new(2, 4);
    let first_scope = first.scope();
    let second_scope = second.scope();
    assert_eq!(first_scope.sequence, second_scope.sequence);
    assert_ne!(first_scope, second_scope);
    let first_id = admit(&first, &first_scope, Sample::new(1, 1)).expect("first admission");
    let second_id = admit(&second, &second_scope, Sample::new(2, 1)).expect("second admission");
    assert_eq!(first_id.nonce, second_id.nonce);
    assert_refused_original(&first, &second_scope, Sample::new(3, 1));
    assert!(first.begin_offer(&second_scope, &first_id).is_none());
    assert!(first.begin_offer(&first_scope, &second_id).is_none());
    assert!(first.snapshot(&second_id).is_none());
    assert!(!first.close(&second_scope));
    assert_eq!(first.scope(), first_scope);

    let replacement = first.replace(&first_scope).expect("replacement binding");
    assert_ne!(replacement, first_scope);
    assert_refused_original(&first, &first_scope, Sample::new(4, 1));
    assert!(first.begin_offer(&first_scope, &first_id).is_none());
}

#[test]
fn replacement_and_offer_start_are_serialized() {
    for _ in 0..64 {
        let owner = Custody::new(1, 1);
        let generation = owner.scope();
        let id = admit(&owner, &generation, Sample::new(1, 1)).expect("admit");
        let barrier = Arc::new(Barrier::new(3));
        let (offer, replacement) = std::thread::scope(|scope| {
            let start = scope.spawn({
                let owner = Arc::clone(&owner);
                let barrier = Arc::clone(&barrier);
                let generation = generation.clone();
                let id = id.clone();
                move || {
                    barrier.wait();
                    owner.begin_offer(&generation, &id)
                }
            });
            let replace = scope.spawn({
                let owner = Arc::clone(&owner);
                let barrier = Arc::clone(&barrier);
                let generation = generation.clone();
                move || {
                    barrier.wait();
                    owner.replace(&generation)
                }
            });
            barrier.wait();
            (
                start.join().expect("start joined"),
                replace.join().expect("replace joined"),
            )
        });
        assert!(replacement.is_some());
        if let Some(offer) = offer {
            assert_eq!(owner.stage(&id), Some(Stage::InFlight));
            drop(offer);
            assert_eq!(owner.stage(&id), Some(Stage::Uncertain));
        } else {
            assert_eq!(owner.stage(&id), Some(Stage::NotAttempted));
        }
        assert!(owner.begin_offer(&generation, &id).is_none());
    }
}

#[derive(Clone, Copy, Debug)]
enum FakeTarget {
    Local,
    Remote,
}

// The fixture acts as a supervisor that owns the worker independently of an
// observing caller. Neither fake is a real queue, socket, or remote protocol.
fn spawn_fake_offer(
    target: FakeTarget,
    offer: Offer<Sample>,
) -> (
    oneshot::Sender<OfferReply>,
    oneshot::Receiver<()>,
    oneshot::Receiver<()>,
    JoinHandle<()>,
) {
    let (reply_tx, reply_rx) = oneshot::channel();
    let (started_tx, started_rx) = oneshot::channel();
    let (done_tx, done_rx) = oneshot::channel();
    let worker = tokio::spawn(async move {
        let snapshot = offer.snapshot();
        if matches!(target, FakeTarget::Remote) {
            tokio::task::yield_now().await;
        }
        let _ = started_tx.send(());
        let reply = reply_rx.await.unwrap_or(OfferReply::Unknown);
        assert_eq!(snapshot.payload().marker, 42);
        offer.resolve(reply);
        drop(snapshot);
        let _ = done_tx.send(());
    });
    (reply_tx, started_rx, done_rx, worker)
}

async fn signal(receiver: oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .expect("signal timed out")
        .expect("signal sender dropped");
}

async fn finish(worker: JoinHandle<()>) {
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("worker timed out")
        .expect("worker failed");
}

#[tokio::test]
async fn cancelled_observer_cannot_cancel_local_or_remote_owned_offers() {
    for target in [FakeTarget::Local, FakeTarget::Remote] {
        let owner = Custody::new(1, 3);
        let generation = owner.scope();
        let id = admit(&owner, &generation, Sample::new(42, 3)).expect("admit");
        let offer = owner
            .begin_offer(&generation, &id)
            .expect("register before await");
        assert_eq!(owner.stage(&id), Some(Stage::InFlight));
        assert!(owner.take_not_enqueued(&id).is_none());
        let (reply, started, done, worker) = spawn_fake_offer(target, offer);
        signal(started).await;
        let (observing, observed) = oneshot::channel();
        let observer = tokio::spawn(async move {
            let _ = observing.send(());
            done.await
        });
        signal(observed).await;
        observer.abort();
        assert!(observer
            .await
            .expect_err("observer cancelled")
            .is_cancelled());
        assert_eq!(owner.stage(&id), Some(Stage::InFlight));
        assert_refused_original(&owner, &generation, Sample::new(1, 1));

        reply.send(OfferReply::Accepted).expect("late reply");
        finish(worker).await;
        assert_eq!(owner.stage(&id), Some(Stage::OfferAccepted));
        assert!(owner.take_not_enqueued(&id).is_none());
        assert_eq!(usage(&owner), (1, 3));
        assert_refused_original(&owner, &generation, Sample::new(2, 1));
    }
}

#[tokio::test]
async fn cancelled_worker_and_closed_reply_channel_leave_uncertain_custody() {
    for target in [FakeTarget::Local, FakeTarget::Remote] {
        for wait_for_adapter in [false, true] {
            let owner = Custody::new(1, 3);
            let generation = owner.scope();
            let id = admit(&owner, &generation, Sample::new(42, 3)).expect("admit");
            let offer = owner
                .begin_offer(&generation, &id)
                .expect("register before await");
            let (reply, started, _done, worker) = spawn_fake_offer(target, offer);
            if wait_for_adapter {
                signal(started).await;
            }
            worker.abort();
            assert!(worker.await.expect_err("worker cancelled").is_cancelled());
            assert_eq!(owner.stage(&id), Some(Stage::Uncertain));
            assert!(reply.send(OfferReply::Accepted).is_err());
            assert!(owner.take_not_enqueued(&id).is_none());
            assert_eq!(usage(&owner), (1, 3));
            assert_refused_original(&owner, &generation, Sample::new(2, 1));
        }

        let owner = Custody::new(1, 3);
        let generation = owner.scope();
        let second = admit(&owner, &generation, Sample::new(42, 3)).expect("admit again");
        let offer = owner
            .begin_offer(&generation, &second)
            .expect("register again");
        let (reply, _started, _done, worker) = spawn_fake_offer(target, offer);
        drop(reply);
        finish(worker).await;
        assert_eq!(owner.stage(&second), Some(Stage::Uncertain));
    }
}

#[tokio::test]
async fn late_stale_acceptance_cannot_settle_or_touch_the_replacement() {
    let owner = Custody::new(2, 6);
    let old = owner.scope();
    let old_id = admit(&owner, &old, Sample::new(42, 3)).expect("old admission");
    let offer = owner
        .begin_offer(&old, &old_id)
        .expect("old in-flight record");
    let (reply, started, _done, worker) = spawn_fake_offer(FakeTarget::Remote, offer);
    signal(started).await;
    let new = owner.replace(&old).expect("new generation");
    let new_id = admit(&owner, &new, Sample::new(1, 3)).expect("new admission");
    assert!(owner.take_not_enqueued(&old_id).is_none());

    reply
        .send(OfferReply::Accepted)
        .expect("old adapter replies");
    finish(worker).await;
    assert_eq!(owner.stage(&old_id), Some(Stage::Uncertain));
    assert_eq!(owner.stage(&new_id), Some(Stage::Queued));
    assert!(owner.take_not_enqueued(&old_id).is_none());
    assert_refused_original(&owner, &new, Sample::new(2, 1));
    assert_eq!(usage(&owner), (2, 6));
}

#[tokio::test]
async fn fake_proven_refusal_is_distinct_from_unknown() {
    let owner = Custody::new(1, 3);
    let generation = owner.scope();
    let id = admit(&owner, &generation, Sample::new(42, 3)).expect("admit");
    let offer = owner.begin_offer(&generation, &id).expect("start");
    let (reply, _started, done, worker) = spawn_fake_offer(FakeTarget::Local, offer);
    reply.send(OfferReply::NotEnqueued).expect("fake refusal");
    signal(done).await;
    finish(worker).await;
    assert_eq!(owner.stage(&id), Some(Stage::NotEnqueued));
    let (terminal, stage) = owner.take_not_enqueued(&id).expect("terminal transfer");
    assert_eq!(stage, Stage::NotEnqueued);
    assert_eq!(terminal.payload().marker, 42);
}
