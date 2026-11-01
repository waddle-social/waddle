//! Minimal public fixtures for this crate's XEP integration suites.
//!
//! The helpers are test-only by construction and use `expect` internally when
//! creating fixed local fixtures; they are never enabled in release images.

#[cfg(feature = "test-support")]
pub mod iq_forward;

use crate::db::DatabasePool;
use crate::ingress::IngressAuthority;
use crate::notification_outbox::NotificationOutboxStore;
use crate::notification_settings_projection::NotificationSettingsProjectionStore;
use crate::pending_delivery::DatabasePendingDeliveryStorage;
use std::sync::Arc;

#[cfg(feature = "test-support")]
use crate::db::physical_attestation::AttestedPgPool;
#[cfg(feature = "test-support")]
use crate::db::Database;
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
use crate::{config::LineageConfig, db::physical_attestation::AttestedPostgresWriterCapability};
#[cfg(feature = "test-support")]
use sqlx::postgres::PgPool;

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
