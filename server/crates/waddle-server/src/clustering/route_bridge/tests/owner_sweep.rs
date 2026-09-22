use super::*;

async fn setup() -> (
    Arc<OrderedRelayDeliveryBridge>,
    Arc<OrderedRelayDeliveryServices>,
    Arc<std::sync::Mutex<SocketLeaseRead>>,
) {
    let socket = NodeIdentity::new("socket-node", "old-epoch");
    let lease = Arc::new(std::sync::Mutex::new(SocketLeaseRead::Present(socket)));
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.node_lease = Arc::new(StaticNodeLease {
        origin: origin_identity(),
        peer_id: test_peer_id(),
        socket_read: Some(Arc::clone(&lease)),
    });
    let services = Arc::new(services);
    services
        .user_registry
        .ask(waddle_xmpp::registry::WireUserClusteringClaims {
            claim_store: Arc::clone(&services.claim_store),
            node_identity: services.node_identity.clone(),
        })
        .await
        .expect("wire owner claim store");
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));
    let reply = bridge
        .register_remote_user_resource_on_owner(remote_registration_request(
            target_full(),
            NodeId::new("socket-node".into()),
        ))
        .await;
    assert_eq!(
        reply.status,
        RelayRemoteResourceRegistrationStatus::Registered
    );
    (bridge, services, lease)
}

async fn assert_mirror_exists(
    bridge: &OrderedRelayDeliveryBridge,
    services: &OrderedRelayDeliveryServices,
    exists: bool,
) {
    assert_eq!(
        services.connection_registry.is_connected(&target_full()),
        exists
    );
    assert_eq!(
        bridge
            .remote_owner_resources
            .lock()
            .await
            .contains_key(&target_full()),
        exists
    );
    let resources =
        waddle_xmpp::registry::try_get_resources_for_user(&services.user_registry, &target_bare())
            .await
            .expect("actor resources");
    assert_eq!(resources.contains(&target_full()), exists);
}

#[tokio::test]
async fn owner_sweep_retires_committed_expired_or_missing_socket_without_delivery() {
    let (bridge, services, lease) = setup().await;
    *lease.lock().unwrap() = SocketLeaseRead::Gone;
    assert!(bridge.sweep_remote_owner_resources().await);
    assert_mirror_exists(&bridge, &services, false).await;
    assert!(
        services
            .claim_store
            .current_claim(&target_entity())
            .await
            .expect("claim read")
            .is_none(),
        "the last retired mirror must release its UserActor claim"
    );
}

#[tokio::test]
async fn owner_sweep_retires_superseded_socket_epoch() {
    let (bridge, services, lease) = setup().await;
    *lease.lock().unwrap() =
        SocketLeaseRead::Present(NodeIdentity::new("socket-node", "new-epoch"));
    assert!(bridge.sweep_remote_owner_resources().await);
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test]
async fn owner_sweep_preserves_unreachable_unexpired_socket_and_failed_reads() {
    let (bridge, services, lease) = setup().await;
    // No socket relay is running. Only the committed node row governs expiry.
    assert!(bridge.sweep_remote_owner_resources().await);
    assert_mirror_exists(&bridge, &services, true).await;
    *lease.lock().unwrap() = SocketLeaseRead::Failed;
    assert!(!bridge.sweep_remote_owner_resources().await);
    assert_mirror_exists(&bridge, &services, true).await;
}

#[tokio::test(start_paused = true)]
async fn owner_sweep_bounds_stalled_lease_reads_and_retries_next_pass() {
    let (bridge, services, lease) = setup().await;
    *lease.lock().unwrap() = SocketLeaseRead::Stalled;
    let started = tokio::time::Instant::now();
    assert!(!bridge.sweep_remote_owner_resources().await);
    assert!(started.elapsed() <= Duration::from_secs(5));
    assert_mirror_exists(&bridge, &services, true).await;
    *lease.lock().unwrap() = SocketLeaseRead::Gone;
    assert!(bridge.sweep_remote_owner_resources().await);
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test]
async fn owner_sweep_old_registration_cannot_remove_successor_mirror() {
    let (bridge, services, lease) = setup().await;
    let old = bridge.remote_owner_resources.lock().await[&target_full()].clone();
    // A replacement publishes a new owner while an older mirror is still inventoried.
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target_full(), entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target_full(),
            entry,
        })
        .await
        .expect("successor actor resource");
    *lease.lock().unwrap() = SocketLeaseRead::Gone;
    assert!(bridge.sweep_remote_owner_resources().await);
    assert!(services
        .connection_registry
        .entry_if_owner(&target_full(), &owner)
        .is_some());
    assert!(services
        .connection_registry
        .entry_if_owner(&target_full(), &old.owner)
        .is_none());
    assert_eq!(
        waddle_xmpp::registry::try_get_resources_for_user(&services.user_registry, &target_bare())
            .await
            .expect("successor resources"),
        vec![target_full()]
    );
}

#[tokio::test]
async fn owner_sweep_pages_past_live_mirrors_to_expired_tail() {
    let (bridge, services, lease) = setup().await;
    let original = bridge.remote_owner_resources.lock().await[&target_full()].clone();
    // Fill a page with live mirrors sorting before the real target. These are
    // liveness-only candidates; their owner handles must never be touched.
    for index in 0..64 {
        let jid = format!("a{index:03}@example.test/resource")
            .parse()
            .unwrap();
        bridge
            .remote_owner_resources
            .lock()
            .await
            .insert(jid, original.clone());
    }
    assert!(bridge.sweep_remote_owner_resources().await);
    assert_mirror_exists(&bridge, &services, true).await;
    *lease.lock().unwrap() = SocketLeaseRead::Gone;
    assert!(bridge.sweep_remote_owner_resources().await);
    assert_mirror_exists(&bridge, &services, false).await;
    assert_eq!(bridge.remote_owner_resources.lock().await.len(), 64);
}
