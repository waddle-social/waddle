use super::{
    mpsc, origin_identity, other_identity, presence, receiver_identity,
    remote_registration_request, remote_registration_request_from_entry, services_with_claims,
    target_bare, target_entity, target_full, test_peer_id, user_entity, Arc, AtomicBool,
    CancellationToken, ClaimError, ClaimStore, ClusteringMessagingConfig, ConnectionEntry,
    Duration, LocalResourcePresence, NodeId, NodeIdentity, OrderedRelayDeliveryBridge,
    OrderedRelayDeliveryServices, Ordering, RelayAskError, RelayRemoteResourceRegistrationStatus,
    RelayRemoteResourceUnregisterStatus, RelaySendEffect, RelaySendFailure, RelayTraceContext,
    RelayUnregisterRemoteUserResource, RemoteOwnerRegistration, RemoteResourceRegistrationId,
    RemoteResourceSocketGeneration, SocketLeaseRead, StaticNodeLease,
};
use crate::clustering::relay::RelayResourcePresenceReply;
use crate::clustering::resource_presence::{ClusterMembership, ResourcePresenceAsker};
use waddle_xmpp::stream_management::SmSessionRegistry as _;
use waddle_xmpp::telemetry::attributes::SweepOutcome;

// Pausing after database setup leaves a fractional timer tick. Tokio rounds
// deadlines up to its documented one-millisecond timer-wheel resolution.
const TIMER_GRANULARITY: Duration = Duration::from_millis(1);

struct SweepMembership {
    known: AtomicBool,
}

#[async_trait::async_trait]
impl ClusterMembership for SweepMembership {
    async fn peers(&self) -> Result<Vec<NodeIdentity>, ClaimError> {
        if self.known.load(Ordering::SeqCst) {
            Ok(vec![NodeIdentity::new("socket-node", "old-epoch")])
        } else {
            Err(ClaimError::Backend("membership unavailable".into()))
        }
    }
}

struct SweepPresence {
    socket: Option<Arc<OrderedRelayDeliveryBridge>>,
    reachable: AtomicBool,
}

struct GatedSweepPresence {
    presence: Arc<SweepPresence>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    first: AtomicBool,
}

#[async_trait::async_trait]
impl ResourcePresenceAsker for GatedSweepPresence {
    async fn resource_presence(
        &self,
        peer: &NodeIdentity,
        target: &jid::FullJid,
    ) -> Result<RelayResourcePresenceReply, RelayAskError> {
        let reply = self.presence.resource_presence(peer, target).await?;
        if self.first.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(reply)
    }
}

struct HealthyMirror {
    owner: Arc<OrderedRelayDeliveryBridge>,
    services: Arc<OrderedRelayDeliveryServices>,
    socket: Arc<OrderedRelayDeliveryBridge>,
    presence: Arc<SweepPresence>,
    membership: Arc<SweepMembership>,
    lease: Arc<std::sync::Mutex<SocketLeaseRead>>,
}

impl HealthyMirror {
    async fn new() -> Self {
        Self::with_gate(false).await.0
    }

    async fn with_gate(gated: bool) -> (Self, Option<Arc<GatedSweepPresence>>) {
        let socket = healthy_socket_node().await;
        let presence = Arc::new(SweepPresence {
            socket: Some(Arc::clone(&socket)),
            reachable: AtomicBool::new(true),
        });
        let membership = Arc::new(SweepMembership {
            known: AtomicBool::new(true),
        });
        let gate = gated.then(|| {
            Arc::new(GatedSweepPresence {
                presence: Arc::clone(&presence),
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
                first: AtomicBool::new(true),
            })
        });
        let asker: Arc<dyn ResourcePresenceAsker> = match &gate {
            Some(gate) => gate.clone(),
            None => presence.clone(),
        };
        let (owner, services, lease) = setup_with_presence(asker, membership.clone()).await;
        (
            Self {
                owner,
                services,
                socket,
                presence,
                membership,
                lease,
            },
            gate,
        )
    }
}

