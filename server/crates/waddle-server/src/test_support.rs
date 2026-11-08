//! Minimal public fixtures for this crate's XEP integration suites.
//!
//! The helpers are test-only by construction and use `expect` internally when
//! creating fixed local fixtures; they are never enabled in release images.

#[cfg(feature = "test-support")]
pub mod iq_forward;
#[cfg(feature = "test-support")]
pub mod observer_quarantine;
#[cfg(feature = "test-support")]
pub mod postgres_proxy;
#[cfg(feature = "test-support")]
pub(crate) mod relation_guard_lab;
#[cfg(feature = "test-support")]
pub use relation_guard_lab::verify_disposable_relation_guard_chain;
#[cfg(all(feature = "test-support", feature = "clustering"))]
pub use relation_guard_lab::verify_recorded_room_archive;
#[cfg(feature = "test-support")]
mod ordered_claim;
#[cfg(feature = "test-support")]
mod shared_correctness;
#[cfg(feature = "test-support")]
pub use crate::pending_delivery::database::ordered_claim::{OrderedClaimError, OrderedClaimPhase};
#[cfg(feature = "test-support")]
pub use ordered_claim::{OrderedClaimAttemptLab, OrderedClaimLab, OrderedClaimObservationLab};
#[cfg(feature = "test-support")]
pub use shared_correctness::{
    probe_public_mam_acl_rollback, SharedCorrectnessFixture, SharedCorrectnessSubmitError,
    SharedPendingResolverFixture,
};
#[cfg(feature = "test-support")]
pub mod supervised_io;
#[cfg(feature = "test-support")]
pub mod actor_local_endpoint {
    pub use crate::server::routes::websocket::actor_local_endpoint::{
        ActorEndpointCleanupIndex, ActorLocalEndpointError, ActorLocalEndpointOwner,
        ActorLocalPublishError, ActorLocalWriteError, ActorPublicationProbe,
    };
    pub use crate::server::routes::websocket::{
        ConnectionActorDriverTestAdmissionError, ConnectionActorDriverTestGate,
        ConnectionActorDriverTestHandle, ConnectionActorDriverTestSnapshot,
        ConnectionActorDriverTestTicket, ConnectionActorDriverTestUnknownReason,
        PhysicalControlRecovery, PhysicalEnrollment, PhysicalLimits, PhysicalProducerRecovery,
        PhysicalRecovery, PhysicalReplayPayload, PhysicalReplayRecovery, PhysicalSnapshot,
        PhysicalWriteKind, PhysicalWriteRecovery,
    };
}
#[cfg(feature = "test-support")]
pub mod connection_actor {
    //! Dark, test-only vertical proof for a connection actor that owns the
    //! real WebSocket connection state and its reserved physical pump pair.
    pub use crate::server::routes::websocket::connection_actor::{
        canonical_actor_gate, start_connection_actor, start_connection_actor_with_effect,
        start_connection_actor_with_hooks, AcknowledgementResult, CanonicalActorAdmission,
        CanonicalActorGate, CompleteHandledInbound, CompleteHandledInboundAtLifecycleGate,
        CompleteHandledInboundBeforeLifecycleGate, CompletePrebindAuth,
        CompleteValidatedPrebindAuth, ConfirmBindingInstall, ConnectionActor,
        ConnectionActorBindingAttempt, ConnectionActorController, ConnectionActorError,
        ConnectionActorGateBarrier, ConnectionActorOperationId, ConnectionActorSequence,
        ConnectionActorSnapshot, ConnectionActorStartBarrier, ConnectionActorStartError,
        ConnectionActorStartHooks, ConnectionWireFrame, ContinuationArbiter, ContinuationError,
        ContinuationLimits, ContinuationOwnerClaim, ContinuationPhase, ContinuationTicket,
        CountableEffectDisposition, CountableInboundEffect, CountableStanzaEffect, InstallBinding,
        OutputAdmissionPause, OutputContinuation, PanicConnectionActor, PauseConnectionActor,
        PrebindAuthAttempt, PrebindAuthOutcome, PrebindAuthToken, PrebindInput, PrebindPhase,
        PrebindSaslInput, PrebindSnapshot, PrebindValidationAttempt, PrepareBinding,
        RequestAcknowledgement, ReserveHandledInbound, RetireBinding, SeedInboundCounterForTest,
        SnapshotConnection, SnapshotPrebind, StopConnectionActor,
    };
}

