use super::{
    mpsc, Arc, BindingAdmissionState, BindingOutboundEndpoint, BindingRef, EndpointEnqueueOutcome,
    EndpointEnqueueRejection, LocalConnectionBindingId, OutboundStanza, SocketOutboundEndpoint,
};
use crate::ingress::MessageKey;
use crate::mam::ArchiveOrdinal;
use crate::pending_delivery::{PendingRowId, SmSessionId};
use crate::registry::{DeliveryKind, OutboundWriteAcceptance};
use crate::stream_management::{
    ArchiveDispatchPosition, SmIngressAppendKey, SmIngressReceiptKind, SmRelayedAppendObligation,
};
use crate::Stanza;
use tokio::sync::oneshot;
use xmpp_parsers::message::{Id, Message, MessageType};

struct ExpectedPayload {
    stanza: minidom::Element,
    row_id: PendingRowId,
    received_at: chrono::DateTime<chrono::Utc>,
    obligation: SmRelayedAppendObligation,
}

fn payload() -> (OutboundStanza, ExpectedPayload, oneshot::Receiver<()>) {
    let resource: jid::FullJid = "bob@example.test/phone".parse().expect("resource JID");
    let mut message = Message::new(Some(resource.clone().into()));
    message.id = Some(Id("full-envelope".into()));
    message.type_ = MessageType::Chat;
    message
        .bodies
        .insert("en".into(), "the full message body".into());
    let stanza = Stanza::Message(message);
    let row_id = PendingRowId::fresh();
    let received_at = chrono::Utc::now();
    let obligation = SmRelayedAppendObligation {
        key: SmIngressAppendKey {
            message_key: MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(1),
            semantic_identity_hash: [7; 32],
            resource: resource.clone(),
        },
        sender_bare: "alice@example.test".parse().expect("sender JID"),
        received_at: Some(received_at),
        archive_positions: vec![ArchiveDispatchPosition {
            archive: resource.to_bare(),
            ordinal: ArchiveOrdinal::from_storage(42).expect("archive ordinal"),
        }],
        dispatch_stream: Some(SmSessionId::new("stream-id")),
    };
    let (acceptance, accepted) = OutboundWriteAcceptance::new();
    let expected = ExpectedPayload {
        stanza: stanza.to_element(),
        row_id: row_id.clone(),
        received_at,
        obligation: obligation.clone(),
    };
    let mut outbound = OutboundStanza::peer_stanza(stanza);
    outbound.pending_row_id = Some(row_id);
    outbound.original_receipt_at = Some(received_at);
    outbound.ingress_append = Some(obligation);
    outbound.write_acceptance = Some(acceptance);
    (outbound, expected, accepted)
}

