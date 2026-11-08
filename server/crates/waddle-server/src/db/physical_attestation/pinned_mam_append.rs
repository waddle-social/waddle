//! Conditional, test-only MAM append catalog and origin observation.
//!
//! Pool hooks run before this transaction. The operator must independently
//! quiesce old writers and privileged catalog, ACL, role, collation and index
//! mutators before those hooks and through acknowledged rollback. Matching
//! collation versions do not prove an index was rebuilt after a prior change.
//! Full pg_depend and extension ownership are not checked; system dependencies
//! are independently trusted. This returns neither an append capability nor
//! a reusable catalog proof.

use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::types::Oid;
use thiserror::Error;
use tokio::sync::{oneshot, Semaphore};
use tokio::time::Instant;
use waddle_xmpp::mam::storage::{BoundedMamOriginError, BoundedMamOriginProbe};
use waddle_xmpp_core::xep0359::StanzaId;

use super::pinned_entry::{prepare_read_only_entry, DarkPinnedEntryError, ENTRY_DEADLINE};
use super::pinned_mam::{verify_mam_append_messages_catalog, PinnedMamLookupPins};
use super::{
    quote_identifier, read_epoch_after_prelock, AttestationMode, AttestedPgPool,
    DarkIngressAuthorityError, PinnedRuntimeAuthority, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use crate::db::{Database, DatabaseError, Transaction};

const RETRY_INTERVAL: Duration = Duration::from_millis(20);
const ROLLBACK_TIMEOUT: Duration = Duration::from_secs(5);
const TABLES: [&str; 2] = ["mam_messages", "mam_archive_sequences"];

/// Operator-supplied object identities. Indexes are the nine message indexes
/// in PinnedMamLookupPins order, followed by mam_archive_sequences_pkey.
#[derive(Clone, Copy, Debug)]
pub struct PinnedMamAppendPins {
    pub messages_relation_oid: u32,
    pub sequences_relation_oid: u32,
    pub index_oids: [u32; 10],
}

#[derive(Debug, Error)]
pub enum PinnedMamAppendError {
    #[error("MAM diagnostic requires a pinned pool and distinct object pins")]
    InvalidPins,
    #[error("MAM diagnostic admission is full")]
    Overloaded,
    #[error("MAM diagnostic requires a Tokio runtime")]
    RuntimeUnavailable,
    #[error("MAM diagnostic worker was lost")]
    WorkerLost,
    #[error("MAM diagnostic timed out")]
    Timeout,
    #[error("MAM diagnostic rollback was not acknowledged")]
    RollbackUnconfirmed,
    #[error("MAM append catalog or direct grant profile differs")]
    CatalogDrift,
    #[error("database-default collation identity or version is unsafe")]
    UnsafeCollation,
    #[error(transparent)]
    Probe(#[from] BoundedMamOriginError),
    #[error(transparent)]
    Entry(#[from] DarkPinnedEntryError),
    #[error(transparent)]
    Epoch(#[from] DarkIngressAuthorityError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("MAM diagnostic PostgreSQL operation failed")]
    Postgres(#[source] sqlx::Error),
}

pub struct PinnedMamAppendDiagnostic {
    pool: AttestedPgPool,
    pins: PinnedMamAppendPins,
    admission: Arc<Semaphore>,
}

pub struct PinnedMamAppendJob {
    result: oneshot::Receiver<Result<Option<StanzaId>, PinnedMamAppendError>>,
}

impl PinnedMamAppendJob {
    pub async fn finish(self) -> Result<Option<StanzaId>, PinnedMamAppendError> {
        self.result
            .await
            .map_err(|_| PinnedMamAppendError::WorkerLost)?
    }
}

type Pause = (oneshot::Sender<i32>, oneshot::Receiver<()>);

impl PinnedMamAppendDiagnostic {
    pub fn new(
        pool: AttestedPgPool,
        pins: PinnedMamAppendPins,
    ) -> Result<Self, PinnedMamAppendError> {
        let AttestationMode::Pinned(authority) = &pool.writer_capability.mode else {
            return Err(PinnedMamAppendError::InvalidPins);
        };
        let mut oids = vec![
            authority.attestor.lineage_relation_oid,
            authority.attestor.lineage_primary_index_oid,
            authority.ledger.ledger_relation_oid,
            authority.ledger.ledger_primary_index_oid,
            authority.epoch_relation_oid,
            authority.epoch_primary_index_oid,
            authority.guard_manifest_relation_oid,
            pins.messages_relation_oid,
            pins.sequences_relation_oid,
        ];
        oids.extend_from_slice(&pins.index_oids);
        if oids.contains(&0)
            || oids
                .iter()
                .enumerate()
                .any(|(i, oid)| oids[..i].contains(oid))
            || pool.max_connections() == 0
        {
            return Err(PinnedMamAppendError::InvalidPins);
        }
        let admission = Arc::new(Semaphore::new(pool.max_connections() as usize));
        Ok(Self {
            pool,
            pins,
            admission,
        })
    }

    /// Transfer the already-bounded request to a supervised worker before the
    /// caller's first wait. Dropping the receiver does not cancel that worker.
    pub fn start_probe(
        &self,
        request: BoundedMamOriginProbe,
    ) -> Result<PinnedMamAppendJob, PinnedMamAppendError> {
        self.start(request, None, ENTRY_DEADLINE)
    }

    pub fn start_probe_paused_for_test(
        &self,
        request: BoundedMamOriginProbe,
        ready: oneshot::Sender<i32>,
        release: oneshot::Receiver<()>,
    ) -> Result<PinnedMamAppendJob, PinnedMamAppendError> {
        self.start(request, Some((ready, release)), ENTRY_DEADLINE)
    }

    fn start(
        &self,
        request: BoundedMamOriginProbe,
        pause: Option<Pause>,
        timeout: Duration,
    ) -> Result<PinnedMamAppendJob, PinnedMamAppendError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| PinnedMamAppendError::RuntimeUnavailable)?;
        let permit = Arc::clone(&self.admission)
            .try_acquire_owned()
            .map_err(|_| PinnedMamAppendError::Overloaded)?;
        let AttestationMode::Pinned(authority) = &self.pool.writer_capability.mode else {
            return Err(PinnedMamAppendError::InvalidPins);
        };
        let authority = (**authority).clone();
        let pins = self.pins;
        let database =
            Database::from_postgres_pool("pinned MAM observation", self.pool.pool.clone());
        let (sender, result) = oneshot::channel();
        drop(runtime.spawn(async move {
            let _permit = permit;
            let outcome = observe(&database, &authority, pins, request, pause, timeout).await;
            let _ = sender.send(outcome);
        }));
        Ok(PinnedMamAppendJob { result })
    }
}

enum Inspection {
    Retry,
    Complete(Option<StanzaId>),
}

async fn observe(
    database: &Database,
    authority: &PinnedRuntimeAuthority,
    pins: PinnedMamAppendPins,
    request: BoundedMamOriginProbe,
    mut pause: Option<Pause>,
    timeout: Duration,
) -> Result<Option<StanzaId>, PinnedMamAppendError> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut tx = tokio::time::timeout_at(deadline, database.begin())
            .await
            .map_err(|_| PinnedMamAppendError::Timeout)??;
        if !tx.discard_postgres_on_drop() {
            return Err(PinnedMamAppendError::InvalidPins);
        }
        let outcome = tokio::time::timeout_at(
            deadline,
            inspect(&mut tx, authority, pins, &request, &mut pause),
        )
        .await
        .unwrap_or(Err(PinnedMamAppendError::Timeout));
        if !tx.rollback_bounded_discard(ROLLBACK_TIMEOUT).await {
            return Err(PinnedMamAppendError::RollbackUnconfirmed);
        }
        match outcome? {
            Inspection::Complete(value) => return Ok(value),
            Inspection::Retry => {
                tokio::time::timeout_at(deadline, tokio::time::sleep(RETRY_INTERVAL))
                    .await
                    .map_err(|_| PinnedMamAppendError::Timeout)?;
            }
        }
    }
}

async fn inspect(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
    pins: PinnedMamAppendPins,
    request: &BoundedMamOriginProbe,
    pause: &mut Option<Pause>,
) -> Result<Inspection, PinnedMamAppendError> {
    // SET TRANSACTION takes no snapshot. On contention, finish this
    // transaction before retrying from a fresh READ COMMITTED snapshot.
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await?;
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedMamAppendError::InvalidPins)?;
    let acquired: bool =
        sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(connection)
            .await
            .map_err(PinnedMamAppendError::Postgres)?;
    if !acquired {
        return Ok(Inspection::Retry);
    }
    prepare_read_only_entry(tx, authority, None).await?;
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedMamAppendError::InvalidPins)?;
    let identity = authority.config.postgres();
    read_epoch_after_prelock(connection, &identity.schema.name.0, identity.schema.oid).await?;
    verify_graph(tx, authority, pins).await?;
    let schema = quote_identifier(&identity.schema.name.0);
    for name in TABLES {
        let connection = tx
            .postgres_connection()
            .ok_or(PinnedMamAppendError::InvalidPins)?;
        sqlx::query(&format!(
            "LOCK TABLE ONLY {schema}.{name} IN ACCESS SHARE MODE"
        ))
        .execute(connection)
        .await
        .map_err(PinnedMamAppendError::Postgres)?;
    }
    // A name-based lock can wait through replacement. Recheck both inspected
    // profiles after the final relation wait while both locks remain held.
    verify_graph(tx, authority, pins).await?;
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedMamAppendError::InvalidPins)?;
    verify_collation(connection, identity.database.oid).await?;
    if let Some((ready, release)) = pause.take() {
        let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await
            .map_err(PinnedMamAppendError::Postgres)?;
        let _ = ready.send(pid);
        release.await.map_err(|_| PinnedMamAppendError::Timeout)?;
    }
    let result = request
        .observe_on_postgres_connection_in_schema(connection, &identity.schema.name.0)
        .await?;
    Ok(Inspection::Complete(result))
}