use crate::db::DatabasePool;
use crate::ingress::IngressAuthority;
use crate::notification_outbox::NotificationOutboxStore;
use crate::notification_settings_projection::NotificationSettingsProjectionStore;
use crate::pending_delivery::DatabasePendingDeliveryStorage;
use std::sync::Arc;

#[cfg(feature = "test-support")]
use crate::config::LineageConfig;
#[cfg(feature = "test-support")]
use crate::db::bootstrap::VerifiedGlobalAttachment;
#[cfg(feature = "test-support")]
use crate::db::physical_attestation::AttestedPgPool;
#[cfg(feature = "test-support")]
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
#[cfg(feature = "test-support")]
use crate::db::Database;
#[cfg(feature = "test-support")]
use crate::db::DatabaseError;
#[cfg(feature = "test-support")]
use crate::ingress_substrate::with_pooled_receipt_commit_ack_lost;
#[cfg(feature = "test-support")]
use crate::ingress_substrate::PostgresIngressSubstrate;
#[cfg(feature = "test-support")]
use crate::ingress_uow::{
    begin_raw_ingress_writer_with_capability, EffectReceiptRepository, IngressUnitOfWork,
    IngressUowError,
};
#[cfg(feature = "test-support")]
use sqlx::postgres::PgPool;

/// Prepare the legacy observation schema before applying registered migrations.
#[cfg(feature = "test-support")]
pub async fn initialize_room_observations_for_test(
    database: &Database,
) -> Result<(), crate::ingress_uow::ObservationError> {
    crate::ingress_uow::initialize_room_observations(database).await
}

/// Prepare both PostgreSQL space stores in a disposable owner fixture.
/// The caller supplies the expected identity, which is checked inside the owner
/// transaction. This does not attest its source or authorize serving.
#[cfg(feature = "test-support")]
pub async fn prepare_paired_space_stores_owner_for_test(
    db: Database,
    identity: &crate::db::lineage::PgIdentity,
) -> Result<(), crate::spaces_metadata::SpaceStoreAttachError> {
    crate::spaces_metadata::prepare_space_stores_owner(db, identity).await
}

/// Exercise the production SM store selection and attachment path.
#[cfg(feature = "test-support")]
pub async fn attach_sm_startup(
    selection: &crate::server::ResolvedXmppDatabaseUrl,
    global: &Database,
    clustering_enabled: bool,
) -> anyhow::Result<Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage>> {
    crate::server::attach_sm_startup_for_test(selection, global, clustering_enabled).await
}

/// Exercise the production pending-delivery store selection and attachment path.
#[cfg(feature = "test-support")]
pub async fn attach_pending_startup(
    selection: &crate::server::ResolvedXmppDatabaseUrl,
    global: &Database,
) -> anyhow::Result<DatabasePendingDeliveryStorage> {
    crate::server::attach_pending_startup_for_test(selection, global).await
}

/// Exercise production MAM startup selection without starting HTTP. The
/// observation includes the lineage alias target derived from the selected
/// production branch, if this attachment shares the global boundary.
#[cfg(feature = "test-support")]
pub async fn select_mam_storage_for_test(
    selection: &crate::server::ResolvedXmppDatabaseUrl,
    owner_handoff: Option<&crate::server::MamOwnerClaimHandoff>,
    clustering_enabled: bool,
    clustering_claim_pair_live: bool,
    global: &Database,
) -> anyhow::Result<(bool, Option<crate::db::lineage::DurableStore>)> {
    crate::server::select_mam_storage_for_test(
        selection,
        owner_handoff,
        clustering_enabled,
        clustering_claim_pair_live,
        global,
    )
    .await
}

/// Exercise production ingress authority selection without starting HTTP.
#[cfg(feature = "test-support")]
pub async fn create_ingress_authority_at_startup(
    config: &crate::config::IngressConfig,
    pool: &DatabasePool,
    lineage: &LineageConfig,
) -> Result<IngressAuthority, crate::ingress::IngressStartupError> {
    crate::server::create_ingress_authority_for_test(config, pool, lineage).await
}