#[async_trait::async_trait]
impl ResourcePresenceAsker for SweepPresence {
    async fn resource_presence(
        &self,
        peer: &NodeIdentity,
        target: &jid::FullJid,
    ) -> Result<RelayResourcePresenceReply, RelayAskError> {
        assert_eq!(peer, &NodeIdentity::new("socket-node", "old-epoch"));
        if !self.reachable.load(Ordering::SeqCst) {
            return Err(RelayAskError::Send {
                failure: RelaySendFailure::ReplyTimeout,
                effect: RelaySendEffect::NoEffect,
                message: "socket node unreachable".into(),
            });
        }
        Ok(match &self.socket {
            Some(socket) => match socket.resource_presence_local(target).await {
                LocalResourcePresence::Present => RelayResourcePresenceReply::Present,
                LocalResourcePresence::Absent => RelayResourcePresenceReply::Absent,
            },
            None => RelayResourcePresenceReply::Present,
        })
    }
}

async fn setup() -> (
    Arc<OrderedRelayDeliveryBridge>,
    Arc<OrderedRelayDeliveryServices>,
    Arc<std::sync::Mutex<SocketLeaseRead>>,
) {
    setup_with_presence(
        Arc::new(SweepPresence {
            socket: None,
            reachable: AtomicBool::new(true),
        }),
        Arc::new(SweepMembership {
            known: AtomicBool::new(true),
        }),
    )
    .await
}

async fn setup_with_presence(
    presence: Arc<dyn ResourcePresenceAsker>,
    membership: Arc<dyn ClusterMembership>,
) -> (
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
    services.resource_presence = Some(presence);
    services.cluster_membership = Some(membership);
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
        .register_remote_user_resource_on_owner(
            remote_registration_request(&bridge, target_full(), NodeId::new("socket-node".into()))
                .await,
        )
        .await;
    assert_eq!(
        reply.status,
        RelayRemoteResourceRegistrationStatus::Registered
    );
    (bridge, services, lease)
}

async fn healthy_socket_node() -> Arc<OrderedRelayDeliveryBridge> {
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        NodeIdentity::new("socket-node", "old-epoch"),
        test_peer_id(),
    )
    .await;
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::new(services));
    bridge
}

#[tokio::test]
async fn owner_sweep_retires_absent_resource_on_healthy_socket_node_without_delivery() {
    let socket = healthy_socket_node().await;
    let sockets = &socket
        .services
        .get()
        .expect("socket services")
        .connection_registry;
    let (tx, _rx) = mpsc::channel(1);
    sockets.register(target_full(), tx);
    let presence = Arc::new(SweepPresence {
        socket: Some(Arc::clone(&socket)),
        reachable: AtomicBool::new(true),
    });
    let membership = Arc::new(SweepMembership {
        known: AtomicBool::new(true),
    });
    let (bridge, services, _lease) = setup_with_presence(presence, membership).await;
    assert_mirror_exists(&bridge, &services, true).await;
    // The socket disappears while the unregister relay to its owner is lost.
    sockets.unregister(&target_full());
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test]
async fn owner_sweep_defers_unreachable_peer_and_unknown_membership_then_retries() {
    let fixture = HealthyMirror::new().await;
    fixture.presence.reachable.store(false, Ordering::SeqCst);
    assert_eq!(
        fixture.owner.sweep_remote_owner_resources().await,
        SweepOutcome::Deferred
    );
    assert_mirror_exists(&fixture.owner, &fixture.services, true).await;
    fixture.presence.reachable.store(true, Ordering::SeqCst);
    fixture.membership.known.store(false, Ordering::SeqCst);
    assert_eq!(
        fixture.owner.sweep_remote_owner_resources().await,
        SweepOutcome::Deferred
    );
    assert_mirror_exists(&fixture.owner, &fixture.services, true).await;
    fixture.membership.known.store(true, Ordering::SeqCst);
    assert_eq!(
        fixture.owner.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&fixture.owner, &fixture.services, false).await;
}

