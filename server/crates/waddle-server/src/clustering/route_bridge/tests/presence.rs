//! #1803 receiver side: the claim owner answers one exact full JID.
use super::*;
use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry as _};

/// Install `jid` in BOTH the connection registry and the actor tree, exactly
/// as production dual registration does, so the probe sees a live resource.
/// The returned receiver must stay alive for the probe: a closed channel
/// lets the registry reap the entry.
#[must_use]
async fn register_local_resource(
    services: &OrderedRelayDeliveryServices,
    jid: &jid::FullJid,
) -> tokio::sync::mpsc::Receiver<waddle_xmpp::registry::OutboundStanza> {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let _owner = services.connection_registry.register(jid.clone(), tx);
    let entry = services
        .connection_registry
        .get_entry(jid)
        .expect("connection entry exists immediately after register");
    assert!(
        crate::server::dual_registration::mirror_register(
            &services.user_registry,
            jid.clone(),
            entry,
        )
        .await,
        "fixture must install {jid} in the actor tree"
    );
    rx
}

/// The target resource every case asks about. `services_with_claims` gives
/// its account's `UserActor` claim to the `target_owner` argument, which is
/// what decides `Present`/`Absent` against `NotOwner`.
fn probe_target() -> jid::FullJid {
    target_full()
}

async fn bridge_owning_target(
    services: OrderedRelayDeliveryServices,
) -> Arc<OrderedRelayDeliveryBridge> {
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::new(services));
    bridge
}

/// Claim owned here, and the account's `UserActor` tree lists the exact
/// resource because a local socket is dual-registered on it — the healthy
/// sibling resource in the production ghost shape.
#[tokio::test]
async fn resource_presence_local_reports_a_live_local_resource_present() {
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    let target = probe_target();
    let _socket = register_local_resource(&services, &target).await;
    let bridge = bridge_owning_target(services).await;

    assert_eq!(
        bridge.resource_presence_local(&target).await,
        LocalResourcePresence::Present
    );
}

/// The exact production shape the guard exists for: the owner mirrors a
/// resource whose socket lives on ANOTHER node. The mirror is in the actor
/// tree, so the probe must answer `Present` even though nothing here has a
/// socket.
#[tokio::test]
async fn resource_presence_local_reports_a_registered_remote_mirror_present() {
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    let target = probe_target();
    let bridge = bridge_owning_target(services).await;
    assert_eq!(
        bridge
            .register_remote_user_resource_on_owner(remote_registration_request(
                target.clone(),
                NodeId::new("socket-node".to_string()),
            ))
            .await
            .status,
        RelayRemoteResourceRegistrationStatus::Registered,
        "fixture must install the production remote-hosted mirror"
    );

    assert_eq!(
        bridge.resource_presence_local(&target).await,
        LocalResourcePresence::Present
    );
}

/// A detached XEP-0198 session has no actor-tree entry but may resume at any
/// moment, so it is not an abandoned occupancy.
#[tokio::test]
async fn resource_presence_local_reports_a_resumable_session_present() {
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    let target = probe_target();
    services
        .sm_session_registry
        .store_session(detached_session(&target))
        .await
        .expect("store detached session");
    let bridge = bridge_owning_target(services).await;

    assert_eq!(
        bridge.resource_presence_local(&target).await,
        LocalResourcePresence::Present
    );
}

/// The only authoritative negative: this node owns the account's claim with a
/// fresh lease, its actor tree does not list the resource, and no resumable
/// session exists for it.
#[tokio::test]
async fn resource_presence_local_reports_an_unknown_resource_absent() {
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    let target = probe_target();
    // A DIFFERENT resource of the same account is live here: the claim is
    // per account, the answer must be per resource.
    let sibling: jid::FullJid = "juliet@example.test/web-healthy"
        .parse()
        .expect("sibling full jid");
    let _socket = register_local_resource(&services, &sibling).await;
    let bridge = bridge_owning_target(services).await;

    assert_eq!(
        bridge.resource_presence_local(&sibling).await,
        LocalResourcePresence::Present,
        "the healthy sibling is reachable"
    );
    assert_eq!(
        bridge.resource_presence_local(&target).await,
        LocalResourcePresence::Absent,
        "a live sibling resource must not vouch for the ghost resource"
    );
}

/// The claim moved (or was never here): this node has no authority to answer,
/// and the asker must not read that as absence.
#[tokio::test]
async fn resource_presence_local_without_the_claim_is_not_owner() {
    let services = services_with_claims(
        origin_identity(),
        // The account's claim belongs to another node.
        origin_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    let bridge = bridge_owning_target(services).await;

    assert_eq!(
        bridge.resource_presence_local(&probe_target()).await,
        LocalResourcePresence::NotOwner
    );
}

/// Fail closed: a durable SM store that cannot be read leaves absence
/// unproven, so the occupant keeps its seat.
#[tokio::test]
async fn resource_presence_local_with_a_failed_resumable_probe_is_present() {
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.sm_session_registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()
            .with_persistence(Arc::new(UnreadableSmPersistence::default())),
    );
    let target = probe_target();
    let bridge = bridge_owning_target(services).await;

    assert_eq!(
        bridge.resource_presence_local(&target).await,
        LocalResourcePresence::Present
    );
}

