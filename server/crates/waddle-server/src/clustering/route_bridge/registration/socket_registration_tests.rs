use super::{
    user_entity, Arc, ConnectionEntry, OrderedRelayDeliveryBridge, OrderedRelayDeliveryServices,
    RemoteResourceRegisterOutcome,
};
use crate::clustering::claims::NodeLeaseStore;
use crate::clustering::route_bridge::tests::{
    origin_identity, receiver_identity, services_with_claims, test_peer_id,
};
use crate::config::ClusteringMessagingConfig;
use std::collections::VecDeque;
use std::time::Duration;
use tokio::sync::mpsc;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ResumeIdentityProof, StalePredicate,
};
use waddle_xmpp::ownership::{ClaimStore, Entity, NodeIdentity};

#[derive(Clone)]
struct RegistrationLeaseFixture {
    services: Arc<OrderedRelayDeliveryServices>,
    store: Arc<crate::clustering::claims::PostgresClaimStore>,
    database: crate::db::Database,
    jid: jid::FullJid,
    local: NodeIdentity,
    remote: NodeIdentity,
}

impl RegistrationLeaseFixture {
    async fn try_new() -> Option<Self> {
        let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
        let database = crate::db::Database::from_config(
            "registration-demand-expiry",
            &crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, url)
                .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open postgres");
        let store = Arc::new(crate::clustering::claims::PostgresClaimStore::new(
            database.clone(),
        ));
        store.ensure_schema().await.expect("claim schema");
        let suffix = uuid::Uuid::new_v4();
        let local = NodeIdentity::new(format!("register-local-{suffix}"), "local-epoch");
        let remote = NodeIdentity::new(format!("register-remote-{suffix}"), "remote-epoch");
        for identity in [&local, &remote] {
            store.register(identity, None).await.expect("register node");
        }
        let jid: jid::FullJid = format!("demand-{suffix}@example.test/phone")
            .parse()
            .expect("jid");
        store
            .acquire(&user_entity(&jid.to_bare()), &remote)
            .await
            .expect("remote user claim");
        let mut services =
            services_with_claims(local.clone(), remote.clone(), local.clone(), test_peer_id())
                .await;
        services.claim_store = store.clone();
        services.node_lease = store.clone();
        services
            .user_registry
            .ask(
                waddle_xmpp::registry::user_registry::WireUserClusteringClaims {
                    claim_store: store.clone(),
                    node_identity: services.node_identity.clone(),
                },
            )
            .await
            .expect("wire registry claims");
        Some(Self {
            services: Arc::new(services),
            store,
            database,
            jid,
            local,
            remote,
        })
    }

    async fn lapse_heartbeat(&self, identity: &NodeIdentity) {
        self.database
            .guard()
            .await
            .expect("database guard")
            .execute(
                "UPDATE clustering_nodes SET heartbeat = now() - interval '60 seconds' WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![identity.node_id.clone(), identity.node_epoch.clone()],
            )
            .await
            .expect("lapse exact node heartbeat");
    }

    async fn register(&self) -> RemoteResourceRegisterOutcome {
        let (tx, _rx) = mpsc::channel(1);
        let entry = ConnectionEntry::new(tx);
        let owner = self
            .services
            .connection_registry
            .register_entry(self.jid.clone(), entry.clone());
        let bridge = OrderedRelayDeliveryBridge::new(
            tokio_util::sync::CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(self.services.clone());
        bridge
            .try_register_remote_user_resource(&self.jid, entry, owner)
            .await
    }

    async fn wait_for_blocked_expiry(&self, blocker_pid: i64) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let blocked = {
                    let connection = self.database.guard().await.expect("database guard");
                    let mut rows = connection
                        .query(
                            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE CAST(? AS bigint) = ANY(pg_blocking_pids(pid)) AND query LIKE '%UPDATE clustering_nodes%' AND query LIKE '%SET expired = true%')",
                            crate::db_params![blocker_pid],
                        )
                        .await
                        .expect("inspect blocked expiry");
                    rows.next()
                        .await
                        .expect("blocking query result")
                        .expect("blocking query row")
                        .get::<bool>(0)
                        .expect("blocking query column")
                };
                if blocked {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("demand expiry waits behind the held node row lock");
    }
}

