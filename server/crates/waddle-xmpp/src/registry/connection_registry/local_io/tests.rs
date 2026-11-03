use super::{LocalIoRelinquishment, LocalIoRelinquishmentOutcome, LocalSocketIo};
use crate::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
use crate::registry::{ConnectionEntry, ConnectionRegistry, SocketCleanupState};
use jid::FullJid;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use tokio::sync::{mpsc, oneshot};
use waddle_xmpp_core::OccupancySessionGeneration;

type Observations = Arc<Mutex<Vec<(&'static str, bool)>>>;
type ObserverSlot = Arc<Mutex<Option<LocalIoRelinquishment>>>;

struct ObservedHalf {
    name: &'static str,
    observations: Observations,
    observer: ObserverSlot,
}

impl Drop for ObservedHalf {
    fn drop(&mut self) {
        let relinquished = self
            .observer
            .lock()
            .expect("observer slot")
            .as_ref()
            .expect("installed observer")
            .is_relinquished();
        self.observations
            .lock()
            .expect("observations")
            .push((self.name, relinquished));
    }
}

fn observed_io() -> (
    LocalSocketIo<ObservedHalf, ObservedHalf>,
    LocalIoRelinquishment,
    Observations,
) {
    let observations = Observations::default();
    let observer = ObserverSlot::default();
    let half = |name| ObservedHalf {
        name,
        observations: Arc::clone(&observations),
        observer: Arc::clone(&observer),
    };
    let (io, witness) = LocalSocketIo::new(half("sender"), half("receiver"));
    *observer.lock().expect("observer slot") = Some(witness.clone());
    (io, witness, observations)
}

fn assert_both_dropped_before_signal(observations: &Observations) {
    assert_eq!(
        *observations.lock().expect("observations"),
        [("sender", false), ("receiver", false)]
    );
}

#[tokio::test]
async fn witness_is_pending_through_both_half_destructors() {
    let (io, witness, observations) = observed_io();
    let mut wait = Box::pin(witness.wait());
    assert!(matches!(futures::poll!(wait.as_mut()), Poll::Pending));

    drop(io);

    assert_both_dropped_before_signal(&observations);
    assert!(witness.is_relinquished());
    assert_eq!(wait.await, LocalIoRelinquishmentOutcome::Relinquished);
}

#[tokio::test]
async fn cancellation_and_unwind_drop_both_halves_before_signalling() {
    for should_panic in [false, true] {
        let (io, witness, observations) = observed_io();
        let (started, started_rx) = oneshot::channel();
        let (proceed, proceed_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _io = io;
            started.send(()).expect("test observes start");
            let _ = proceed_rx.await;
            assert!(!should_panic, "intentional connection task panic");
        });

        started_rx.await.expect("owner started");
        assert!(!witness.is_relinquished());
        assert!(observations.lock().expect("observations").is_empty());
        if should_panic {
            proceed.send(()).expect("release task to panic");
        } else {
            task.abort();
        }
        let result = task.await.expect_err("task is cancelled or panicked");
        assert_eq!(result.is_panic(), should_panic);
        assert_eq!(result.is_cancelled(), !should_panic);
        assert_both_dropped_before_signal(&observations);
        assert_eq!(
            witness.wait().await,
            LocalIoRelinquishmentOutcome::Relinquished
        );
    }
}

struct PanicOnDrop;

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("intentional half destructor panic");
    }
}

#[tokio::test]
async fn panicking_half_destructor_never_signals_relinquishment() {
    assert_half_panic_is_unobserved(PanicOnDrop, ()).await;
    assert_half_panic_is_unobserved((), PanicOnDrop).await;
}

async fn assert_half_panic_is_unobserved<S, R>(sender: S, receiver: R) {
    let (io, witness) = LocalSocketIo::new(sender, receiver);

    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(io))).is_err());

    assert!(!witness.is_relinquished());
    assert_eq!(
        witness.wait().await,
        LocalIoRelinquishmentOutcome::Unobserved
    );
}

#[tokio::test]
async fn binding_detachment_does_not_relinquish_the_same_socket() {
    let (io, witness) = LocalSocketIo::new((), ());
    let (other_io, other_witness) = LocalSocketIo::new((), ());
    let physical = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let old_binding = BindingRef::fresh(physical.clone());
    let fresh_binding = BindingRef::fresh(physical.clone());
    let jid: FullJid = "user@example.com/web".parse().expect("jid");
    let registry = ConnectionRegistry::new();
    let mut guard = registry.lock_bind(&jid).await;
    let (old_endpoint, _old_rx) = mpsc::channel(1);
    let old_entry = ConnectionEntry::new_for_binding_with_local_io(
        old_endpoint,
        old_binding.clone(),
        witness.clone(),
    );
    let old = guard.publish(old_entry, OccupancySessionGeneration::mint());

    old.finish(SocketCleanupState::Detached);
    assert_eq!(old.state(), SocketCleanupState::Detached);
    assert!(!old
        .entry
        .local_io_relinquishment()
        .expect("witness")
        .is_relinquished());

    let (fresh_endpoint, _fresh_rx) = mpsc::channel(1);
    let fresh_entry = ConnectionEntry::new_for_binding_with_local_io(
        fresh_endpoint,
        fresh_binding.clone(),
        witness,
    );
    let fresh = guard.publish(fresh_entry, OccupancySessionGeneration::mint());
    let (other_endpoint, _other_rx) = mpsc::channel(1);
    let other = ConnectionEntry::new_for_binding_with_local_io(
        other_endpoint,
        BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process())),
        other_witness,
    );
    let (remote_endpoint, _remote_rx) = mpsc::channel(1);
    let remote = ConnectionEntry::remote_hosted_for_connection(remote_endpoint, physical);

    assert_eq!(old.entry.connection_ref(), fresh.entry.connection_ref());
    assert_eq!(old.entry.binding_ref(), Some(&old_binding));
    assert_eq!(fresh.entry.binding_ref(), Some(&fresh_binding));
    assert_ne!(old_binding, fresh_binding);
    assert_ne!(fresh.entry.connection_ref(), other.connection_ref());
    assert!(!fresh
        .entry
        .local_io_relinquishment()
        .expect("witness")
        .is_relinquished());
    assert!(remote.local_io_relinquishment().is_none());

    drop(io);
    assert!(old
        .entry
        .local_io_relinquishment()
        .expect("witness")
        .is_relinquished());
    assert!(fresh
        .entry
        .local_io_relinquishment()
        .expect("witness")
        .is_relinquished());
    assert!(!other
        .local_io_relinquishment()
        .expect("witness")
        .is_relinquished());
    drop(other_io);
    assert!(other
        .local_io_relinquishment()
        .expect("witness")
        .is_relinquished());
}