fn assert_payload(
    outbound: &OutboundStanza,
    expected: &ExpectedPayload,
    accepted: &mut oneshot::Receiver<()>,
) {
    assert_eq!(outbound.stanza.to_element(), expected.stanza);
    assert_eq!(outbound.kind, DeliveryKind::PeerStanza);
    assert_eq!(outbound.pending_row_id.as_ref(), Some(&expected.row_id));
    assert_eq!(outbound.original_receipt_at, Some(expected.received_at));
    assert_eq!(outbound.ingress_append.as_ref(), Some(&expected.obligation));
    assert!(outbound.write_acceptance.is_some());
    assert!(matches!(
        accepted.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
}

fn refused(
    outcome: EndpointEnqueueOutcome,
    reason: EndpointEnqueueRejection,
    expected: &ExpectedPayload,
    accepted: &mut oneshot::Receiver<()>,
) -> OutboundStanza {
    let EndpointEnqueueOutcome::ProvenNotEnqueued {
        reason: actual,
        outbound,
    } = outcome
    else {
        panic!("expected {reason:?} refusal");
    };
    assert_eq!(actual, reason);
    assert_payload(&outbound, expected, accepted);
    *outbound
}

fn acknowledge(outbound: OutboundStanza, accepted: &mut oneshot::Receiver<()>) {
    outbound
        .write_acceptance
        .as_ref()
        .expect("retained callback")
        .acknowledge();
    assert_eq!(accepted.try_recv(), Ok(()));
}

#[test]
fn actor_queue_retains_complete_envelope_after_id_dequeue_and_seal() {
    let connection = crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::local_process(),
    );
    let binding_ref = BindingRef::fresh(connection.clone());
    let owner = LocalConnectionBindingId::fresh();
    let (socket, mut receiver, custody) =
        SocketOutboundEndpoint::actor_channel(connection, 1, 4096).expect("bounded actor queue");
    let endpoint = socket.bind(
        owner,
        Some(binding_ref.clone()),
        BindingAdmissionState::Active,
    );
    let (outbound, expected, mut accepted) = payload();
    assert!(matches!(
        endpoint.try_send(outbound),
        EndpointEnqueueOutcome::AcceptedOrUnknown
    ));
    let id = receiver
        .try_recv()
        .expect("only the ID crosses the channel");
    assert_eq!(id.owner(), owner);
    assert_eq!(id.binding(), &binding_ref);
    assert_payload(
        custody
            .snapshot(&id)
            .expect("custody retains envelope")
            .outbound(),
        &expected,
        &mut accepted,
    );
    let (second, second_expected, mut second_accepted) = payload();
    let _ = refused(
        endpoint.try_send(second),
        EndpointEnqueueRejection::Full,
        &second_expected,
        &mut second_accepted,
    );
    assert_eq!(custody.retained_count(), 1);
    assert!(endpoint.seal(owner, &binding_ref));
    assert_eq!(
        custody.stage(&id),
        Some(super::super::ActorOutboundStage::NotAttempted)
    );
    let (outbound, _) = custody
        .take_terminal(&id)
        .expect("exact terminal retrieval");
    assert_payload(outbound.outbound(), &expected, &mut accepted);
    assert_eq!(custody.retained_count(), 0);
    assert!(receiver.try_recv().is_err());
}

fn binding(
    state: BindingAdmissionState,
) -> (
    BindingOutboundEndpoint,
    mpsc::Receiver<OutboundStanza>,
    LocalConnectionBindingId,
    BindingRef,
) {
    let (socket, receiver) = SocketOutboundEndpoint::channel(1);
    let owner = LocalConnectionBindingId::fresh();
    let binding_ref = BindingRef::fresh(crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::local_process(),
    ));
    (
        socket.bind(owner, Some(binding_ref.clone()), state),
        receiver,
        owner,
        binding_ref,
    )
}

#[test]
fn try_send_refusals_return_the_complete_envelope() {
    let (provisional, mut provisional_rx, owner, binding_ref) =
        binding(BindingAdmissionState::Provisional);
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        provisional.try_send(outbound),
        EndpointEnqueueRejection::Provisional,
        &expected,
        &mut accepted,
    );
    assert!(provisional_rx.try_recv().is_err());
    acknowledge(outbound, &mut accepted);

    assert!(provisional.activate(owner, &binding_ref));
    assert!(provisional.seal(owner, &binding_ref));
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        provisional.try_send(outbound),
        EndpointEnqueueRejection::Sealed,
        &expected,
        &mut accepted,
    );
    assert!(provisional_rx.try_recv().is_err());
    acknowledge(outbound, &mut accepted);

    let (active, mut receiver, _, _) = binding(BindingAdmissionState::Active);
    assert!(matches!(
        active.try_send(OutboundStanza::new(Stanza::Message(Message::new(None)))),
        EndpointEnqueueOutcome::AcceptedOrUnknown
    ));
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        active.try_send(outbound),
        EndpointEnqueueRejection::Full,
        &expected,
        &mut accepted,
    );
    acknowledge(outbound, &mut accepted);
    receiver.try_recv().expect("drain first envelope");
    receiver.close();
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        active.try_send(outbound),
        EndpointEnqueueRejection::ChannelClosed,
        &expected,
        &mut accepted,
    );
    acknowledge(outbound, &mut accepted);
}

#[tokio::test]
async fn async_send_refusals_return_the_complete_envelope() {
    let (provisional, _receiver, _, _) = binding(BindingAdmissionState::Provisional);
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        provisional.send(outbound).await,
        EndpointEnqueueRejection::Provisional,
        &expected,
        &mut accepted,
    );
    acknowledge(outbound, &mut accepted);

    let (sealed, _receiver, owner, binding_ref) = binding(BindingAdmissionState::Active);
    assert!(sealed.seal(owner, &binding_ref));
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        sealed.send(outbound).await,
        EndpointEnqueueRejection::Sealed,
        &expected,
        &mut accepted,
    );
    acknowledge(outbound, &mut accepted);

    let (closed, receiver, _, _) = binding(BindingAdmissionState::Active);
    drop(receiver);
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        closed.send(outbound).await,
        EndpointEnqueueRejection::ChannelClosed,
        &expected,
        &mut accepted,
    );
    // A returned callback stays pending until its owner acknowledges or drops it.
    drop(outbound);
    assert!(matches!(
        accepted.try_recv(),
        Err(oneshot::error::TryRecvError::Closed)
    ));
}