async fn verify_collation(
    connection: &mut sqlx::PgConnection,
    database_oid: u32,
) -> Result<(), PinnedMamAppendError> {
    // Evaluate the actual version once and compare bytewise: comparing version
    // strings using the unverified database-default collation would be circular.
    let safe: bool = sqlx::query_scalar(
        r#"WITH observed AS MATERIALIZED (
             SELECT d.datlocprovider, d.datcollversion,
                    pg_catalog.pg_database_collation_actual_version(d.oid) AS actual
             FROM pg_catalog.pg_database d
             WHERE d.oid=$1 AND d.datname=pg_catalog.current_database()
           )
           SELECT COALESCE((SELECT CASE WHEN datlocprovider IN ('b','c','i')
                 AND datcollversion IS NOT NULL AND actual IS NOT NULL
                 AND pg_catalog.octet_length(datcollversion) BETWEEN 1 AND 1024
                 AND pg_catalog.octet_length(actual) BETWEEN 1 AND 1024
               THEN pg_catalog.convert_to(datcollversion,'UTF8')
                    OPERATOR(pg_catalog.=) pg_catalog.convert_to(actual,'UTF8')
               ELSE false END
               FROM observed), false)
             AND EXISTS (SELECT 1 FROM pg_catalog.pg_collation c WHERE c.oid=100
               AND c.collnamespace=11 AND c.collname='default'
               AND c.collprovider='d' AND c.collisdeterministic)"#,
    )
    .bind(Oid(database_oid))
    .fetch_one(connection)
    .await
    .map_err(PinnedMamAppendError::Postgres)?;
    if !safe {
        return Err(PinnedMamAppendError::UnsafeCollation);
    }
    Ok(())
}

