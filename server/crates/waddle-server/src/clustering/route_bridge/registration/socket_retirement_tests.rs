use super::*;
use crate::clustering::route_bridge::{
    tests::{origin_identity, receiver_identity, services_with_claims, test_peer_id},
    track_remote_socket_for_test,
};

#[tokio::test]
async fn fresh_remote_retirement_interrupts_captured_socket_before_cleanup_ack() {
    let (services, bridge, entry, mut control, request) = fresh_remote_retirement_fixture().await;
    let jid = request.jid.clone();
    let retirement = bridge.force_detach_remote_user_resource_on_socket(request);
    tokio::pin!(retirement);
    assert!(futures::poll!(retirement.as_mut()).is_pending());
    let queued = control
        .try_recv()
        .expect("retirement is queued before interruption");
    assert!(
        entry.write_interruption.is_cancelled(),
        "captured socket must wake before cleanup acknowledgement"
    );

    let (tx, _rx) = mpsc::channel(1);
    let successor = ConnectionEntry::new(tx);
    services
        .connection_registry
        .register_entry(jid, successor.clone());
    queued.ack.send(ForceDetachOutcome::NotPersisted).unwrap();
    retirement.await;
    assert!(!successor.write_interruption.is_cancelled());
}

async fn fresh_remote_retirement_fixture() -> (
    Arc<OrderedRelayDeliveryServices>,
    Arc<OrderedRelayDeliveryBridge>,
    ConnectionEntry,
    mpsc::Receiver<ForceDetachRequest>,
    RelayForceDetachRemoteUserResource,
) {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(services.clone());
    let jid: jid::FullJid = "alice@example.test/retiring".parse().unwrap();
    let generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    *entry.occupancy_session.lock().unwrap() = Some(generation);
    let control = entry.take_force_detach_rx().unwrap();
    services
        .connection_registry
        .register_entry(jid.clone(), entry.clone());
    let registration_id = track_remote_socket_for_test(&bridge, &jid, entry.carbons_handle()).await;
    let request = RelayForceDetachRemoteUserResource {
        jid: jid.clone(),
        registration_id,
        occupancy_session: Some(generation),
        origin: waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement,
        requester_bare_jid: jid.to_bare(),
        trace: RelayTraceContext::default(),
    };
    (services, bridge, entry, control, request)
}