pub use crate::server::routes::websocket::muc_invites::{
    claim_invite, list_invites, record_invite_at, InviteStorageError, OutstandingInvite,
    RecordOutcome,
};
/// The production `/ws` RFC 7395 router, so suites can serve a test state
/// over a real loopback WebSocket and drive the live connection loop.
pub use crate::server::routes::websocket::router;
pub use crate::server::routes::websocket::test_state::{
    create_test_session, register_test_connection, seed_local_account,
};
use crate::server::routes::websocket::test_state::{
    create_test_websocket_state_with_ingress_factory, empty_extension_manager, TestStateOverrides,
};
pub use crate::server::routes::websocket::WebSocketState;

/// Build the default in-memory WebSocket state the in-crate suites use
/// (SQLite in memory, single-node registries, the test ingress authority).
///
/// # Panics
/// Panics if database schema or WebSocket fixture construction fails.
pub async fn websocket_state() -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides::default(),
        || std::future::ready(None),
    )
    .await
}

/// Pause each receipt continuation attempt for a canonical message.
#[cfg(feature = "test-support")]
pub fn receipt_custody_probe(
    authority: &IngressAuthority,
    message: waddle_xmpp::ingress::MessageKey,
    lose_ack: bool,
) -> Arc<crate::ingress::ReceiptCustodyProbe> {
    authority.receipt_custody_test_probe(message, lose_ack)
}

/// Bind the same node fatal-fence token that production startup supplies.
#[cfg(feature = "test-support")]
pub fn bind_receipt_custody_fatal_fence(authority: &IngressAuthority, state: &WebSocketState) {
    authority
        .bind_receipt_fatal_fence(state.deps.app_state.node_lifecycle.fatal_fence_token())
        .expect("bind test receipt fatal fence");
}

/// Override the bounded receipt continuation capacity in a test fixture.
#[cfg(feature = "test-support")]
pub fn receipt_custody_capacity(authority: &IngressAuthority, batches: usize, receipts: usize) {
    authority.receipt_custody_test_capacity(batches, receipts);
}

/// Inspect retained pinned operations and their original backend observations.
#[cfg(feature = "test-support")]
pub fn receipt_custody_unresolved(
    authority: &IngressAuthority,
) -> Vec<crate::ingress::ReceiptCustodyUnresolved> {
    authority.receipt_custody_test_unresolved()
}

/// Build the real ingress executor without its owner-side GC task. The
/// existing database and lineage must already be prepared by the fixture.
#[cfg(feature = "test-support")]
pub fn quiescent_pinned_receipt_ingress(
    database: Database,
    lineage: LineageConfig,
) -> IngressAuthority {
    IngressAuthority::for_pinned_receipt_test(database, lineage)
}

/// Select the pinned receipt worker for the real executor in a dark fixture.
#[cfg(feature = "test-support")]
pub async fn select_pinned_receipt_custody(
    authority: &IngressAuthority,
    pinned: crate::db::physical_attestation::PinnedReceiptAuthority,
    grant: crate::clustering::singleton_lease::LeaseGrant,
) -> bool {
    authority
        .select_pinned_receipt_for_test(pinned, grant)
        .await
}

/// Reconstruct the versioned operation identity from a recovered intent key.
#[cfg(feature = "test-support")]
pub fn receipt_operation_from_intent(
    message: waddle_xmpp::ingress::MessageKey,
    key: &crate::ingress::EffectReceiptKey,
) -> crate::db::physical_attestation::ReceiptOperation {
    crate::ingress::receipt_operation_for_test(message, key)
}

/// Whether both the authority and its receipt worker carry this reserved lane.
#[cfg(feature = "test-support")]
pub fn receipt_custody_shares_ingress_lane(
    authority: &IngressAuthority,
    database: &Database,
) -> bool {
    authority.receipt_custody_shares_ingress_lane(database)
}

/// Build a test-only WebSocket fixture that advertises and dispatches the
/// dark SASL2 OAuthBearer path. This fixture does not assert TLS termination
/// or prove that a request arrived without early data.
#[cfg(feature = "test-support")]
pub async fn websocket_state_with_sasl2() -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            sasl2_test_capability: true,
            ..TestStateOverrides::default()
        },
        || std::future::ready(None),
    )
    .await
}

