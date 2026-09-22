use super::*;
use waddle_xmpp::telemetry::attributes::SweepOutcome;

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
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
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
    *lease.lock().expect("socket lease fixture lock") =
        SocketLeaseRead::Present(NodeIdentity::new("socket-node", "new-epoch"));
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test]
async fn owner_sweep_preserves_unreachable_unexpired_socket_and_failed_reads() {
    let (bridge, services, lease) = setup().await;
    // No socket relay is running. Only the committed node row governs expiry.
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, true).await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Failed;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Failed
    );
    assert_mirror_exists(&bridge, &services, true).await;
}

#[tokio::test(start_paused = true)]
async fn owner_sweep_bounds_stalled_lease_reads_and_retries_next_pass() {
    let (bridge, services, lease) = setup().await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Stalled;
    let started = tokio::time::Instant::now();
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Failed
    );
    assert!(started.elapsed() <= Duration::from_secs(5));
    assert_mirror_exists(&bridge, &services, true).await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
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
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
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
            .expect("valid synthetic mirror JID");
        bridge
            .remote_owner_resources
            .lock()
            .await
            .insert(jid, original.clone());
    }
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, true).await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, false).await;
    assert_eq!(bridge.remote_owner_resources.lock().await.len(), 64);
}

#[tokio::test]
async fn owner_sweep_revisits_failed_mirror_despite_continuous_later_arrivals() {
    let (bridge, services, lease) = setup().await;
    let original = bridge.remote_owner_resources.lock().await[&target_full()].clone();
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Failed;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Failed
    );
    assert_mirror_exists(&bridge, &services, true).await;

    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    for pass in 0..3 {
        // Keep the lexical suffix full on every pass, including after the
        // previous suffix was removed. A cursor that waits to reach an empty
        // suffix before wrapping never revisits the retained failed mirror.
        for index in 0..64 {
            let jid = format!("z{pass:03}-{index:03}@example.test/resource")
                .parse()
                .expect("valid later-arriving mirror JID");
            bridge
                .remote_owner_resources
                .lock()
                .await
                .insert(jid, original.clone());
        }
        assert_eq!(
            bridge.sweep_remote_owner_resources().await,
            SweepOutcome::Completed
        );
    }
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test(start_paused = true)]
async fn owner_sweep_budget_preserves_unattempted_candidates_for_later_passes() {
    let (bridge, services, lease) = setup().await;
    {
        let mut registrations = bridge.remote_owner_resources.lock().await;
        let original = registrations[&target_full()].clone();
        // A full page fits in the item limit, but only six stalled lease reads
        // fit the time budget. The last candidate must keep its place until
        // reached instead of being rotated with the unattempted page suffix.
        for index in 0..63 {
            let jid = format!("a{index:03}@example.test/resource")
                .parse()
                .expect("valid stalled mirror JID");
            let mut registration = original.clone();
            registration.socket_node = NodeId::new(format!("stalled-socket-{index}"));
            registrations.insert(jid, registration);
        }
        registrations
            .get_mut(&target_full())
            .expect("target mirror remains registered")
            .unregister_pending = true;
    }
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Stalled;
    for _ in 0..11 {
        let started = tokio::time::Instant::now();
        assert_eq!(
            bridge.sweep_remote_owner_resources().await,
            SweepOutcome::Failed
        );
        assert!(started.elapsed() <= Duration::from_secs(30));
    }
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test(start_paused = true)]
async fn owner_sweep_cancellation_keeps_attempted_mirror_available_for_retry() {
    let (bridge, services, lease) = setup().await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Stalled;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(1),
            bridge.sweep_remote_owner_resources()
        )
        .await
        .is_err(),
        "cancel the sweep while its lease lookup is in flight"
    );
    assert_mirror_exists(&bridge, &services, true).await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test]
async fn owner_sweep_contention_is_deferred_and_retried() {
    let (bridge, services, lease) = setup().await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    let guard = bridge.remote_owner_sweep_lock.lock().await;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Deferred
    );
    drop(guard);
    let guard = bridge.remote_owner_resources.lock().await;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Deferred
    );
    drop(guard);
    assert_mirror_exists(&bridge, &services, true).await;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test]