#[test]
fn reserved_commit_refusals_return_the_envelope_and_capacity() {
    let (endpoint, mut receiver, owner, binding_ref) = binding(BindingAdmissionState::Active);
    let permit = endpoint.try_reserve_owned().expect("reserve capacity");
    assert!(endpoint.seal(owner, &binding_ref));
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        endpoint.commit_reserved(permit, outbound),
        EndpointEnqueueRejection::Sealed,
        &expected,
        &mut accepted,
    );
    assert_eq!(endpoint.queue.capacity(), 1);
    assert!(receiver.try_recv().is_err());
    acknowledge(outbound, &mut accepted);

    let (endpoint, mut receiver, _, _) = binding(BindingAdmissionState::Active);
    let permit = endpoint.try_reserve_owned().expect("reserve capacity");
    receiver.close();
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        endpoint.commit_reserved(permit, outbound),
        EndpointEnqueueRejection::ChannelClosed,
        &expected,
        &mut accepted,
    );
    assert_eq!(endpoint.queue.capacity(), 1);
    assert!(receiver.try_recv().is_err());
    acknowledge(outbound, &mut accepted);
}

#[test]
fn sealed_empty_socket_can_rebind_while_old_reservation_is_held() {
    let connection = crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::local_process(),
    );
    let old_binding = BindingRef::fresh(connection.clone());
    let old_owner = LocalConnectionBindingId::fresh();
    let (socket, receiver) = SocketOutboundEndpoint::channel_with_socket_receiver(1);
    let old = socket.bind(
        old_owner,
        Some(old_binding.clone()),
        BindingAdmissionState::Active,
    );
    let permit = old.try_reserve_owned().expect("reserve the old queue");
    assert!(old.seal(old_owner, &old_binding));
    let recovered = old
        .clone()
        .into_socket_endpoint_if_empty(old_owner, &old_binding, Some(&receiver))
        .expect("the paired receiver has no accepted frames");
    let fresh_binding = BindingRef::fresh(connection);
    let fresh = recovered.bind(
        LocalConnectionBindingId::fresh(),
        Some(fresh_binding),
        BindingAdmissionState::Active,
    );

    let (waiting, expected_waiting, mut waiting_accepted) = payload();
    let waiting = refused(
        fresh.try_send(waiting),
        EndpointEnqueueRejection::Full,
        &expected_waiting,
        &mut waiting_accepted,
    );
    acknowledge(waiting, &mut waiting_accepted);
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        old.commit_reserved(permit, outbound),
        EndpointEnqueueRejection::Sealed,
        &expected,
        &mut accepted,
    );
    acknowledge(outbound, &mut accepted);
    let (fresh_outbound, _, _fresh_accepted) = payload();
    assert!(matches!(
        fresh.try_send(fresh_outbound),
        EndpointEnqueueOutcome::AcceptedOrUnknown
    ));
    let mut receiver = receiver.into_receiver();
    assert!(receiver.try_recv().is_ok());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn receiver_proof_rejects_queued_frames_and_other_queues() {
    let connection = crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::local_process(),
    );
    let binding_ref = BindingRef::fresh(connection);
    let owner = LocalConnectionBindingId::fresh();
    let (socket, receiver) = SocketOutboundEndpoint::channel_with_socket_receiver(2);
    let (_other_socket, other_receiver) = SocketOutboundEndpoint::channel_with_socket_receiver(2);
    let endpoint = socket.bind(
        owner,
        Some(binding_ref.clone()),
        BindingAdmissionState::Active,
    );
    let permit = endpoint.try_reserve_owned().expect("hold one slot");
    let (outbound, expected, mut accepted) = payload();
    assert!(matches!(
        endpoint.try_send(outbound),
        EndpointEnqueueOutcome::AcceptedOrUnknown
    ));
    assert!(endpoint.seal(owner, &binding_ref));
    assert!(endpoint
        .clone()
        .into_socket_endpoint_if_empty(owner, &binding_ref, Some(&receiver))
        .is_err());
    assert!(endpoint
        .into_socket_endpoint_if_empty(owner, &binding_ref, Some(&other_receiver))
        .is_err());
    drop(permit);
    let mut receiver = receiver.into_receiver();
    let retained = receiver.try_recv().expect("accepted frame remains queued");
    assert_payload(&retained, &expected, &mut accepted);
    acknowledge(retained, &mut accepted);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn actor_reserved_permit_racing_seal_returns_full_envelope_without_custody_insert() {
    let connection = crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::local_process(),
    );
    let binding_ref = BindingRef::fresh(connection.clone());
    let owner = LocalConnectionBindingId::fresh();
    let (socket, mut receiver, custody) =
        SocketOutboundEndpoint::actor_channel(connection, 1, 4096).expect("bounded actor queue");
    let endpoint = socket.bind(
        owner,
        Some(binding_ref.clone()),
        BindingAdmissionState::Active,
    );
    let permit = endpoint
        .try_reserve_owned()
        .expect("reserve actor queue slot");
    assert!(endpoint.seal(owner, &binding_ref));
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        endpoint.commit_reserved(permit, outbound),
        EndpointEnqueueRejection::Sealed,
        &expected,
        &mut accepted,
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(custody.retained_count(), 0);
    assert!(custody.terminal_ids().expect("sealed custody").is_empty());
    acknowledge(outbound, &mut accepted);
}