/// Fail closed: a control-plane read that never answers must resolve inside
/// the receiver's own budget and answer `Present`, not hang one delegated
/// relay task per repair attempt.
#[tokio::test]
async fn resource_presence_local_bounds_a_stalled_claim_store() {
    use super::reassert::stalled_claim::StalledClaimStore;

    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.claim_store = Arc::new(StalledClaimStore);
    let bridge = bridge_owning_target(services).await;

    let outcome = tokio::time::timeout(
        super::super::presence::RESOURCE_PRESENCE_CLAIM_READ_TIMEOUT + Duration::from_secs(3),
        bridge.resource_presence_local(&probe_target()),
    )
    .await
    .expect("a stalled claim store must not hang the executor");

    assert_eq!(outcome, LocalResourcePresence::Present);
}

fn detached_session(resource: &jid::FullJid) -> DetachedSession {
    DetachedSession {
        stream_id: resource.to_string(),
        user_id: resource.to_bare().to_string(),
        jid: resource.clone(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

/// Durable SM store whose enumeration always fails — the shape a pool
/// exhaustion or a lost connection takes at the probe seam. Every other
/// operation delegates, so the registry behaves normally otherwise.
#[derive(Default)]
struct UnreadableSmPersistence {
    inner: waddle_xmpp::stream_management::persistence::InMemorySmPersistence,
}

type SmResult<T> = Result<T, waddle_xmpp::stream_management::persistence::SmPersistenceError>;

#[async_trait::async_trait]
impl waddle_xmpp::stream_management::persistence::SmPersistenceStorage for UnreadableSmPersistence {
    async fn upsert_session(
        &self,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
    ) -> SmResult<()> {
        self.inner.upsert_session(session).await
    }

    async fn get_session(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> SmResult<Option<waddle_xmpp::stream_management::persistence::PersistedSession>> {
        self.inner.get_session(stream_id).await
    }

    async fn delete_session(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> SmResult<()> {
        self.inner.delete_session(stream_id).await
    }

    async fn append_unacked(
        &self,
        stanza: waddle_xmpp::stream_management::persistence::PersistedUnackedStanza,
    ) -> SmResult<()> {
        self.inner.append_unacked(stanza).await
    }

    async fn ack_through(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
        up_to_sequence: u32,
    ) -> SmResult<u64> {
        self.inner.ack_through(stream_id, up_to_sequence).await
    }

    async fn delete_unacked(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
        sequences: &[u32],
    ) -> SmResult<u64> {
        self.inner.delete_unacked(stream_id, sequences).await
    }

    async fn list_unacked(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> SmResult<Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>> {
        self.inner.list_unacked(stream_id).await
    }

    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> SmResult<Vec<waddle_xmpp::stream_management::persistence::PersistedSession>> {
        self.inner.list_expired_sessions(now).await
    }

    async fn list_all_sessions(
        &self,
    ) -> SmResult<Vec<waddle_xmpp::stream_management::persistence::PersistedSession>> {
        Err(
            waddle_xmpp::stream_management::persistence::SmPersistenceError::Other(
                "simulated durable read failure".to_string(),
            ),
        )
    }

    async fn store_session_atomic_with_principal(
        &self,
        principal: &waddle_xmpp::auth::AuthenticatedPrincipalRef,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
        unacked: Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
    ) -> SmResult<()> {
        self.inner
            .store_session_atomic_with_principal(principal, session, unacked)
            .await
    }

    async fn get_session_principal(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> SmResult<Option<waddle_xmpp::auth::AuthenticatedPrincipalRef>> {
        self.inner.get_session_principal(stream_id).await
    }

    async fn store_session_atomic_with_ingress_append(
        &self,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
        unacked: Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
        append: waddle_xmpp::stream_management::persistence::PersistedIngressAppend,
    ) -> SmResult<waddle_xmpp::stream_management::persistence::KeyedSnapshotOutcome> {
        self.inner
            .store_session_atomic_with_ingress_append(session, unacked, append)
            .await
    }

    async fn store_session_atomic_with_principal_and_ingress_appends(
        &self,
        principal: &waddle_xmpp::auth::AuthenticatedPrincipalRef,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
        unacked: Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
        appends: Vec<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>,
    ) -> SmResult<Vec<waddle_xmpp::stream_management::SmIngressAppendKey>> {
        self.inner
            .store_session_atomic_with_principal_and_ingress_appends(
                principal, session, unacked, appends,
            )
            .await
    }

    async fn get_ingress_append(
        &self,
        key: &waddle_xmpp::stream_management::SmIngressAppendKey,
    ) -> SmResult<Option<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>> {
        self.inner.get_ingress_append(key).await
    }
}