#[tokio::test]
async fn fresh_remote_retirement_refuses_mismatched_requester_before_enqueue() {
    let (_services, bridge, entry, mut control, mut request) =
        fresh_remote_retirement_fixture().await;
    request.requester_bare_jid = "mallory@example.test".parse().unwrap();
    let retirement = bridge.force_detach_remote_user_resource_on_socket(request);
    tokio::pin!(retirement);
    let reply = futures::poll!(retirement.as_mut());
    assert!(
        !entry.write_interruption.is_cancelled(),
        "another user cannot interrupt the socket"
    );
    assert!(
        matches!(control.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "identity rejection must not queue cleanup"
    );
    let std::task::Poll::Ready(reply) = reply else {
        panic!("identity rejection must not wait for socket cleanup");
    };
    assert_eq!(reply.outcome, ForceDetachOutcome::IdentityMismatch);
    assert_eq!(reply.status, RelayRemoteResourceForceDetachStatus::Refused);
}

#[tokio::test]
async fn fresh_remote_retirement_does_not_interrupt_when_control_queue_is_full() {
    let (_services, bridge, entry, control, request) = fresh_remote_retirement_fixture().await;
    let sender = entry.force_detach_sender();
    let queued = sender.capacity();
    for _ in 0..queued {
        let (ack, _ack_rx) = tokio::sync::oneshot::channel();
        sender
            .try_send(ForceDetachRequest {
                origin: request.origin,
                requester_bare_jid: request.requester_bare_jid.clone(),
                ack,
            })
            .unwrap();
    }
    let reply = bridge
        .force_detach_remote_user_resource_on_socket(request)
        .await;
    assert_eq!(reply.status, RelayRemoteResourceForceDetachStatus::Unknown);
    assert_eq!(
        control.len(),
        queued,
        "rejected cleanup cannot replace queued work"
    );
    assert!(!entry.write_interruption.is_cancelled());
}

#[tokio::test]
async fn fresh_remote_retirement_does_not_interrupt_when_control_queue_is_closed() {
    let (_services, bridge, entry, control, request) = fresh_remote_retirement_fixture().await;
    drop(control);
    let reply = bridge
        .force_detach_remote_user_resource_on_socket(request)
        .await;
    assert_eq!(reply.status, RelayRemoteResourceForceDetachStatus::Unknown);
    assert!(!entry.write_interruption.is_cancelled());
}

#[tokio::test]
async fn fresh_remote_retirement_does_not_interrupt_a_stale_registration() {
    let (_services, bridge, entry, mut control, mut request) =
        fresh_remote_retirement_fixture().await;
    request.registration_id = RemoteResourceRegistrationId::fresh();
    let reply = bridge
        .force_detach_remote_user_resource_on_socket(request)
        .await;
    assert_eq!(reply.status, RelayRemoteResourceForceDetachStatus::Unknown);
    assert!(!entry.write_interruption.is_cancelled());
    assert!(matches!(
        control.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn fresh_remote_retirement_does_not_interrupt_a_successor_owner() {
    let (services, bridge, entry, mut control, request) = fresh_remote_retirement_fixture().await;
    let (tx, _rx) = mpsc::channel(1);
    let successor = ConnectionEntry::new(tx);
    let mut successor_control = successor.take_force_detach_rx().unwrap();
    *successor.occupancy_session.lock().unwrap() =
        Some(waddle_xmpp_core::OccupancySessionGeneration::mint());
    services
        .connection_registry
        .register_entry(request.jid.clone(), successor.clone());
    let reply = bridge
        .force_detach_remote_user_resource_on_socket(request.clone())
        .await;
    assert_eq!(reply.status, RelayRemoteResourceForceDetachStatus::Unknown);
    assert!(!entry.write_interruption.is_cancelled());
    assert!(!successor.write_interruption.is_cancelled());
    assert!(matches!(
        control.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        successor_control.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert!(services
        .connection_registry
        .entry_if_owner(&request.jid, &successor.carbons_handle())
        .is_some());
}

#[tokio::test]
async fn fresh_remote_retirement_requires_the_captured_occupancy_generation() {
    for occupancy_session in [
        None,
        Some(waddle_xmpp_core::OccupancySessionGeneration::mint()),
    ] {
        let (_services, bridge, entry, mut control, mut request) =
            fresh_remote_retirement_fixture().await;
        request.occupancy_session = occupancy_session;
        let reply = bridge
            .force_detach_remote_user_resource_on_socket(request)
            .await;
        assert_eq!(reply.status, RelayRemoteResourceForceDetachStatus::Refused);
        assert!(!entry.write_interruption.is_cancelled());
        assert!(matches!(
            control.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }
}

#[tokio::test]
async fn nonfresh_remote_retirement_keeps_the_socket_write_uninterrupted() {
    use waddle_xmpp::registry::ForceDetachOrigin;
    for origin in [
        ForceDetachOrigin::CrossNodeResume,
        ForceDetachOrigin::OwnerManagedRetirement,
        ForceDetachOrigin::RegistryStaleActorRetirement,
    ] {
        let (_services, bridge, entry, mut control, mut request) =
            fresh_remote_retirement_fixture().await;
        request.origin = origin;
        let retirement = bridge.force_detach_remote_user_resource_on_socket(request);
        tokio::pin!(retirement);
        let initial_reply = futures::poll!(retirement.as_mut());
        let queued = control
            .try_recv()
            .expect("nonfresh cleanup must still be queued");
        assert!(!entry.write_interruption.is_cancelled());
        if origin == ForceDetachOrigin::RegistryStaleActorRetirement {
            let std::task::Poll::Ready(reply) = initial_reply else {
                panic!("stale actor retirement must acknowledge enqueue promptly");
            };
            assert_eq!(reply.status, RelayRemoteResourceForceDetachStatus::Detached);
        } else {
            assert!(
                initial_reply.is_pending(),
                "cleanup ACK remains authoritative"
            );
            queued.ack.send(ForceDetachOutcome::Detached).unwrap();
            assert_eq!(
                retirement.await.status,
                RelayRemoteResourceForceDetachStatus::Detached
            );
        }
        assert!(!entry.write_interruption.is_cancelled());
    }
}