#[test]
fn permit_binds_queue_and_admission_even_for_equal_ids() {
    let (socket_a, mut receiver_a) = SocketOutboundEndpoint::channel(1);
    let (socket_b, mut receiver_b) = SocketOutboundEndpoint::channel(1);
    let owner = LocalConnectionBindingId::fresh();
    let binding_ref = BindingRef::fresh(crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::local_process(),
    ));
    let a = socket_a.bind(
        owner,
        Some(binding_ref.clone()),
        BindingAdmissionState::Active,
    );
    let b = socket_b.bind(
        owner,
        Some(binding_ref.clone()),
        BindingAdmissionState::Active,
    );
    let a_permit = a.try_reserve_owned().expect("reserve A");
    let b_permit = b.try_reserve_owned().expect("reserve B");
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        b.commit_reserved(a_permit, outbound),
        EndpointEnqueueRejection::BindingMismatch,
        &expected,
        &mut accepted,
    );
    assert_eq!(a.queue.capacity(), 1);
    assert_eq!(b.queue.capacity(), 0);
    assert!(receiver_a.try_recv().is_err());
    assert!(receiver_b.try_recv().is_err());
    assert!(matches!(
        b.clone().commit_reserved(b_permit, outbound),
        EndpointEnqueueOutcome::AcceptedOrUnknown
    ));
    let queued = receiver_b.try_recv().expect("clone accepts its permit");
    assert_payload(&queued, &expected, &mut accepted);
    acknowledge(queued, &mut accepted);

    // Isolate queue identity as well: both handles share one admission object,
    // but a permit from the first queue cannot publish to the second.
    let (socket_a, mut receiver_a) = SocketOutboundEndpoint::channel(1);
    let (socket_b, mut receiver_b) = SocketOutboundEndpoint::channel(1);
    let a = socket_a.bind(
        owner,
        Some(binding_ref.clone()),
        BindingAdmissionState::Active,
    );
    let b = BindingOutboundEndpoint {
        queue: socket_b.queue,
        admission: Arc::clone(&a.admission),
    };
    let a_permit = a.try_reserve_owned().expect("reserve queue A");
    let b_permit = b.try_reserve_owned().expect("reserve queue B");
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        b.commit_reserved(a_permit, outbound),
        EndpointEnqueueRejection::BindingMismatch,
        &expected,
        &mut accepted,
    );
    assert_eq!(a.queue.capacity(), 1);
    assert_eq!(b.queue.capacity(), 0);
    assert!(receiver_a.try_recv().is_err());
    assert!(receiver_b.try_recv().is_err());
    drop(b_permit);
    acknowledge(outbound, &mut accepted);

    let (socket, mut receiver) = SocketOutboundEndpoint::channel(1);
    let same_queue = SocketOutboundEndpoint {
        queue: Arc::clone(&socket.queue),
    };
    let a = socket.bind(
        owner,
        Some(binding_ref.clone()),
        BindingAdmissionState::Active,
    );
    let b = same_queue.bind(owner, Some(binding_ref), BindingAdmissionState::Active);
    let permit = a.try_reserve_owned().expect("reserve shared queue");
    let (outbound, expected, mut accepted) = payload();
    let outbound = refused(
        b.commit_reserved(permit, outbound),
        EndpointEnqueueRejection::BindingMismatch,
        &expected,
        &mut accepted,
    );
    assert_eq!(a.queue.capacity(), 1);
    assert!(receiver.try_recv().is_err());
    acknowledge(outbound, &mut accepted);
}