/// Resolve store URL provenance with the same normalization used by startup.
#[cfg(feature = "test-support")]
pub fn resolve_xmpp_database_url_for_test(
    explicit: Option<&str>,
    global: Option<&str>,
) -> crate::server::ResolvedXmppDatabaseUrl {
    crate::server::resolve_xmpp_database_url_values(explicit, global)
}

/// Opaque test handle for the production inbox startup result and its lineage.
#[cfg(feature = "test-support")]
pub struct InboxStartupForTest(crate::server::InboxStartup);

#[cfg(feature = "test-support")]
impl InboxStartupForTest {
    pub fn storage(&self) -> Arc<crate::inbox::DatabaseInboxStorage> {
        Arc::clone(self.0.storage())
    }

    pub async fn register_lineage(&self, state: &crate::server::AppState) -> anyhow::Result<()> {
        crate::server::register_inbox_lineage(state, &self.0).await
    }
}

/// Exercise the production inbox startup factory with a supplied database.
#[cfg(feature = "test-support")]
pub async fn create_inbox_startup_for_test(
    selection: &crate::server::ResolvedXmppDatabaseUrl,
    global: &Database,
) -> anyhow::Result<InboxStartupForTest> {
    crate::server::create_ingress_inbox_storage(selection, global)
        .await
        .map(InboxStartupForTest)
}

/// Consume a physically attested SQLx pool into the crate's database wrapper.
/// The pool and its opaque writer capability stay paired by construction.
#[cfg(feature = "test-support")]
pub fn database_from_attested_postgres_pool(name: &str, pool: AttestedPgPool) -> Database {
    Database::from_attested_postgres_pool(name, pool)
}

/// Wrap a physically attested pool in separate ordinary and ingress lanes.
#[cfg(feature = "test-support")]
pub async fn database_pool_from_attested_postgres_pool(
    name: &str,
    pool: AttestedPgPool,
    main_budget: u32,
    ingress_budget: u32,
) -> Result<DatabasePool, crate::db::DatabaseError> {
    DatabasePool::from_attested_postgres_pool(name, pool, main_budget, ingress_budget).await
}

/// Initialize MAM on the same SQLite pool used by a disposable fixture.
#[cfg(feature = "test-support")]
pub async fn initialize_mam_on_sqlite_test_pool(
    pool: &DatabasePool,
) -> Result<(), waddle_xmpp::mam::MamStorageError> {
    let sqlite = pool.global().sqlite_pool().ok_or_else(|| {
        waddle_xmpp::mam::MamStorageError::Database("test fixture requires SQLite".to_owned())
    })?;
    waddle_xmpp::mam::SqlxMamStorage::from_sqlite_pool(sqlite.clone()).await?;
    Ok(())
}

/// Wrap attested main/ingress and optional control-plane pools for integration
/// tests. All supplied pools must share the same attestation contract.
#[cfg(feature = "test-support")]
pub async fn database_pool_from_attested_postgres_pools(
    name: &str,
    pool: AttestedPgPool,
    control_plane_pool: Option<(AttestedPgPool, u32)>,
    main_budget: u32,
    ingress_budget: u32,
) -> Result<DatabasePool, crate::db::DatabaseError> {
    DatabasePool::from_attested_postgres_pools(
        name,
        pool,
        control_plane_pool,
        main_budget,
        ingress_budget,
    )
    .await
}

/// Read-only observations of a verified attachment with one slot per lane.
#[cfg(feature = "test-support")]
pub struct VerifiedAttachmentObservations {
    pub has_control_plane_pool: bool,
    pub has_control_system_execute: bool,
    pub control_plane_identity: Option<(String, String, String)>,
    pub second_control_plane_blocked: bool,
    pub main_available_while_control_plane_busy: bool,
    pub control_plane_available_while_main_busy: bool,
    pub second_main_blocked: bool,
    pub second_ingress_blocked: bool,
}

