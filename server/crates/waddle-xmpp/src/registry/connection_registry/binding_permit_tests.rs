use super::{
    ActorSocketOutboundEndpoint, ConnectionEntry, ConnectionRegistry, ForceDetachOfferError,
    ForceDetachOrigin, ForceDetachRequest, LocalConnectionBindingId,
};
use crate::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
use crate::pending_delivery::SmSessionId;
use jid::FullJid;
use std::sync::{mpsc as sync_mpsc, Arc};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

fn jid() -> FullJid {
    "alice@example.com/phone".parse().expect("full JID")
}

fn entry() -> (ConnectionEntry, mpsc::Receiver<super::OutboundStanza>) {
    let (tx, rx) = mpsc::channel(8);
    (ConnectionEntry::new(tx), rx)
}

fn wait_for_route_lock(registry: &ConnectionRegistry) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match registry.connections.try_get(&jid()) {
            dashmap::try_result::TryResult::Locked => return,
            dashmap::try_result::TryResult::Present(entry) => drop(entry),
            dashmap::try_result::TryResult::Absent => panic!("route vanished before revocation"),
        }
        assert!(
            Instant::now() < deadline,
            "mutation never reached the route lock"
        );
        std::thread::yield_now();
    }
}

fn request() -> ForceDetachRequest {
    let (ack, _) = oneshot::channel();
    ForceDetachRequest {
        origin: ForceDetachOrigin::CrossNodeResume,
        target_binding: None,
        requester_bare_jid: jid().to_bare(),
        ack,
    }
}

#[test]
fn start_and_revocation_are_ordered_in_both_directions() {
    let registry = Arc::new(ConnectionRegistry::new());
    let (old, _rx) = entry();
    let owner = registry.register_socket_entry(jid(), old).expect("publish");
    let permit = registry
        .local_binding_permit_if_owner(&jid(), &owner)
        .expect("permit");
    let (inside, entered) = sync_mpsc::channel();
    let (release, continue_start) = sync_mpsc::channel();
    let start = std::thread::spawn({
        let permit = permit.clone();
        move || {
            permit.start_if_current(|| {
                inside.send(()).expect("start entered");
                continue_start.recv().expect("release start");
            })
        }
    });
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("start entered");
    let (done, retired) = sync_mpsc::channel();
    let remover = std::thread::spawn({
        let registry = registry.clone();
        move || {
            registry.unregister_if_owner(&jid(), &owner);
            done.send(()).expect("retired");
        }
    });
    wait_for_route_lock(&registry);
    assert!(
        retired.recv_timeout(Duration::from_millis(50)).is_err(),
        "removal must wait for the synchronous start"
    );
    release.send(()).expect("release start");
    assert_eq!(start.join().expect("start thread"), Some(()));
    retired
        .recv_timeout(Duration::from_secs(5))
        .expect("retirement completes");
    remover.join().expect("remove thread");
    assert_eq!(
        permit.start_if_current(|| 1),
        None,
        "revocation wins every later start"
    );
}

#[test]
fn same_socket_replacement_preserves_lock_order_and_refusal_preserves_incumbent() {
    let registry = Arc::new(ConnectionRegistry::new());
    let connection = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let (old_tx, _old_rx) = mpsc::channel(1);
    let old = ConnectionEntry::new_for_binding(old_tx, BindingRef::fresh(connection.clone()));
    let owner = registry
        .register_socket_entry(jid(), old.clone())
        .expect("old");
    let permit = registry
        .local_binding_permit_if_owner(&jid(), &owner)
        .expect("permit");
    let (new_tx, _new_rx) = mpsc::channel(1);
    let mut successor = ConnectionEntry::new_for_binding(new_tx, BindingRef::fresh(connection));
    successor.write_interruption = old.write_interruption.clone();
    let (inside, entered) = sync_mpsc::channel();
    let (release, proceed) = sync_mpsc::channel();
    let start = std::thread::spawn({
        let permit = permit.clone();
        let socket = old.write_interruption.clone();
        move || {
            permit.start_if_current(|| {
                inside.send(()).expect("inside permit");
                proceed.recv().expect("release");
                socket.with_uninterrupted(|| ()).expect("socket live")
            })
        }
    });
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("entered");
    let (done, result) = sync_mpsc::channel();
    let replacement = std::thread::spawn({
        let registry = registry.clone();
        move || {
            done.send(registry.register_socket_entry(jid(), successor))
                .expect("reply")
        }
    });
    wait_for_route_lock(&registry);
    assert!(result.recv_timeout(Duration::from_millis(50)).is_err());
    release.send(()).expect("release");
    assert_eq!(start.join().expect("start"), Some(()));
    result
        .recv_timeout(Duration::from_secs(5))
        .expect("no lock inversion")
        .expect("successor");
    replacement.join().expect("replacement");
    assert!(!permit.is_active());

    let (candidate, _rx) = entry();
    let current = registry.entry(&jid()).expect("successor entry");
    let current_permit = registry
        .local_binding_permit_if_owner(&jid(), &current.binding_id())
        .expect("successor permit");
    candidate.write_interruption.cancel();
    assert!(registry.register_socket_entry(jid(), candidate).is_none());
    assert!(
        current_permit.is_active(),
        "failed publication retains incumbent permit"
    );
}