async fn owner_sweep_reads_each_socket_once_per_page_and_refreshes_next_page() {
    let (bridge, services, lease) = setup().await;
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Counted {
        identity: Some(NodeIdentity::new("socket-node", "old-epoch")),
        reads: Arc::clone(&reads),
    };
    let original = bridge.remote_owner_resources.lock().await[&target_full()].clone();
    for index in 0..64 {
        let jid = format!("a{index:03}@example.test/resource")
            .parse()
            .expect("mirror JID");
        bridge
            .remote_owner_resources
            .lock()
            .await
            .insert(jid, original.clone());
    }
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "one lease read per socket, not per mirror"
    );
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Counted {
        identity: None,
        reads: Arc::clone(&reads),
    };
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "refresh committed expiry on the next page"
    );
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test(start_paused = true)]
async fn owner_sweep_retires_1024_mirrors_within_thirty_seconds() {
    let (bridge, services, lease) = setup().await;
    let mut resources = vec![target_full()];
    for index in 0..1023 {
        let jid: jid::FullJid = format!("juliet@example.test/mirror-{index:04}")
            .parse()
            .expect("mirror JID");
        let reply = bridge
            .register_remote_user_resource_on_owner(remote_registration_request(
                jid.clone(),
                NodeId::new("socket-node".into()),
            ))
            .await;
        assert_eq!(
            reply.status,
            RelayRemoteResourceRegistrationStatus::Registered
        );
        resources.push(jid);
    }
    assert_eq!(bridge.remote_owner_resources.lock().await.len(), 1024);
    assert!(resources
        .iter()
        .all(|jid| services.connection_registry.is_connected(jid)));
    assert_eq!(
        waddle_xmpp::registry::try_get_resources_for_user(&services.user_registry, &target_bare())
            .await
            .expect("live actor mirror resources")
            .len(),
        1024
    );
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    let started = tokio::time::Instant::now();
    let mut ticker = crate::server::session_janitors::remote_owner_mirror_ticker();
    for _ in 0..16 {
        ticker.tick().await;
        assert_eq!(
            bridge.sweep_remote_owner_resources().await,
            SweepOutcome::Completed
        );
    }
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(bridge.remote_owner_resources.lock().await.is_empty());
    assert!(resources
        .iter()
        .all(|jid| !services.connection_registry.is_connected(jid)));
    assert!(waddle_xmpp::registry::try_get_resources_for_user(
        &services.user_registry,
        &target_bare()
    )
    .await
    .expect("retired actor mirror resources")
    .is_empty());
    assert!(services
        .claim_store
        .current_claim(&target_entity())
        .await
        .expect("claim after last real mirror retired")
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn owner_sweep_pending_age_survives_retries_and_clears_with_incarnation() {
    let (bridge, _services, _lease) = setup().await;
    let original = bridge.remote_owner_resources.lock().await[&target_full()].clone();
    bridge
        .mark_remote_owner_unregister_pending(&target_full(), &original)
        .await;
    tokio::time::advance(Duration::from_secs(10)).await;
    bridge
        .mark_remote_owner_unregister_pending(&target_full(), &original)
        .await;
    let mut registrations = bridge.remote_owner_resources.lock().await;
    let (_, pending, age) = registrations.sweep_backlog();
    assert_eq!(pending, 1);
    assert_eq!(age, Duration::from_secs(10));
    let mut successor = original.clone();
    successor.registration_id = RemoteResourceRegistrationId::fresh();
    successor.socket_generation =
        RemoteResourceSocketGeneration::next(Some(original.socket_generation));
    successor.owner = Arc::new(AtomicBool::new(false));
    registrations.insert(target_full(), successor);
    drop(registrations);
    // A late old-incarnation retry must not re-age the successor.
    bridge
        .mark_remote_owner_unregister_pending(&target_full(), &original)
        .await;
    let registrations = bridge.remote_owner_resources.lock().await;
    let (_, pending, age) = registrations.sweep_backlog();
    assert_eq!(
        pending, 0,
        "a new registration must not inherit old pending age"
    );
    assert_eq!(age, Duration::ZERO);
}

#[tokio::test]
async fn owner_sweep_actor_failure_is_failed_and_retains_pending_mirror() {
    let (bridge, services, lease) = setup().await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    services.user_registry.kill();
    services.user_registry.wait_for_shutdown().await;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Failed
    );
    assert!(services.connection_registry.is_connected(&target_full()));
    let registrations = bridge.remote_owner_resources.lock().await;
    assert!(registrations[&target_full()].unregister_pending);
    let (inventory, pending, _) = registrations.sweep_backlog();
    assert_eq!((inventory, pending), (1, 1));
}

async fn gate_mirror_actor(
    services: &OrderedRelayDeliveryServices,
) -> tokio::sync::oneshot::Sender<()> {
    let actor = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target_bare(),
        })
        .await
        .expect("get owner actor")
        .expect("owner actor exists");
    let entered = Arc::new(tokio::sync::Notify::new());
    let (release, release_rx) = tokio::sync::oneshot::channel();
    actor
        .tell(
            waddle_xmpp::registry::user_actor::test_support::GateMailbox {
                entered: Arc::clone(&entered),
                release_rx,
            },
        )
        .await
        .expect("gate owner actor");
    entered.notified().await;
    release
}