#[tokio::test]
async fn owner_sweep_preserves_real_peer_socket_and_peer_or_local_resumable_sessions() {
    let fixture = HealthyMirror::new().await;
    let socket_services = fixture.socket.services.get().expect("socket services");
    let (tx, _rx) = mpsc::channel(1);
    socket_services
        .connection_registry
        .register(target_full(), tx);
    assert_eq!(
        fixture.owner.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&fixture.owner, &fixture.services, true).await;
    socket_services
        .connection_registry
        .unregister(&target_full());
    let session = super::presence::detached_session(&target_full());
    let stream = session.stream_id.clone();
    socket_services
        .sm_session_registry
        .store_session(session)
        .await
        .expect("peer resumable session");
    assert_eq!(
        fixture.owner.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&fixture.owner, &fixture.services, true).await;
    socket_services
        .sm_session_registry
        .take_session(&stream)
        .await
        .expect("take peer session")
        .expect("peer session exists");
    fixture
        .services
        .sm_session_registry
        .store_session(super::presence::detached_session(&target_full()))
        .await
        .expect("local resumable session");
    assert_eq!(
        fixture.owner.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&fixture.owner, &fixture.services, true).await;
}

#[tokio::test]
async fn owner_sweep_retires_obsolete_mirror_but_preserves_rebound_socket() {
    let (fixture, gate) = HealthyMirror::with_gate(true).await;
    let gate = gate.expect("probe gate");
    let sweep = {
        let owner = Arc::clone(&fixture.owner);
        tokio::spawn(async move { owner.sweep_remote_owner_resources().await })
    };
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .expect("peer absence probe started");
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
    *entry.occupancy_session.lock().expect("occupancy mutex") = Some(generation);
    let socket_owner = entry.carbons_handle();
    fixture
        .socket
        .services
        .get()
        .expect("socket services")
        .connection_registry
        .register_entry(target_full(), entry.clone());
    crate::occupancy_authority::publish(
        &fixture.services.occupancy_database,
        &target_full(),
        generation,
    )
    .await
    .expect("rebound binding authority");
    gate.release.notify_one();
    assert_eq!(sweep.await.expect("sweep task"), SweepOutcome::Completed);
    assert_mirror_exists(&fixture.owner, &fixture.services, false).await;
    let socket_registry = &fixture
        .socket
        .services
        .get()
        .expect("socket services")
        .connection_registry;
    assert!(socket_registry
        .entry_if_owner(&target_full(), &socket_owner)
        .is_some());
    fixture
        .services
        .claim_store
        .ensure_claimed(&target_entity(), &receiver_identity())
        .await
        .expect("successor owner claim");
    assert_eq!(
        fixture
            .owner
            .register_remote_user_resource_on_owner(remote_registration_request_from_entry(
                target_full(),
                NodeId::new("socket-node".into()),
                &entry,
            ))
            .await
            .status,
        RelayRemoteResourceRegistrationStatus::Registered
    );
    assert_eq!(
        fixture.owner.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&fixture.owner, &fixture.services, true).await;
    assert!(socket_registry
        .entry_if_owner(&target_full(), &socket_owner)
        .is_some());
}

#[tokio::test]
async fn owner_sweep_preserves_successor_registered_during_peer_absence_probe() {
    let (fixture, gate) = HealthyMirror::with_gate(true).await;
    let gate = gate.expect("probe gate");
    let old = fixture.owner.remote_owner_resources.lock().await[&target_full()].clone();
    let sweep = {
        let owner = Arc::clone(&fixture.owner);
        tokio::spawn(async move { owner.sweep_remote_owner_resources().await })
    };
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .expect("peer absence probe started");
    // A delayed exact unregister lands, then a healthy successor is admitted
    // while the older absence reply is still on its way back to the owner.
    let reply = fixture
        .owner
        .unregister_remote_user_resource_on_owner(RelayUnregisterRemoteUserResource {
            jid: target_full(),
            registration_id: old.registration_id,
            socket_generation: old.socket_generation,
            trace: RelayTraceContext::default(),
        })
        .await;
    assert_eq!(
        reply.status,
        RelayRemoteResourceUnregisterStatus::Unregistered
    );
    fixture
        .services
        .claim_store
        .ensure_claimed(&target_entity(), &receiver_identity())
        .await
        .expect("successor owner claim");
    let (tx, _rx) = mpsc::channel(1);
    fixture
        .socket
        .services
        .get()
        .expect("socket services")
        .connection_registry
        .register(target_full(), tx);
    let request = remote_registration_request(
        &fixture.owner,
        target_full(),
        NodeId::new("socket-node".into()),
    )
    .await;
    assert_eq!(
        fixture
            .owner
            .register_remote_user_resource_on_owner(request)
            .await
            .status,
        RelayRemoteResourceRegistrationStatus::Registered
    );
    let successor = fixture.owner.remote_owner_resources.lock().await[&target_full()].clone();
    gate.release.notify_one();
    assert_eq!(sweep.await.expect("sweep task"), SweepOutcome::Completed);
    assert_mirror_exists(&fixture.owner, &fixture.services, true).await;
    let current = fixture.owner.remote_owner_resources.lock().await[&target_full()].clone();
    assert_eq!(current.registration_id, successor.registration_id);
    assert!(Arc::ptr_eq(&current.owner, &successor.owner));
    assert_eq!(
        fixture.owner.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&fixture.owner, &fixture.services, true).await;
}