#[test]
fn exact_republication_and_stale_removal_preserve_current_permit() {
    let registry = ConnectionRegistry::new();
    let (old, _rx) = entry();
    let owner = registry
        .register_socket_entry(jid(), old.clone())
        .expect("publish");
    let permit = registry
        .local_binding_permit_if_owner(&jid(), &owner)
        .expect("permit");
    assert!(registry.register_entry_if_owner_or_absent(jid(), old.clone(), &owner));
    assert!(registry.register_socket_entry(jid(), old).is_some());
    assert!(registry
        .unregister_if_owner(&jid(), &LocalConnectionBindingId::fresh())
        .is_none());
    assert!(permit.is_active());
    registry
        .unregister_if_owner(&jid(), &owner)
        .expect("owned removal");
    assert!(!permit.is_active());
}

#[test]
fn accepted_detach_revokes_only_its_binding_and_rejected_offers_do_not() {
    let registry = ConnectionRegistry::new();
    let (old, _rx) = entry();
    let owner = registry
        .register_socket_entry(jid(), old.clone())
        .expect("publish");
    let permit = registry
        .local_binding_permit_if_owner(&jid(), &owner)
        .expect("permit");
    let mut control = old.take_force_detach_rx().expect("receiver");
    for _ in 0..8 {
        old.force_detach_sender().try_send(request()).expect("fill");
    }
    assert!(matches!(
        old.try_force_detach_and_revoke(request()),
        Err(ForceDetachOfferError::Full(_))
    ));
    assert!(permit.is_active());
    control.try_recv().expect("free capacity");
    old.try_force_detach_and_revoke(request())
        .expect("offer accepted");
    assert!(!permit.is_active());
    assert!(
        !old.write_interruption.is_cancelled(),
        "resume detach retains socket"
    );
    let (successor, _rx) = entry();
    let next = registry
        .register_socket_entry(jid(), successor)
        .expect("successor");
    let next_permit = registry
        .local_binding_permit_if_owner(&jid(), &next)
        .expect("next permit");
    control.try_recv().expect("free stale offer capacity");
    old.try_force_detach_and_revoke(request())
        .expect("stale entry can still offer");
    assert!(
        next_permit.is_active(),
        "accepted old offer cannot revoke successor"
    );
    drop(control);
    assert!(matches!(
        old.try_force_detach_and_revoke(request()),
        Err(ForceDetachOfferError::Closed(_))
    ));
    assert!(
        next_permit.is_active(),
        "old control cannot revoke successor"
    );
}

#[test]
fn each_closed_or_exact_retirement_revokes_the_removed_entry() {
    for mode in 0..5 {
        let registry = ConnectionRegistry::new();
        let (old, rx) = entry();
        let owner = registry
            .register_socket_entry(jid(), old.clone())
            .expect("publish");
        let permit = registry
            .local_binding_permit_if_owner(&jid(), &owner)
            .expect("permit");
        let stream = SmSessionId::new("permit-test-stream");
        assert!(registry.set_sm_stream_id_if_owner(&jid(), &owner, Some(stream.clone())));
        let endpoint = old.endpoint.clone();
        drop(rx);
        match mode {
            0 => {
                registry.unregister(&jid());
            }
            1 => {
                registry.unregister_if_owner(&jid(), &owner);
            }
            2 => {
                registry.unregister_if_sm_stream_id(&jid(), &stream);
            }
            3 => registry.remove_if_sender_closed(&jid()),
            4 => registry.remove_if_sender_closed_owner(&jid(), &endpoint),
            _ => unreachable!(),
        }
        assert!(!permit.is_active(), "retirement mode {mode} must revoke");
    }
}

