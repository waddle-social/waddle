//! #1295: a socket node takes over a user whose UserActor owner is gone.
//!
//! After the owner crashes, or retires its identity at the end of a graceful
//! shutdown, nothing else reclaims a user whose only live socket is on this
//! node. The origin-refresh path therefore promotes locally when the claim is
//! absent or its owner is committed expired.
use super::*;
use waddle_xmpp::ownership::{ClaimSnapshot, ResumeIdentityProof, StalePredicate};
use waddle_xmpp::registry::ListUsers;

/// `current_claim` answers a fixed snapshot; the refresh path reads nothing else.
struct FixedClaimStore {
    snapshot: Option<ClaimSnapshot>,
}

#[async_trait::async_trait]
impl ClaimStore for FixedClaimStore {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        Ok(())
    }
    async fn acquire(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("the refresh path only reads the current claim")
    }
    async fn ensure_claimed(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("the refresh path only reads the current claim")
    }
    async fn steal_stale(
        &self,
        _entity: &Entity,
        _observed: ClaimEpoch,
        _staleness: StalePredicate,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("the refresh path only reads the current claim")
    }
    async fn steal_for_resume(
        &self,
        _entity: &Entity,
        _observed: ClaimEpoch,
        _witness: ResumeIdentityProof,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("the refresh path only reads the current claim")
    }
    async fn current_claim(&self, _entity: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        Ok(self.snapshot.clone())
    }
    async fn current_claim_after_pending_writes(
        &self,
        entity: &Entity,
    ) -> Result<Option<ClaimSnapshot>, ClaimError> {
        self.current_claim(entity).await
    }
    async fn fence(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
        _mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        unreachable!("the refresh path only reads the current claim")
    }
    async fn release(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
        _mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        unreachable!("the refresh path only reads the current claim")
    }
    async fn release_many(
        &self,
        _entities: &[Entity],
        _me: &NodeIdentity,
    ) -> Result<(), ClaimError> {
        unreachable!("the refresh path only reads the current claim")
    }
}

/// A socket on this node registered with a remote owner, whose claim now
/// reads as `snapshot`.
async fn socket_registered_with_departed_owner(
    snapshot: Option<ClaimSnapshot>,
) -> (
    Arc<OrderedRelayDeliveryBridge>,
    Arc<OrderedRelayDeliveryServices>,
    RemoteResourceOriginSnapshot,
) {
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.claim_store = Arc::new(FixedClaimStore { snapshot });
    let services = Arc::new(services);
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));
    let target = target_full();
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), entry);
    let departed = NodeId::new("departed-owner".to_owned());
    let registration_id = bridge
        .test_insert_remote_socket_registration(target.clone(), owner, departed.clone())
        .await;
    let socket_generation = bridge.remote_socket_resources.lock().await[&target].socket_generation;
    let origin = RemoteResourceOriginSnapshot {
        jid: target,
        registration_id,
        socket_generation,
        user_owner: departed,
    };
    (bridge, services, origin)
}

async fn assert_promoted_locally(
    bridge: &Arc<OrderedRelayDeliveryBridge>,
    services: &OrderedRelayDeliveryServices,
    origin: &RemoteResourceOriginSnapshot,
) {
    let refresh = bridge.refresh_remote_resource_origin(origin).await;
    assert!(
        matches!(refresh, RemoteResourceOriginRefresh::LocalOwner),
        "the socket node takes the user over: {refresh:?}"
    );
    assert!(
        !bridge
            .remote_socket_resources
            .lock()
            .await
            .contains_key(&origin.jid),
        "the registration with the departed owner is retired"
    );
    let users = services
        .user_registry
        .ask(ListUsers)
        .await
        .expect("list local users");
    assert!(
        users.contains(&origin.jid.to_bare()),
        "the live socket is in the new local UserActor: {users:?}"
    );
}

#[tokio::test]
async fn a_socket_whose_owner_retired_promotes_the_user_locally() {
    let (bridge, services, origin) = socket_registered_with_departed_owner(Some(ClaimSnapshot {
        owner: NodeIdentity::new("departed-owner", "departed-epoch"),
        claim_epoch: ClaimEpoch(7),
        owner_lease_fresh: false,
    }))
    .await;
    assert_promoted_locally(&bridge, &services, &origin).await;
}

#[tokio::test]
async fn a_socket_whose_user_has_no_owner_promotes_the_user_locally() {
    let (bridge, services, origin) = socket_registered_with_departed_owner(None).await;
    assert_promoted_locally(&bridge, &services, &origin).await;
}