/// Exercise the private attachment without exposing its database or a raw
/// query interface. Transactions are read-only and remain inside this helper.
#[cfg(feature = "test-support")]
pub async fn observe_verified_attachment(
    attachment: &VerifiedGlobalAttachment,
    observer: &PgPool,
) -> Result<VerifiedAttachmentObservations, DatabaseError> {
    let database = attachment.database_for_test();
    let has_control_plane_pool = database.has_control_plane_pool();
    let has_control_system_execute = {
        let guard = database.guard().await?;
        let mut rows = guard
            .query(
                "SELECT pg_catalog.has_function_privilege(current_user, \
                 'pg_catalog.pg_control_system()'::pg_catalog.regprocedure, 'EXECUTE')",
                (),
            )
            .await?;
        rows.next()
            .await?
            .ok_or_else(|| DatabaseError::QueryFailed("missing privilege result".to_owned()))?
            .get::<bool>(0)?
    };

    let (
        control_plane_identity,
        second_control_plane_blocked,
        main_available_while_control_plane_busy,
        control_plane_available_while_main_busy,
    ) = if has_control_plane_pool {
        let guard = database.control_plane_guard().await?;
        let mut rows = guard
            .query(
                "SELECT pg_catalog.current_database(), pg_catalog.current_schema()::text, \
                 session_user::text",
                (),
            )
            .await?;
        let identity = rows
            .next()
            .await?
            .ok_or_else(|| DatabaseError::QueryFailed("missing control-plane identity".into()))?;
        let identity = (
            identity.get::<String>(0)?,
            identity.get::<String>(1)?,
            identity.get::<String>(2)?,
        );

        // Keep a statement active on one reserved pool while checking that
        // its own capacity is bounded and the other pool remains available.
        // Observe the server-side query before probing the second slot so the
        // capacity assertion does not depend on task scheduling latency.
        let control_plane_marker = "p21_control_plane_capacity_sentinel";
        let control_plane_query =
            format!("SELECT pg_catalog.pg_sleep(1) /* {control_plane_marker} */");
        let slow_control_plane_database = database.clone();
        let slow_control_plane = tokio::spawn(async move {
            slow_control_plane_database
                .control_plane_guard()
                .await?
                .query(&control_plane_query, ())
                .await?;
            Ok::<(), DatabaseError>(())
        });
        wait_for_active_pool_query(observer, control_plane_marker).await?;
        let second_control_plane_blocked =
            tokio::time::timeout(std::time::Duration::from_millis(75), async {
                database
                    .control_plane_guard()
                    .await?
                    .query("SELECT 1", ())
                    .await
            })
            .await
            .is_err();
        let main_available_while_control_plane_busy =
            tokio::time::timeout(std::time::Duration::from_millis(150), async {
                database.guard().await?.query("SELECT 1", ()).await
            })
            .await
            .is_ok();
        slow_control_plane
            .await
            .map_err(|error| DatabaseError::QueryFailed(error.to_string()))??;

        let main_marker = "p21_main_capacity_sentinel";
        let main_query = format!("SELECT pg_catalog.pg_sleep(1) /* {main_marker} */");
        let slow_main_database = database.clone();
        let slow_main = tokio::spawn(async move {
            slow_main_database
                .guard()
                .await?
                .query(&main_query, ())
                .await?;
            Ok::<(), DatabaseError>(())
        });
        wait_for_active_pool_query(observer, main_marker).await?;
        let control_plane_available_while_main_busy =
            tokio::time::timeout(std::time::Duration::from_millis(150), async {
                database
                    .control_plane_guard()
                    .await?
                    .query("SELECT 1", ())
                    .await
            })
            .await
            .is_ok();
        slow_main
            .await
            .map_err(|error| DatabaseError::QueryFailed(error.to_string()))??;

        (
            Some(identity),
            second_control_plane_blocked,
            main_available_while_control_plane_busy,
            control_plane_available_while_main_busy,
        )
    } else {
        (None, false, false, false)
    };

    let mut main = database.begin().await?;
    main.execute("SET TRANSACTION READ ONLY", ()).await?;
    let ingress_database = database.ingress_lane();
    let mut ingress = ingress_database.begin().await?;
    ingress.execute("SET TRANSACTION READ ONLY", ()).await?;
    let second_main_blocked =
        tokio::time::timeout(std::time::Duration::from_millis(200), database.begin())
            .await
            .is_err();
    drop(main);
    let mut next_main = database.begin().await?;
    next_main.execute("SET TRANSACTION READ ONLY", ()).await?;
    let second_ingress_blocked = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        ingress_database.begin(),
    )
    .await
    .is_err();

    Ok(VerifiedAttachmentObservations {
        has_control_plane_pool,
        has_control_system_execute,
        control_plane_identity,
        second_control_plane_blocked,
        main_available_while_control_plane_busy,
        control_plane_available_while_main_busy,
        second_main_blocked,
        second_ingress_blocked,
    })
}