#[tokio::test]
async fn registration_expires_heartbeat_stale_owner_without_waiting_for_watchdog() {
    let _serial = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    let Some(fixture) = RegistrationLeaseFixture::try_new().await else {
        return;
    };
    fixture.lapse_heartbeat(&fixture.remote).await;
    let entity = user_entity(&fixture.jid.to_bare());
    assert!(
        fixture
            .store
            .current_claim(&entity)
            .await
            .expect("claim lookup")
            .expect("remote claim")
            .owner_lease_fresh
    );

    assert_eq!(
        fixture.register().await,
        RemoteResourceRegisterOutcome::NotRemote,
        "a failed relay lookup must commit stale-node expiry before local claim recovery"
    );
    let (tx, _rx) = mpsc::channel(1);
    fixture
        .services
        .user_registry
        .ask(waddle_xmpp::registry::user_registry::RegisterUserResource {
            jid: fixture.jid.clone(),
            entry: ConnectionEntry::new(tx),
        })
        .await
        .expect("registry wins normal OwnerStale CAS after committed expiry");
    assert_eq!(
        fixture
            .store
            .current_claim(&entity)
            .await
            .expect("recovered claim lookup")
            .expect("recovered claim")
            .owner,
        fixture.local
    );
}

#[tokio::test]
async fn registration_demand_expiry_preserves_a_fresh_foreign_owner() {
    let _serial = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    let Some(fixture) = RegistrationLeaseFixture::try_new().await else {
        return;
    };
    assert_eq!(
        fixture.register().await,
        RemoteResourceRegisterOutcome::Retryable
    );
    let claim = fixture
        .store
        .current_claim(&user_entity(&fixture.jid.to_bare()))
        .await
        .expect("claim lookup")
        .expect("claim");
    assert_eq!(claim.owner, fixture.remote);
    assert!(claim.owner_lease_fresh);
}

#[tokio::test]
async fn registration_demand_expiry_requires_an_active_fresh_local_lease() {
    let _serial = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    for disable in [false, true] {
        let Some(fixture) = RegistrationLeaseFixture::try_new().await else {
            return;
        };
        fixture.lapse_heartbeat(&fixture.remote).await;
        if disable {
            fixture.services.node_identity.disable().await;
        } else {
            fixture.lapse_heartbeat(&fixture.local).await;
        }
        assert_eq!(
            fixture.register().await,
            RemoteResourceRegisterOutcome::Retryable
        );
        assert!(
            fixture
                .store
                .current_claim(&user_entity(&fixture.jid.to_bare()))
                .await
                .expect("claim lookup")
                .expect("claim")
                .owner_lease_fresh,
            "a stale or disabled local node must not expire the remote owner"
        );
    }
}

#[tokio::test]
async fn registration_demand_expiry_skips_a_changed_owner_or_claim_epoch() {
    let _serial = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    for change_owner in [false, true] {
        let Some(mut fixture) = RegistrationLeaseFixture::try_new().await else {
            return;
        };
        fixture.lapse_heartbeat(&fixture.remote).await;
        let entity = user_entity(&fixture.jid.to_bare());
        let attempted = fixture
            .store
            .current_claim(&entity)
            .await
            .expect("claim lookup")
            .expect("claim");
        let mut successor = attempted.clone();
        if change_owner {
            successor.owner = NodeIdentity::new("replacement-owner", "replacement-epoch");
        } else {
            successor.claim_epoch = ClaimEpoch(attempted.claim_epoch.0 + 1);
        }
        Arc::get_mut(&mut fixture.services)
            .expect("unshared fixture services")
            .claim_store = Arc::new(ScriptedClaims {
            reads: std::sync::Mutex::new(VecDeque::from([
                Ok(Some(attempted)),
                Ok(Some(successor)),
            ])),
        });
        assert_eq!(
            fixture.register().await,
            RemoteResourceRegisterOutcome::Retryable
        );
        assert!(
            fixture
                .store
                .current_claim(&entity)
                .await
                .expect("original owner lookup")
                .expect("original owner")
                .owner_lease_fresh,
            "a changed claim must not expire the originally attempted node"
        );
    }
}

#[tokio::test]
async fn registration_demand_expiry_rechecks_renewal_after_a_node_row_lock() {
    let _serial = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    let Some(fixture) = RegistrationLeaseFixture::try_new().await else {
        return;
    };
    fixture.lapse_heartbeat(&fixture.remote).await;
    let mut renewal = fixture.database.begin().await.expect("renewal transaction");
    let mut rows = renewal
        .query("SELECT pg_backend_pid()::bigint", ())
        .await
        .expect("renewal connection pid");
    let blocker_pid = rows
        .next()
        .await
        .expect("pid result")
        .expect("pid row")
        .get::<i64>(0)
        .expect("pid column");
    drop(rows);
    renewal
        .execute(
            "UPDATE clustering_nodes SET heartbeat = now() WHERE node_id = ? AND node_epoch = ?",
            crate::db_params![
                fixture.remote.node_id.clone(),
                fixture.remote.node_epoch.clone()
            ],
        )
        .await
        .expect("hold uncommitted heartbeat renewal");
    let registering = tokio::spawn({
        let fixture = fixture.clone();
        async move { fixture.register().await }
    });
    fixture.wait_for_blocked_expiry(blocker_pid).await;
    renewal
        .commit()
        .await
        .expect("commit heartbeat renewal before expiry");
    assert_eq!(
        registering.await.expect("registration task"),
        RemoteResourceRegisterOutcome::Retryable
    );
    assert!(
        fixture
            .store
            .current_claim(&user_entity(&fixture.jid.to_bare()))
            .await
            .expect("renewed owner lookup")
            .expect("renewed owner")
            .owner_lease_fresh,
        "expiry must recheck the renewed heartbeat after acquiring the row lock"
    );
}