async fn verify_graph(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
    pins: PinnedMamAppendPins,
) -> Result<(), PinnedMamAppendError> {
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedMamAppendError::InvalidPins)?;
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(connection)
        .await
        .map_err(PinnedMamAppendError::Postgres)?;
    let mut message_indexes = [0; 9];
    message_indexes.copy_from_slice(&pins.index_oids[..9]);
    verify_mam_append_messages_catalog(
        tx,
        authority,
        PinnedMamLookupPins {
            messages_relation_oid: pins.messages_relation_oid,
            index_oids: message_indexes,
        },
    )
    .await
    .map_err(|_| PinnedMamAppendError::CatalogDrift)?;
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedMamAppendError::InvalidPins)?;
    let identity = authority.config.postgres();
    for query in [
        include_str!("pinned_mam_append_bounds.sql"),
        include_str!("pinned_mam_append_shape.sql"),
    ] {
        let exact: bool = sqlx::query_scalar(query)
            .bind(Oid(pins.messages_relation_oid))
            .bind(Oid(pins.sequences_relation_oid))
            .bind(Oid(identity.schema.oid))
            .bind(Oid(authority.attestor.schema_owner_role_oid))
            .bind(Oid(authority.attestor.runtime_role_oid))
            .bind(Oid(pins.index_oids[9]))
            .bind(&identity.schema.name.0)
            .fetch_one(&mut *connection)
            .await
            .map_err(PinnedMamAppendError::Postgres)?;
        if !exact {
            return Err(PinnedMamAppendError::CatalogDrift);
        }
    }
    Ok(())
}