#[tokio::test]
async fn blocked_send_rechecks_seal_without_seal_waking_the_waiter() {
    let (endpoint, mut receiver, owner, binding_ref) = binding(BindingAdmissionState::Active);
    assert!(matches!(
        endpoint.try_send(OutboundStanza::new(Stanza::Message(Message::new(None)))),
        EndpointEnqueueOutcome::AcceptedOrUnknown
    ));
    let (outbound, expected, mut accepted) = payload();
    let mut send = Box::pin(endpoint.send(outbound));
    assert!(futures::poll!(send.as_mut()).is_pending());
    assert!(endpoint.seal(owner, &binding_ref));
    assert!(futures::poll!(send.as_mut()).is_pending());
    assert!(matches!(
        accepted.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    receiver.recv().await.expect("release capacity");
    let outbound = refused(
        send.await,
        EndpointEnqueueRejection::Sealed,
        &expected,
        &mut accepted,
    );
    assert_eq!(endpoint.queue.capacity(), 1);
    assert!(receiver.try_recv().is_err());
    acknowledge(outbound, &mut accepted);
}

#[tokio::test]
async fn cancelled_waits_release_capacity_and_send_drops_its_envelope() {
    let (endpoint, mut receiver, _, _) = binding(BindingAdmissionState::Active);
    let held = endpoint.try_reserve_owned().expect("reserve capacity");
    let mut reserve = Box::pin(endpoint.reserve_owned());
    assert!(futures::poll!(reserve.as_mut()).is_pending());
    drop(reserve);
    drop(held);
    assert_eq!(endpoint.queue.capacity(), 1);

    assert!(matches!(
        endpoint.try_send(OutboundStanza::new(Stanza::Message(Message::new(None)))),
        EndpointEnqueueOutcome::AcceptedOrUnknown
    ));
    let (outbound, _, mut accepted) = payload();
    let mut send = Box::pin(endpoint.send(outbound));
    assert!(futures::poll!(send.as_mut()).is_pending());
    drop(send);
    assert!(matches!(
        accepted.try_recv(),
        Err(oneshot::error::TryRecvError::Closed)
    ));
    receiver.recv().await.expect("drain original frame");
    assert_eq!(endpoint.queue.capacity(), 1);
    assert!(endpoint.try_reserve_owned().is_ok());
}

#[test]
fn concurrent_close_never_returns_an_accepted_envelope() {
    for _ in 0..64 {
        let (endpoint, receiver, _, _) = binding(BindingAdmissionState::Active);
        let permit = endpoint.try_reserve_owned().expect("reserve capacity");
        let (outbound, expected, mut accepted) = payload();
        let barrier = std::sync::Barrier::new(2);
        let (outcome, mut receiver) = std::thread::scope(|scope| {
            let commit = scope.spawn(|| {
                barrier.wait();
                endpoint.commit_reserved(permit, outbound)
            });
            let close = scope.spawn(|| {
                let mut receiver = receiver;
                barrier.wait();
                receiver.close();
                receiver
            });
            (
                commit.join().expect("commit thread"),
                close.join().expect("close thread"),
            )
        });
        match outcome {
            EndpointEnqueueOutcome::AcceptedOrUnknown => {
                if let Ok(queued) = receiver.try_recv() {
                    assert_payload(&queued, &expected, &mut accepted);
                    acknowledge(queued, &mut accepted);
                } else {
                    drop(receiver);
                    assert!(matches!(
                        accepted.try_recv(),
                        Err(oneshot::error::TryRecvError::Closed)
                    ));
                }
            }
            other => {
                let outbound = refused(
                    other,
                    EndpointEnqueueRejection::ChannelClosed,
                    &expected,
                    &mut accepted,
                );
                assert!(receiver.try_recv().is_err());
                acknowledge(outbound, &mut accepted);
            }
        }
    }
}