async fn add_six_live_socket_mirrors(bridge: &OrderedRelayDeliveryBridge) {
    let mut registrations = bridge.remote_owner_resources.lock().await;
    let original = registrations[&target_full()].clone();
    for index in 0..6 {
        let mut registration = original.clone();
        registration.socket_node = NodeId::new(format!("socket-{index}"));
        registration.socket_identity =
            NodeIdentity::new(registration.socket_node.as_str(), "old-epoch");
        let jid = format!("a{index}@example.test/resource")
            .parse()
            .expect("earlier mirror JID");
        registrations.insert(jid, registration);
    }
}

#[tokio::test(start_paused = true)]
async fn owner_sweep_busy_child_reply_is_deferred_after_its_full_timeout() {
    let (bridge, services, lease) = setup().await;
    let release = gate_mirror_actor(&services).await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    let started = tokio::time::Instant::now();
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Deferred
    );
    assert!(started.elapsed() >= waddle_xmpp::registry::user_registry::CHILD_ACTOR_TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(bridge.remote_owner_resources.lock().await[&target_full()].unregister_pending);
    release.send(()).expect("release owner actor");
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test(start_paused = true)]
async fn owner_sweep_page_deadline_during_lease_read_is_deferred() {
    let (bridge, services, lease) = setup().await;
    add_six_live_socket_mirrors(&bridge).await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::DelayedLive {
        delay: Duration::from_millis(4500),
    };
    // Six successful distinct-node reads consume 27s. The seventh gets the
    // remaining 3s page budget, not its full 5s dependency timeout.
    let started = tokio::time::Instant::now();
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Deferred
    );
    assert_eq!(started.elapsed(), Duration::from_secs(30));
    assert_mirror_exists(&bridge, &services, true).await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test(start_paused = true)]
async fn owner_sweep_page_deadline_during_busy_actor_is_deferred() {
    let (bridge, services, lease) = setup().await;
    add_six_live_socket_mirrors(&bridge).await;
    let registration = bridge.remote_owner_resources.lock().await[&target_full()].clone();
    bridge
        .mark_remote_owner_unregister_pending(&target_full(), &registration)
        .await;
    let release = gate_mirror_actor(&services).await;
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::DelayedLive {
        delay: Duration::from_millis(4750),
    };
    // Six live reads leave 1.5s for the owed unregister. The child's Busy
    // classification needs 2s, so the page cutoff cancels the waiting ask.
    let started = tokio::time::Instant::now();
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Deferred
    );
    assert_eq!(started.elapsed(), Duration::from_secs(30));
    assert!(bridge.remote_owner_resources.lock().await[&target_full()].unregister_pending);
    release.send(()).expect("release owner actor");
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, false).await;
}
