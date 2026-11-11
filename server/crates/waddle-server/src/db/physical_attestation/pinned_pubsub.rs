//! Unselected PubSub relation inventory on one pinned read-only transaction.
//!
//! This inspects only relation identities and a narrow heap shape. It does not
//! inspect PubSub ACLs, functions, data, or the full role graph and does not
//! authorize a store or serving. The caller must independently fence old
//! writers and privileged or uncooperative catalog and ACL mutators. The
//! advisory fence coordinates only participants; v3 quiescence is procedural.

use crate::db::DarkEpochAclStage;
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::types::Oid;
use thiserror::Error;
use tokio::sync::{oneshot, Semaphore};
use tokio::time::Instant;

use super::pinned_entry::{prepare_read_only_entry, DarkPinnedEntryError, ENTRY_DEADLINE};
use super::{
    quote_identifier, read_epoch_after_prelock, AttestationMode, AttestedPgPool,
    DarkIngressAuthorityError, PinnedRuntimeAuthority, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use crate::db::{Database, DatabaseError, Transaction};

const RETRY_INTERVAL: Duration = Duration::from_millis(20);
const ROLLBACK_TIMEOUT: Duration = Duration::from_secs(5);

/// The order of independently supplied relation OIDs. The same fixed order is
/// used for preflight, locking, and the post-lock identity checks.
const RELATIONS: [&str; 12] = [
    "dnd_projection",
    "dnd_projection_source_version",
    "notification_settings_projection",
    "notification_settings_projection_source_version",
    "pubsub_affiliations",
    "pubsub_items",
    "pubsub_nodes",
    "pubsub_owner_configured",
    "pubsub_push_publications",
    "pubsub_schema_marker",
    "pubsub_schema_version",
    "pubsub_subscriptions",
];

#[derive(Debug, Error)]
pub enum PinnedPubSubInventoryError {
    #[error("PubSub inventory requires one pinned pool and distinct independent relation pins")]
    InvalidPins,
    #[error("PubSub relation {relation} does not match its pinned identity or heap shape")]
    CatalogDrift { relation: &'static str },
    #[error("PubSub inventory is at its bounded concurrency limit")]
    Overloaded,
    #[error("PubSub inventory requires a Tokio runtime")]
    RuntimeUnavailable,
    #[error("the PubSub inventory worker did not return a result")]
    WorkerLost,
    #[error("the PubSub inventory timed out")]
    Timeout,
    #[error("PubSub inventory rollback was not acknowledged; reconcile the original backend")]
    RollbackUnconfirmed,
    #[cfg(feature = "test-support")]
    #[error("the PubSub inventory test pause was cancelled")]
    PauseCancelled,
    #[error(transparent)]
    Entry(#[from] DarkPinnedEntryError),
    #[error(transparent)]
    Epoch(#[from] DarkIngressAuthorityError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("PubSub inventory PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// Owns the exact pinned pool and the independently provisioned OIDs in the
/// order documented on [`Self::new`]. No pool, connection, transaction, SQL
/// callback, or reusable proof can be extracted. A success is only a completed
/// read-only inventory and does not authorize attachment or serving.
pub struct PinnedPubSubRelationInventory {
    pool: AttestedPgPool,
    relation_oids: [u32; 12],
    admission: Arc<Semaphore>,
}

/// Receives an owned worker's result. Dropping this receiver does not cancel
/// the worker. Runtime shutdown or backend failure is not rollback proof.
pub struct PinnedPubSubInventoryJob {
    result: oneshot::Receiver<Result<(), PinnedPubSubInventoryError>>,
}

#[derive(Default)]
struct TestHooks {
    #[cfg(feature = "test-support")]
    before_fence: Option<TestCheckpoint>,
    #[cfg(feature = "test-support")]
    after_retry_rollback: Option<TestCheckpoint>,
    #[cfg(feature = "test-support")]
    after_inventory: Option<InventoryPause>,
    #[cfg(feature = "test-support")]
    stall_after_inventory: Option<oneshot::Sender<i32>>,
}

#[cfg(feature = "test-support")]
struct TestCheckpoint {
    ready: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

#[cfg(feature = "test-support")]
struct InventoryPause {
    ready: oneshot::Sender<(i32, bool, bool)>,
    release: oneshot::Receiver<()>,
}

impl PinnedPubSubInventoryJob {
    pub async fn finish(self) -> Result<(), PinnedPubSubInventoryError> {
        self.result
            .await
            .map_err(|_| PinnedPubSubInventoryError::WorkerLost)?
    }
}

impl PinnedPubSubRelationInventory {
    /// Fixed inventory names for the test comparing this list with the live
    /// PubSub attachment table list.
    #[cfg(feature = "test-support")]
    pub fn relation_names_for_test() -> &'static [&'static str] {
        &RELATIONS
    }

    /// Bind independently provisioned OIDs in alphabetical relation-name
    /// order: `dnd_projection`, `dnd_projection_source_version`,
    /// `notification_settings_projection`,
    /// `notification_settings_projection_source_version`,
    /// `pubsub_affiliations`, `pubsub_items`, `pubsub_nodes`,
    /// `pubsub_owner_configured`, `pubsub_push_publications`,
    /// `pubsub_schema_marker`, `pubsub_schema_version`, and
    /// `pubsub_subscriptions`.
    pub fn new(
        pool: AttestedPgPool,
        relation_oids: [u32; 12],
    ) -> Result<Self, PinnedPubSubInventoryError> {
        let AttestationMode::Pinned(authority) = &pool.writer_capability.mode else {
            return Err(PinnedPubSubInventoryError::InvalidPins);
        };
        let mut relations = vec![
            authority.attestor.lineage_relation_oid,
            authority.attestor.lineage_primary_index_oid,
            authority.ledger.ledger_relation_oid,
            authority.ledger.ledger_primary_index_oid,
            authority.epoch_relation_oid,
            authority.epoch_primary_index_oid,
            authority.guard_manifest_relation_oid,
        ];
        relations.extend_from_slice(&relation_oids);
        if relations.contains(&0)
            || relations
                .iter()
                .enumerate()
                .any(|(index, oid)| relations[..index].contains(oid))
            || pool.max_connections() == 0
        {
            return Err(PinnedPubSubInventoryError::InvalidPins);
        }
        let admission = Arc::new(Semaphore::new(pool.max_connections() as usize));
        Ok(Self {
            pool,
            relation_oids,
            admission,
        })
    }

    pub(crate) fn start(&self) -> Result<PinnedPubSubInventoryJob, PinnedPubSubInventoryError> {
        self.start_inner(TestHooks::default(), ENTRY_DEADLINE)
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn start_paused_for_test(
        &self,
        ready: oneshot::Sender<(i32, bool, bool)>,
        release: oneshot::Receiver<()>,
    ) -> Result<PinnedPubSubInventoryJob, PinnedPubSubInventoryError> {
        self.start_inner(
            TestHooks {
                after_inventory: Some(InventoryPause { ready, release }),
                ..TestHooks::default()
            },
            ENTRY_DEADLINE,
        )
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn start_fence_paused_for_test(
        &self,
        before_ready: oneshot::Sender<()>,
        before_release: oneshot::Receiver<()>,
        retry_ready: oneshot::Sender<()>,
        retry_release: oneshot::Receiver<()>,
    ) -> Result<PinnedPubSubInventoryJob, PinnedPubSubInventoryError> {
        self.start_inner(
            TestHooks {
                before_fence: Some(TestCheckpoint {
                    ready: before_ready,
                    release: before_release,
                }),
                after_retry_rollback: Some(TestCheckpoint {
                    ready: retry_ready,
                    release: retry_release,
                }),
                ..TestHooks::default()
            },
            ENTRY_DEADLINE,
        )
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn start_rollback_stall_for_test(
        &self,
        timeout: Duration,
        ready: oneshot::Sender<i32>,
    ) -> Result<PinnedPubSubInventoryJob, PinnedPubSubInventoryError> {
        self.start_inner(
            TestHooks {
                stall_after_inventory: Some(ready),
                ..TestHooks::default()
            },
            timeout,
        )
    }

    fn start_inner(
        &self,
        hooks: TestHooks,
        timeout: Duration,
    ) -> Result<PinnedPubSubInventoryJob, PinnedPubSubInventoryError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| PinnedPubSubInventoryError::RuntimeUnavailable)?;
        let permit = Arc::clone(&self.admission)
            .try_acquire_owned()
            .map_err(|_| PinnedPubSubInventoryError::Overloaded)?;
        let AttestationMode::Pinned(authority) = &self.pool.writer_capability.mode else {
            return Err(PinnedPubSubInventoryError::InvalidPins);
        };
        let authority = (**authority).clone();
        let relation_oids = self.relation_oids;
        let database =
            Database::from_postgres_pool("pinned PubSub inventory", self.pool.pool.clone());
        let (sender, result) = oneshot::channel();
        drop(runtime.spawn(async move {
            let outcome = inspect(&database, &authority, relation_oids, hooks, timeout).await;
            drop(permit);
            let _ = sender.send(outcome);
        }));
        Ok(PinnedPubSubInventoryJob { result })
    }
}

enum Inspection {
    Complete,
    Retry,
}

async fn inspect(
    database: &Database,
    authority: &PinnedRuntimeAuthority,
    relation_oids: [u32; 12],
    mut hooks: TestHooks,
    timeout: Duration,
) -> Result<(), PinnedPubSubInventoryError> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut tx = tokio::time::timeout_at(deadline, database.begin())
            .await
            .map_err(|_| PinnedPubSubInventoryError::Timeout)??;
        if !tx.discard_postgres_on_drop() {
            return Err(PinnedPubSubInventoryError::InvalidPins);
        }
        let outcome = tokio::time::timeout_at(
            deadline,
            inspect_transaction(&mut tx, authority, relation_oids, &mut hooks),
        )
        .await
        .unwrap_or(Err(PinnedPubSubInventoryError::Timeout));

        // The checkout is marked for discard from acquisition through this
        // bounded acknowledgement. A failure or timeout closes its socket but
        // does not establish when the backend stopped or released its locks.
        if !tx.rollback_bounded_discard(ROLLBACK_TIMEOUT).await {
            return Err(PinnedPubSubInventoryError::RollbackUnconfirmed);
        }
        match outcome? {
            Inspection::Complete => return Ok(()),
            Inspection::Retry => {
                #[cfg(feature = "test-support")]
                tokio::time::timeout_at(deadline, checkpoint(&mut hooks.after_retry_rollback))
                    .await
                    .map_err(|_| PinnedPubSubInventoryError::Timeout)??;
                tokio::time::timeout_at(deadline, tokio::time::sleep(RETRY_INTERVAL))
                    .await
                    .map_err(|_| PinnedPubSubInventoryError::Timeout)?;
            }
        }
    }
}

async fn inspect_transaction(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
    relation_oids: [u32; 12],
    hooks: &mut TestHooks,
) -> Result<Inspection, PinnedPubSubInventoryError> {
    #[cfg(not(feature = "test-support"))]
    let _ = &hooks;
    // SET TRANSACTION takes no snapshot. The try-lock is the first
    // snapshot-bearing statement; a failed try is rolled back before retry.
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await?;
    #[cfg(feature = "test-support")]
    checkpoint(&mut hooks.before_fence).await?;
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedPubSubInventoryError::InvalidPins)?;
    let acquired: bool =
        sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(connection)
            .await?;
    if !acquired {
        return Ok(Inspection::Retry);
    }
    prepare_read_only_entry(
        tx,
        authority,
        DarkEpochAclStage::RelationGuardEpochZero,
        None,
    )
    .await?;
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedPubSubInventoryError::InvalidPins)?;
    let identity = authority.config.postgres();
    read_epoch_after_prelock(connection, &identity.schema.name.0, identity.schema.oid).await?;
    inspect_relations(tx, authority, relation_oids).await?;

    #[cfg(feature = "test-support")]
    if let Some(pause) = hooks.after_inventory.take() {
        let connection = tx
            .postgres_connection()
            .ok_or(PinnedPubSubInventoryError::InvalidPins)?;
        let observation: (i32, bool, bool) = sqlx::query_as(
            "SELECT pg_catalog.pg_backend_pid(), \
             pg_catalog.current_setting('transaction_read_only') = 'on', \
             pg_catalog.current_setting('transaction_isolation') = 'read committed'",
        )
        .fetch_one(connection)
        .await?;
        let _ = pause.ready.send(observation);
        pause
            .release
            .await
            .map_err(|_| PinnedPubSubInventoryError::PauseCancelled)?;
    }
    #[cfg(feature = "test-support")]
    if let Some(ready) = hooks.stall_after_inventory.take() {
        let connection = tx
            .postgres_connection()
            .ok_or(PinnedPubSubInventoryError::InvalidPins)?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await?;
        let _ = ready.send(pid);
        // Test-only: leave an in-flight server query when the worker's short
        // deadline expires, so its subsequent rollback cannot be acknowledged.
        sqlx::query("SELECT pg_catalog.pg_sleep(15)")
            .execute(connection)
            .await?;
    }
    Ok(Inspection::Complete)
}

#[cfg(feature = "test-support")]
async fn checkpoint(point: &mut Option<TestCheckpoint>) -> Result<(), PinnedPubSubInventoryError> {
    if let Some(point) = point.take() {
        let _ = point.ready.send(());
        point
            .release
            .await
            .map_err(|_| PinnedPubSubInventoryError::PauseCancelled)?;
    }
    Ok(())
}

async fn inspect_relations(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
    relation_oids: [u32; 12],
) -> Result<(), PinnedPubSubInventoryError> {
    for (name, oid) in RELATIONS.into_iter().zip(relation_oids) {
        verify_relation(tx, authority, name, oid).await?;
    }
    let schema = quote_identifier(&authority.config.postgres().schema.name.0);
    for (name, oid) in RELATIONS.into_iter().zip(relation_oids) {
        let connection = tx
            .postgres_connection()
            .ok_or(PinnedPubSubInventoryError::InvalidPins)?;
        // Both names are constrained: the schema is the pinned validated
        // identifier, and the relation name comes from the fixed list above.
        sqlx::query(&format!(
            "LOCK TABLE ONLY {schema}.{name} IN ACCESS SHARE MODE"
        ))
        .execute(connection)
        .await?;
        // A name-based lock can wait through replacement. READ COMMITTED gives
        // this statement a fresh snapshot while the acquired lock remains held.
        verify_relation(tx, authority, name, oid).await?;
    }
    Ok(())
}

async fn verify_relation(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
    name: &'static str,
    oid: u32,
) -> Result<(), PinnedPubSubInventoryError> {
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedPubSubInventoryError::InvalidPins)?;
    let identity = authority.config.postgres();
    // Only raw catalog fields and the inheritance relation are read here.
    // This deliberately does not expand ACLs, deparse expressions, call
    // privilege helpers, read store data, or invoke any store function.
    let exact: bool = sqlx::query_scalar(
        "SELECT c.relnamespace=$2 AND c.relowner=$3 AND n.nspowner=$3 \
         AND n.nspname=$4 AND c.relname=$5 AND c.relkind='r' \
         AND c.relpersistence='p' AND c.relam=2::pg_catalog.oid \
         AND NOT c.relispartition AND c.reloftype=0::pg_catalog.oid \
         AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i \
                         WHERE i.inhrelid=c.oid OR i.inhparent=c.oid) \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
         WHERE c.oid=$1",
    )
    .bind(Oid(oid))
    .bind(Oid(identity.schema.oid))
    .bind(Oid(authority.attestor.schema_owner_role_oid))
    .bind(&identity.schema.name.0)
    .bind(name)
    .fetch_optional(connection)
    .await?
    .unwrap_or(false);
    if !exact {
        return Err(PinnedPubSubInventoryError::CatalogDrift { relation: name });
    }
    Ok(())
}