#[test]
fn exact_seal_revokes_and_wrong_identity_cannot_seal() {
    let registry = ConnectionRegistry::new();
    let (old, _rx) = entry();
    let owner = registry
        .register_socket_entry(jid(), old.clone())
        .expect("publish");
    let permit = registry
        .local_binding_permit_if_owner(&jid(), &owner)
        .expect("permit");
    let binding = old.binding_ref().expect("local binding").clone();
    assert!(!old.seal_binding(LocalConnectionBindingId::fresh(), &binding));
    assert!(permit.is_active());
    assert!(old.seal_binding(owner, &binding));
    assert!(!permit.is_active());
    assert!(registry
        .local_binding_permit_if_owner(&jid(), &owner)
        .is_none());
}

#[tokio::test]
async fn provisional_publication_and_exact_retirement_fail_closed() {
    let registry = ConnectionRegistry::new();
    let connection = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
    let binding = BindingRef::fresh(connection.clone());
    let (socket, _receiver, _custody) =
        ActorSocketOutboundEndpoint::channel(connection.clone(), 2, 4096).expect("socket");
    let candidate =
        ConnectionEntry::new_actor_provisional_for_binding(socket, binding.clone(), jid())
            .expect("candidate");
    let guard = registry.lock_bind(&jid()).await;
    assert!(registry.publish_provisional_if_expected(&guard, jid(), &candidate, None));
    assert!(
        registry.publish_provisional_if_expected(
            &guard,
            jid(),
            &candidate,
            Some(candidate.binding_id()),
        ),
        "provisional same-entry publication stays provisional"
    );
    assert!(
        registry
            .local_binding_permit_if_owner(&jid(), &candidate.binding_id())
            .is_none(),
        "provisional binding cannot mint a permit"
    );
    let current = registry.entry(&jid()).expect("published candidate");
    assert!(current.activate_binding(candidate.binding_id(), &binding));
    let old_permit = registry
        .local_binding_permit_if_owner(&jid(), &candidate.binding_id())
        .expect("active permit");
    registry.register_entry(jid(), current.clone());
    assert!(registry.register_entry_if_owner_or_absent(
        jid(),
        current.clone(),
        &candidate.binding_id()
    ));
    assert!(registry.register_socket_entry(jid(), current).is_some());
    assert!(old_permit.is_active());
    assert!(
        registry
            .local_binding_permit_if_owner(&jid(), &candidate.binding_id())
            .is_some(),
        "same-entry publication must not seal actor admission"
    );

    let next_binding = BindingRef::fresh(connection.clone());
    let (socket, _receiver, _custody) =
        ActorSocketOutboundEndpoint::channel(connection, 2, 4096).expect("successor socket");
    let next =
        ConnectionEntry::new_actor_provisional_for_binding(socket, next_binding.clone(), jid())
            .expect("successor");
    assert!(registry.publish_provisional_if_expected(
        &guard,
        jid(),
        &next,
        Some(candidate.binding_id())
    ));
    assert!(!old_permit.is_active());
    assert!(registry
        .local_binding_permit_if_owner(&jid(), &next.binding_id())
        .is_none());
    assert!(!registry.retire_provisional_if_exact(&guard, &jid(), &candidate));
    let current = registry.entry(&jid()).expect("published successor");
    assert!(current.activate_binding(next.binding_id(), &next_binding));
    let next_permit = registry
        .local_binding_permit_if_owner(&jid(), &next.binding_id())
        .expect("successor permit");
    assert!(registry.retire_provisional_if_exact(&guard, &jid(), &next));
    assert!(!next_permit.is_active());
}