#[tokio::test]
async fn owner_sweep_retries_healthy_cleanup_after_actor_release_and_inventory_contention() {
    let fixture = HealthyMirror::new().await;
    let release = gate_mirror_actor(&fixture.services).await;
    let sweep = {
        let owner = Arc::clone(&fixture.owner);
        tokio::spawn(async move { owner.sweep_remote_owner_resources().await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if fixture.owner.remote_owner_resources.lock().await[&target_full()].unregister_pending
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("sweep records owed cleanup before actor reply");
    let inventory = fixture.owner.remote_owner_resources.lock().await;
    release.send(()).expect("release actor");
    assert_eq!(sweep.await.expect("sweep task"), SweepOutcome::Deferred);
    assert!(inventory[&target_full()].unregister_pending);
    assert!(fixture
        .services
        .claim_store
        .current_claim(&target_entity())
        .await
        .expect("released claim read")
        .is_none());
    assert!(!fixture
        .services
        .connection_registry
        .is_connected(&target_full()));
    drop(inventory);
    assert_eq!(
        fixture.owner.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&fixture.owner, &fixture.services, false).await;
}

#[tokio::test]
async fn owner_sweep_retires_expired_mirror_without_releasing_foreign_claim() {
    let (bridge, services, lease) = setup().await;
    let original = services
        .claim_store
        .current_claim(&target_entity())
        .await
        .expect("original claim read")
        .expect("original owner claim");
    services
        .claim_store
        .release(&target_entity(), &original.owner, original.claim_epoch)
        .await
        .expect("release original claim");
    services
        .claim_store
        .acquire(&target_entity(), &other_identity())
        .await
        .expect("acquire foreign claim");
    let successor = services
        .claim_store
        .current_claim(&target_entity())
        .await
        .expect("successor claim read")
        .expect("foreign successor claim");
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;

    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed,
        "lost publication authority must not pin expired local routing copies"
    );
    assert_mirror_exists(&bridge, &services, false).await;
    let retained = services
        .claim_store
        .current_claim(&target_entity())
        .await
        .expect("retained claim read")
        .expect("foreign claim must survive local cleanup");
    assert_eq!(retained.owner, successor.owner);
    assert_eq!(retained.claim_epoch, successor.claim_epoch);
}

#[tokio::test]
async fn owner_sweep_retires_rolled_back_bind_mirror_without_rewriting_new_generation() {
    for expired_socket in [false, true] {
        let fixture = HealthyMirror::new().await;
        let generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
        // A fresh bind published authority, then rolled back its registration.
        // The mirror must not wait for its old generation to become current again.
        crate::occupancy_authority::publish(
            &fixture.services.occupancy_database,
            &target_full(),
            generation,
        )
        .await
        .expect("replacement binding authority");
        if expired_socket {
            *fixture.lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
        }
        assert_eq!(
            fixture.owner.sweep_remote_owner_resources().await,
            SweepOutcome::Completed,
            "obsolete local generation must converge; expired_socket={expired_socket}"
        );
        assert_mirror_exists(&fixture.owner, &fixture.services, false).await;
        assert!(crate::occupancy_authority::is_current(
            &fixture.services.occupancy_database,
            &target_full(),
            generation,
        )
        .await
        .expect("replacement generation read"));
    }
}

#[tokio::test]
async fn owner_sweep_retires_old_claim_mirrors_without_releasing_successor_claims() {
    for pending in [false, true] {
        for foreign in [false, true] {
            let fixture = HealthyMirror::new().await;
            // Capture the actor before changing ownership so this assertion cannot
            // initiate stale-actor retirement through a separate GetUser lookup.
            let actor = fixture
                .services
                .user_registry
                .ask(waddle_xmpp::registry::GetUser {
                    bare_jid: target_bare(),
                })
                .await
                .expect("owner actor lookup")
                .expect("owner actor");
            let claim = fixture
                .services
                .claim_store
                .current_claim(&target_entity())
                .await
                .expect("claim read")
                .expect("owner claim");
            fixture
                .services
                .claim_store
                .release(&target_entity(), &claim.owner, claim.claim_epoch)
                .await
                .expect("release ownership");
            if foreign {
                fixture
                    .services
                    .claim_store
                    .acquire(&target_entity(), &other_identity())
                    .await
                    .expect("foreign owner");
            }
            let successor = fixture
                .services
                .claim_store
                .current_claim(&target_entity())
                .await
                .expect("successor claim read");
            if pending {
                let registration =
                    fixture.owner.remote_owner_resources.lock().await[&target_full()].clone();
                fixture
                    .owner
                    .mark_remote_owner_unregister_pending(&target_full(), &registration)
                    .await;
            }
            assert_eq!(
                fixture.owner.sweep_remote_owner_resources().await,
                SweepOutcome::Completed,
                "local cleanup must converge; pending={pending}, foreign={foreign}"
            );
            assert!(!fixture
                .services
                .connection_registry
                .is_connected(&target_full()));
            assert!(!fixture
                .owner
                .remote_owner_resources
                .lock()
                .await
                .contains_key(&target_full()));
            assert_eq!(
                actor
                    .ask(waddle_xmpp::registry::user_actor::GetResources)
                    .await
                    .expect("retired actor resources"),
                Vec::<jid::FullJid>::new()
            );
            let retained = fixture
                .services
                .claim_store
                .current_claim(&target_entity())
                .await
                .expect("retained successor claim read");
            assert_eq!(
                retained.map(|claim| (claim.owner, claim.claim_epoch)),
                successor.map(|claim| (claim.owner, claim.claim_epoch))
            );
        }
    }
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

async fn insert_sweep_candidate(
    bridge: &OrderedRelayDeliveryBridge,
    jid: jid::FullJid,
    registration: RemoteOwnerRegistration,
) {
    let services = bridge.services.get().expect("wired sweep fixture");
    services
        .claim_store
        .ensure_claimed(
            &user_entity(&jid.to_bare()),
            &services.node_identity.current(),
        )
        .await
        .expect("candidate owner claim");
    crate::occupancy_authority::publish(
        &services.occupancy_database,
        &jid,
        registration.occupancy_session,
    )
    .await
    .expect("candidate generation authority");
    bridge
        .remote_owner_resources
        .lock()
        .await
        .insert(jid, registration);
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
async fn owner_sweep_preserves_present_unexpired_socket_and_failed_reads() {
    let (bridge, services, lease) = setup().await;
    // The exact resource is present on an unexpired socket node.
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

#[tokio::test]
async fn owner_sweep_bounds_stalled_lease_reads_and_retries_next_pass() {
    let (bridge, services, lease) = setup().await;
    tokio::time::pause();
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Stalled;
    let started = tokio::time::Instant::now();
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Failed
    );
    assert!(started.elapsed() <= Duration::from_secs(5) + TIMER_GRANULARITY);
    assert_mirror_exists(&bridge, &services, true).await;
    // Check recovery on real dependency time after the simulated stall.
    tokio::time::resume();
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
        insert_sweep_candidate(&bridge, jid, original.clone()).await;
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
            insert_sweep_candidate(&bridge, jid, original.clone()).await;
        }
        assert_eq!(
            bridge.sweep_remote_owner_resources().await,
            SweepOutcome::Completed
        );
    }
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test]
async fn owner_sweep_budget_preserves_unattempted_candidates_for_later_passes() {
    let (bridge, services, lease) = setup().await;
    {
        let original = bridge.remote_owner_resources.lock().await[&target_full()].clone();
        // A full page fits in the item limit, but only six stalled lease reads
        // fit the time budget. The last candidate must keep its place until
        // reached instead of being rotated with the unattempted page suffix.
        for index in 0..63 {
            let jid = format!("a{index:03}@example.test/resource")
                .parse()
                .expect("valid stalled mirror JID");
            let mut registration = original.clone();
            registration.socket_node = NodeId::new(format!("stalled-socket-{index}"));
            insert_sweep_candidate(&bridge, jid, registration).await;
        }
        bridge
            .remote_owner_resources
            .lock()
            .await
            .get_mut(&target_full())
            .expect("target mirror remains registered")
            .unregister_pending = true;
    }
    // SQLite runs on a background thread; finish SQL setup before virtual
    // time can auto-advance its pool-acquisition deadlines.
    tokio::time::pause();
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Stalled;
    for pass in 0..11 {
        // Ten bounded virtual passes attempt the first 60 stalled entries.
        // The final page reaches the pending target after three more stalls;
        // measure its registry cleanup on real dependency time.
        if pass == 10 {
            tokio::time::resume();
        }
        let started = tokio::time::Instant::now();
        assert_eq!(
            bridge.sweep_remote_owner_resources().await,
            SweepOutcome::Failed
        );
        assert!(started.elapsed() <= Duration::from_secs(30) + TIMER_GRANULARITY);
    }
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test]
async fn owner_sweep_cancellation_keeps_attempted_mirror_available_for_retry() {
    let (bridge, services, lease) = setup().await;
    tokio::time::pause();
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
    tokio::time::resume();
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
        insert_sweep_candidate(&bridge, jid, original.clone()).await;
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

#[tokio::test]
async fn owner_sweep_retires_1024_mirrors_within_thirty_seconds() {
    let (bridge, services, lease) = setup().await;
    let mut resources = vec![target_full()];
    for index in 0..1023 {
        let jid: jid::FullJid = format!("juliet@example.test/mirror-{index:04}")
            .parse()
            .expect("mirror JID");
        let reply = bridge
            .register_remote_user_resource_on_owner(
                remote_registration_request(
                    &bridge,
                    jid.clone(),
                    NodeId::new("socket-node".into()),
                )
                .await,
            )
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
    // Exercise the production ticker and real registry latency rather than
    // auto-advancing through maintenance intervals.
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

#[tokio::test]
async fn owner_sweep_pending_age_survives_retries_and_clears_with_incarnation() {
    let (bridge, _services, _lease) = setup().await;
    tokio::time::pause();
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
    let original = bridge.remote_owner_resources.lock().await[&target_full()].clone();
    for index in 0..6 {
        let mut registration = original.clone();
        registration.socket_node = NodeId::new(format!("socket-{index}"));
        registration.socket_identity =
            NodeIdentity::new(registration.socket_node.as_str(), "old-epoch");
        let jid = format!("a{index}@example.test/resource")
            .parse()
            .expect("earlier mirror JID");
        insert_sweep_candidate(bridge, jid, registration).await;
    }
}

#[tokio::test]
async fn owner_sweep_busy_child_reply_is_deferred_after_its_full_timeout() {
    let (bridge, services, lease) = setup().await;
    // Measure the actual child reply timeout and subsequent registry convergence.
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

#[tokio::test]
async fn owner_sweep_page_deadline_during_lease_read_is_deferred() {
    let (bridge, services, lease) = setup().await;
    add_six_live_socket_mirrors(&bridge).await;
    tokio::time::pause();
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
    assert!(started.elapsed() >= Duration::from_secs(30));
    assert!(started.elapsed() <= Duration::from_secs(30) + TIMER_GRANULARITY);
    assert_mirror_exists(&bridge, &services, true).await;
    tokio::time::resume();
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, false).await;
}

#[tokio::test]
async fn owner_sweep_page_deadline_during_busy_actor_is_deferred() {
    let (bridge, services, lease) = setup().await;
    add_six_live_socket_mirrors(&bridge).await;
    tokio::time::pause();
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
    assert!(started.elapsed() >= Duration::from_secs(30));
    assert!(started.elapsed() <= Duration::from_secs(30) + TIMER_GRANULARITY);
    assert!(bridge.remote_owner_resources.lock().await[&target_full()].unregister_pending);
    tokio::time::resume();
    release.send(()).expect("release owner actor");
    *lease.lock().expect("socket lease fixture lock") = SocketLeaseRead::Gone;
    assert_eq!(
        bridge.sweep_remote_owner_resources().await,
        SweepOutcome::Completed
    );
    assert_mirror_exists(&bridge, &services, false).await;
}