#[cfg(feature = "test-support")]
async fn wait_for_active_pool_query(observer: &PgPool, marker: &str) -> Result<(), DatabaseError> {
    let wait = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let active: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE datname = pg_catalog.current_database() AND state = 'active' \
                 AND query LIKE $1)",
            )
            .bind(format!("%{marker}%"))
            .fetch_one(observer)
            .await?;
            if active {
                return Ok::<(), DatabaseError>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .map_err(|_| DatabaseError::QueryFailed("pool capacity query did not start".into()))?;
    wait
}

/// Probe the attached control-plane lane after a test has changed trusted
/// database state. This lets PostgreSQL integration tests prove that every
/// checkout re-runs the physical attestor and fails closed on drift.
#[cfg(feature = "test-support")]
pub async fn probe_verified_control_plane(
    attachment: &VerifiedGlobalAttachment,
) -> Result<(), DatabaseError> {
    let mut rows = attachment
        .database_for_test()
        .control_plane_guard()
        .await?
        .query("SELECT 1", ())
        .await?;
    rows.next()
        .await?
        .ok_or_else(|| DatabaseError::QueryFailed("control-plane probe returned no row".into()))?;
    Ok(())
}

/// Characterize that startup-only detached sessions are closed after a
/// reserved shared-pool lane has been created.
#[cfg(feature = "test-support")]
pub async fn detached_startup_connection_is_available(database: &Database) -> bool {
    database
        .detached_postgres_startup_connection()
        .await
        .is_ok()
}

/// Wrap an ordinary PostgreSQL pool without attaching any physical writer
/// authority. This supports wrong-pool identity checks in dark tests.
#[cfg(feature = "test-support")]
pub fn database_from_unattested_postgres_pool(name: &str, pool: PgPool) -> Database {
    Database::from_postgres_pool(name, pool)
}

/// Run the dark raw ingress writer against the attested pool carried by
/// `authority`. Supplying a different `database` must fail before DML.
#[cfg(feature = "test-support")]
pub async fn attested_raw_ingress_candidate_write(
    database: &Database,
    authority: &Database,
    key: uuid::Uuid,
) -> Result<(), IngressUowError> {
    let capability = writer_capability(authority)?;
    let mut transaction = begin_raw_ingress_writer_with_capability(database, &capability).await?;
    transaction
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![31_u8; 32]],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

/// Run the pre-capability raw writer against a disposable relation-guard
/// candidate. The candidate must reject it for lacking the schema prelock.
#[cfg(feature = "test-support")]
pub async fn legacy_ingress_candidate_write(
    database: &Database,
    key: uuid::Uuid,
) -> Result<(), IngressUowError> {
    let mut transaction = crate::ingress_uow::begin_raw_ingress_writer(database).await?;
    transaction
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![34_u8; 32]],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