/// The claim database can expire an owner while a relay lookup is in flight.
struct ScriptedClaims {
    reads: std::sync::Mutex<VecDeque<Result<Option<ClaimSnapshot>, ClaimError>>>,
}

#[async_trait::async_trait]
impl ClaimStore for ScriptedClaims {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn acquire(&self, _: &Entity, _: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn ensure_claimed(&self, _: &Entity, _: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn steal_stale(
        &self,
        _: &Entity,
        _: ClaimEpoch,
        _: StalePredicate,
        _: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("bridge never authorizes takeover")
    }
    async fn steal_for_resume(
        &self,
        _: &Entity,
        _: ClaimEpoch,
        _: ResumeIdentityProof,
        _: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn current_claim(&self, _: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        self.reads
            .lock()
            .expect("script lock")
            .pop_front()
            .expect("unexpected claim read")
    }
    async fn fence(&self, _: &Entity, _: &NodeIdentity, _: ClaimEpoch) -> Result<bool, ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn release(&self, _: &Entity, _: &NodeIdentity, _: ClaimEpoch) -> Result<(), ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn release_many(&self, _: &[Entity], _: &NodeIdentity) -> Result<(), ClaimError> {
        unreachable!("read-only bridge")
    }
}

fn foreign_claim(fresh: bool) -> Result<Option<ClaimSnapshot>, ClaimError> {
    Ok(Some(ClaimSnapshot {
        owner: NodeIdentity::new("registration-unreachable-owner", "test-epoch"),
        claim_epoch: ClaimEpoch(1),
        owner_lease_fresh: fresh,
    }))
}

async fn register_with_claim_reads(
    reads: impl IntoIterator<Item = Result<Option<ClaimSnapshot>, ClaimError>>,
) -> RemoteResourceRegisterOutcome {
    let mut services = services_with_claims(
        origin_identity(),
        origin_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.claim_store = Arc::new(ScriptedClaims {
        reads: std::sync::Mutex::new(reads.into_iter().collect()),
    });
    let jid: jid::FullJid = "alice@example.test/reconnect".parse().expect("jid");
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = services
        .connection_registry
        .register_entry(jid.clone(), entry.clone());
    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::new(services));
    bridge
        .try_register_remote_user_resource(&jid, entry, owner)
        .await
}

#[tokio::test]
async fn registration_rechecks_owner_expiry_after_remote_lookup_failure() {
    assert_eq!(
        register_with_claim_reads([foreign_claim(true), foreign_claim(false)]).await,
        RemoteResourceRegisterOutcome::NotRemote,
        "committed expiry during relay failure must return to local CAS registration",
    );
}

#[tokio::test]
async fn registration_preserves_fresh_foreign_owner_after_remote_lookup_failure() {
    assert_eq!(
        register_with_claim_reads([
            foreign_claim(true),
            foreign_claim(true),
            foreign_claim(true)
        ])
        .await,
        RemoteResourceRegisterOutcome::Retryable,
        "a disconnected relay cannot authorize local takeover of a live owner",
    );
}

#[tokio::test]
async fn registration_demand_expiry_requires_a_successful_post_cas_claim_read() {
    assert_eq!(
        register_with_claim_reads([
            foreign_claim(true),
            foreign_claim(true),
            Err(ClaimError::Backend("post-expiry lookup unavailable".into())),
        ])
        .await,
        RemoteResourceRegisterOutcome::Failed,
        "even successful expiry cannot authorize fallback when the final owner is unknown",
    );
}

#[tokio::test]
async fn registration_does_not_treat_initial_claim_backend_failure_as_a_missing_owner() {
    assert_eq!(
        register_with_claim_reads([Err(ClaimError::Backend("unavailable".into()))]).await,
        RemoteResourceRegisterOutcome::Failed,
        "failed owner lookup is not evidence that the claim is absent",
    );
}

#[tokio::test]
async fn registration_does_not_fallback_when_owner_recheck_fails() {
    assert_eq!(
        register_with_claim_reads([
            foreign_claim(true),
            Err(ClaimError::Backend("unavailable".into())),
        ])
        .await,
        RemoteResourceRegisterOutcome::Failed,
        "a relay failure plus unknown owner liveness cannot authorize local takeover",
    );
}