#[test]
fn control_reservation_and_start_have_one_winner() {
    let registry = ConnectionRegistry::new();
    let (old, _rx) = entry();
    let owner = registry
        .register_socket_entry(jid(), old.clone())
        .expect("publish");
    let permit = registry
        .local_binding_permit_if_owner(&jid(), &owner)
        .expect("permit");
    let mut control = old.take_force_detach_rx().expect("control receiver");
    let (entered_tx, entered_rx) = sync_mpsc::channel();
    let (release_tx, release_rx) = sync_mpsc::channel();
    let start = std::thread::spawn({
        let permit = permit.clone();
        move || {
            permit.start_if_current(|| {
                entered_tx.send(()).expect("start entered");
                release_rx.recv().expect("release start");
            })
        }
    });
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("start entered");
    let (trying_tx, trying_rx) = sync_mpsc::channel();
    let (sent_tx, sent_rx) = sync_mpsc::channel();
    let detach = std::thread::spawn(move || {
        trying_tx.send(()).expect("begin detach");
        sent_tx
            .send(old.try_force_detach_and_revoke(request()))
            .expect("detach result");
    });
    trying_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("detach trying");
    assert!(sent_rx.recv_timeout(Duration::from_millis(50)).is_err());
    assert!(
        control.try_recv().is_err(),
        "offer cannot be accepted while start holds the cell"
    );
    release_tx.send(()).expect("release start");
    assert_eq!(start.join().expect("start thread"), Some(()));
    sent_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("detach completes")
        .expect("accepted");
    detach.join().expect("detach thread");
    assert!(control.try_recv().is_ok());
    assert_eq!(
        permit.start_if_current(|| ()),
        None,
        "accepted reservation beats every later start"
    );
}

#[test]
fn queue_wake_may_reenter_the_permit_without_deadlock() {
    use std::task::{Context, Poll, Wake, Waker};
    struct ReentrantWake {
        permit: super::LocalBindingPermit,
        observed: sync_mpsc::Sender<bool>,
    }
    impl Wake for ReentrantWake {
        fn wake(self: Arc<Self>) {
            use std::future::Future;
            let mut revoked = Box::pin(self.permit.revoked());
            let mut context = Context::from_waker(Waker::noop());
            let notified = revoked.as_mut().poll(&mut context).is_ready();
            self.observed
                .send(notified && self.permit.start_if_current(|| ()).is_none())
                .expect("wake result");
        }
    }
    let registry = ConnectionRegistry::new();
    let (old, _rx) = entry();
    let owner = registry
        .register_socket_entry(jid(), old.clone())
        .expect("publish");
    let permit = registry
        .local_binding_permit_if_owner(&jid(), &owner)
        .expect("permit");
    let mut receiver = old.take_force_detach_rx().expect("receiver");
    let (observed_tx, observed_rx) = sync_mpsc::channel();
    let waker = Waker::from(Arc::new(ReentrantWake {
        permit,
        observed: observed_tx,
    }));
    let mut context = Context::from_waker(&waker);
    assert!(matches!(receiver.poll_recv(&mut context), Poll::Pending));
    let (done_tx, done_rx) = sync_mpsc::channel();
    let sender = std::thread::spawn(move || {
        done_tx
            .send(old.try_force_detach_and_revoke(request()))
            .expect("offer result")
    });
    assert!(observed_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("reentrant wake"));
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("offer returned")
        .expect("accepted");
    sender.join().expect("sender thread");
    assert!(receiver.try_recv().is_ok());
}

#[test]
fn receiver_closing_after_reservation_is_not_cleanup_proof() {
    let registry = ConnectionRegistry::new();
    let (old, _rx) = entry();
    let owner = registry
        .register_socket_entry(jid(), old.clone())
        .expect("publish");
    let permit = registry
        .local_binding_permit_if_owner(&jid(), &owner)
        .expect("permit");
    let control = old.take_force_detach_rx().expect("receiver");
    old.try_force_detach_and_revoke_after_reserve_for_test(request(), || drop(control))
        .expect("successful reservation remains accepted");
    assert!(!permit.is_active());
    assert!(
        registry.is_connected(&jid()),
        "reservation does not prove lifecycle cleanup"
    );
    assert!(!old.write_interruption.is_cancelled());
}

#[test]
fn unconditional_replacement_and_stale_eviction_preserve_only_the_successor() {
    let registry = ConnectionRegistry::new();
    let (old, rx) = entry();
    let owner = registry.register_entry(jid(), old.clone());
    let permit = registry
        .local_binding_permit_if_owner(&jid(), &owner)
        .expect("old permit");
    registry.register_entry(jid(), old.clone());
    assert!(permit.is_active());
    let old_endpoint = old.endpoint.clone();
    drop(rx);
    let (next, _rx) = entry();
    let next_owner = registry.register_entry(jid(), next);
    assert!(!permit.is_active());
    let next_permit = registry
        .local_binding_permit_if_owner(&jid(), &next_owner)
        .expect("next permit");
    registry.remove_if_sender_closed_owner(&jid(), &old_endpoint);
    assert!(registry
        .unregister_if_sm_stream_id(&jid(), &SmSessionId::new("stale"))
        .is_none());
    assert!(next_permit.is_active());
}