/// Run an attested candidate write with an intentionally stale transaction
/// epoch proof. The candidate must reject the guarded DML.
#[cfg(feature = "test-support")]
pub async fn attested_raw_candidate_write_with_epoch(
    database: &Database,
    authority: &Database,
    key: uuid::Uuid,
    epoch: &str,
) -> Result<(), IngressUowError> {
    let capability = writer_capability(authority)?;
    let mut transaction = begin_raw_ingress_writer_with_capability(database, &capability).await?;
    transaction
        .execute(
            "SELECT set_config('waddle.protocol_epoch', ?, true)",
            crate::db_params![epoch.to_owned()],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![35_u8; 32]],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

/// Exercise the dark UoW entrypoint with the database's own physical writer
/// capability, without returning its raw transaction to the integration test.
#[cfg(feature = "test-support")]
pub async fn attested_uow_candidate_write(
    database: &Database,
    lineage: LineageConfig,
    key: uuid::Uuid,
) -> Result<(), IngressUowError> {
    let unit_of_work =
        IngressUnitOfWork::open(database.clone(), lineage)?.with_attested_pg_writer()?;
    let mut transaction = unit_of_work.begin().await?;
    transaction
        .transaction_mut()
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![32_u8; 32]],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

/// Record and read back a receipt through one attested UoW transaction. The
/// tuple reports exact-key presence, completion status, and receipt-key count
/// before that same caller-owned transaction commits.
#[cfg(feature = "test-support")]
pub async fn attested_uow_receipt_write(
    database: &Database,
    lineage: LineageConfig,
    key: waddle_xmpp::ingress::MessageKey,
    kind: crate::ingress_substrate::EffectReceiptKind,
    hash: &[u8; 32],
) -> Result<(bool, bool, usize), IngressUowError> {
    let unit_of_work =
        IngressUnitOfWork::open(database.clone(), lineage)?.with_attested_pg_writer()?;
    let mut transaction = unit_of_work.begin().await?;
    EffectReceiptRepository::record_receipt(&mut transaction, key, kind, hash).await?;
    let present = EffectReceiptRepository::contains(&mut transaction, key, kind, hash).await?;
    let complete = EffectReceiptRepository::receipts_complete(&mut transaction, key).await?;
    let keys = EffectReceiptRepository::keys(&mut transaction, key).await?;
    transaction.commit().await?;
    Ok((present, complete, keys.len()))
}

/// Record one canonical direct-effect receipt and scheduler descendant through
/// the same physically attested UoW transaction. An optional barrier exposes
/// only the backend PID and keeps that repository transaction open for an
/// owner-transition lock-order test.
#[cfg(feature = "test-support")]
pub async fn attested_uow_receipt_and_descendant_write(
    database: &Database,
    lineage: LineageConfig,
    key: waddle_xmpp::ingress::MessageKey,
    digest: waddle_xmpp::ingress::SemanticDigest,
    intent: waddle_xmpp::ingress::IngressEffectIntent,
    descendant: uuid::Uuid,
    barrier: Option<(
        tokio::sync::oneshot::Sender<i32>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
) -> Result<waddle_xmpp::ingress::ProtocolEpoch, IngressUowError> {
    use crate::ingress_uow::{
        CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
    };

    let unit_of_work = IngressUnitOfWork::open(database.clone(), lineage)?;
    let mut transaction = unit_of_work.begin().await?;
    let epoch = transaction.protocol_epoch();
    CanonicalMessageRepository::record_message(&mut transaction, key, &digest, None).await?;
    EffectIntentRepository::reconcile(&mut transaction, key, std::slice::from_ref(&intent), false)
        .await?;
    let receipt = crate::ingress::receipt_key(&intent)?;
    EffectReceiptRepository::record_receipt(
        &mut transaction,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await?;
    EffectDescendantRepository::attach(&mut transaction, key, &intent.semantic_key(), descendant)
        .await?;

    if let Some((ready, release)) = barrier {
        let mut rows = transaction
            .transaction_mut()
            .query("SELECT pg_catalog.pg_backend_pid()", ())
            .await?;
        let pid = rows
            .next()
            .await?
            .ok_or(IngressUowError::EpochProofMissing)?
            .get::<i32>(0)?;
        drop(rows);
        let _ = ready.send(pid);
        let _ = release.await;
    }

    transaction.commit().await?;
    Ok(epoch)
}

/// Simulate a lost successful COMMIT acknowledgement while retaining the
/// actual commit, then exercise the pooled receipt path's exact-key readback.
#[cfg(feature = "test-support")]
pub async fn attested_pooled_receipt_write_after_lost_ack(
    database: &Database,
    key: waddle_xmpp::ingress::MessageKey,
    kind: crate::ingress_substrate::EffectReceiptKind,
    hash: &[u8; 32],
) -> Result<(), IngressUowError> {
    with_pooled_receipt_commit_ack_lost(async {
        EffectReceiptRepository::record_receipt_pooled(database, key, kind, hash).await
    })
    .await
}

/// Hold an attested UoW's epoch row lock until the caller releases it. Only
/// the backend PID crosses the test-support boundary; the raw transaction
/// remains private to this helper.
#[cfg(feature = "test-support")]
pub async fn hold_attested_uow_epoch_lock(
    database: Database,
    lineage: LineageConfig,
    ready: tokio::sync::oneshot::Sender<i32>,
    release: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), IngressUowError> {
    let unit_of_work = IngressUnitOfWork::open(database, lineage)?.with_attested_pg_writer()?;
    let mut transaction = unit_of_work.begin().await?;
    let mut rows = transaction
        .transaction_mut()
        .query("SELECT pg_backend_pid()", ())
        .await?;
    let pid = rows
        .next()
        .await?
        .ok_or(IngressUowError::EpochProofMissing)?
        .get::<i32>(0)?;
    drop(rows);
    let _ = ready.send(pid);
    let _ = release.await;
    transaction.commit().await?;
    Ok(())
}

/// Exercise the dark substrate writer entrypoint without exposing the raw
/// transaction to the integration test.
#[cfg(feature = "test-support")]
pub async fn attested_substrate_candidate_write(
    database: &Database,
    key: uuid::Uuid,
) -> Result<(), IngressUowError> {
    let capability = writer_capability(database)?;
    let substrate = PostgresIngressSubstrate::open(database.clone())?;
    let mut transaction = substrate.begin_with_attested_pg_writer(&capability).await?;
    transaction
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![33_u8; 32]],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

#[cfg(feature = "test-support")]
fn writer_capability(
    database: &Database,
) -> Result<AttestedPostgresWriterCapability, IngressUowError> {
    database
        .attested_pg_writer_capability()
        .ok_or(IngressUowError::AttestedPoolRequired)
}

/// Overrides for production WebSocket admission in in-process tests.
#[cfg(feature = "test-support")]
pub struct WebSocketAdmissionTestConfig {
    pub capacity: crate::config::MaxWebSocketConnections,
    /// `None` is standalone; `Some(None)` represents missing clustered identity.
    pub clustered_identity: Option<Option<waddle_xmpp::ownership::SharedNodeIdentity>>,
    pub barriers: crate::websocket_transport::AdmissionTestBarriers,
}

#[cfg(feature = "test-support")]
pub async fn websocket_state_with_admission(
    config: WebSocketAdmissionTestConfig,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            websocket_admission: Some(config),
            ..Default::default()
        },
        || std::future::ready(None),
    )
    .await
}

/// Build a minimal WebSocket state over a caller-owned database pool and ingress authority.
///
/// The ingress authority must come from [`crate::ingress::IngressAuthority::try_new`] when a test
/// drives recovery through `trigger_maintenance`, because it owns the maintenance task.
pub async fn websocket_state_with_ingress(
    db_pool: Arc<DatabasePool>,
    ingress: Arc<IngressAuthority>,
) -> Arc<WebSocketState> {
    build_websocket_state_with_ingress(db_pool, move || std::future::ready(Some(ingress))).await
}

/// Build all fixture schemas before creating an authority that starts maintenance.
///
/// `start_ingress` runs only after the WebSocket state's final schema constructor
/// completes. The returned handle is the exact authority installed in the state,
/// so callers can bind recovery and join its real maintenance task.
///
/// # Panics
/// Panics if database schema or WebSocket fixture construction fails.
pub async fn websocket_state_with_ingress_factory<F, Fut>(
    db_pool: Arc<DatabasePool>,
    start_ingress: F,
) -> (Arc<WebSocketState>, Arc<IngressAuthority>)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = IngressAuthority>,
{
    let state = build_websocket_state_with_ingress(db_pool, move || async move {
        Some(Arc::new(start_ingress().await))
    })
    .await;
    let ingress = Arc::clone(&state.deps.protocol.ingress);
    (state, ingress)
}

async fn build_websocket_state_with_ingress<F, Fut>(
    db_pool: Arc<DatabasePool>,
    start_ingress: F,
) -> Arc<WebSocketState>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<Arc<IngressAuthority>>>,
{
    let database = db_pool.global().clone();
    let pending_delivery_storage = Arc::new(
        DatabasePendingDeliveryStorage::from_database(
            database.clone(),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("database-backed pending delivery test storage"),
    );
    let notification_settings_projection =
        Arc::new(NotificationSettingsProjectionStore::new(database.clone()));
    NotificationOutboxStore::try_new(database)
        .await
        .expect("notification outbox test schema");
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(db_pool),
            pending_delivery_storage: Some(pending_delivery_storage),
            notification_settings_projection: Some(notification_settings_projection),
            ..Default::default()
        },
        start_ingress,
    )
    .await
}
