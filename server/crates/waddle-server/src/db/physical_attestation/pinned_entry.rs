//! Fixed, dark observation of a pinned read-only transaction entry.
//!
//! This is not a serving capability. In particular, the epoch heap prelock
//! does not fence ordinary row updates or prove the epoch value, index, guard
//! manifest, or owner/ACL transition protocol.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use thiserror::Error;

use super::preflight::{attest_dark_catalog_pinned_db_transaction, DarkPhysicalAttestationError};
use super::{quote_identifier, AttestationMode, AttestedPgPool, PhysicalAttestationError};
use crate::db::migrations::{
    acquire_dark_shared_migration_ledger_v1030_read, DarkLedgerPreflightError,
};
use crate::db::read_only_timeout::{bound_postgres_waits, timeout_millis};
use crate::db::{Database, DatabaseError, Transaction};

const ENTRY_DEADLINE: Duration = Duration::from_secs(25);

#[derive(Debug, Error)]
pub enum DarkPinnedEntryError {
    #[error("dark pinned entry requires a pinned pool")]
    NotPinned,
    #[error("dark pinned entry timed out")]
    Timeout,
    #[error("dark pinned entry could not verify a transaction timeout")]
    InvalidTimeout,
    #[error("dark pinned entry epoch relation differs from its independent pin")]
    EpochRelationDrift,
    #[error("dark pinned entry rollback was not acknowledged")]
    RollbackUnconfirmed,
    #[error(transparent)]
    Physical(#[from] DarkPhysicalAttestationError),
    #[error(transparent)]
    Attestation(#[from] PhysicalAttestationError),
    #[error(transparent)]
    Ledger(#[from] DarkLedgerPreflightError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("dark pinned entry PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

impl AttestedPgPool {
    /// Observe one exact pool-backed transaction through pinned physical,
    /// complete V1030 ledger, and epoch-heap prelock checks. The transaction
    /// is rolled back before success returns; no handle or authority escapes.
    /// This requires a direct session-preserving PG17 primary and independent
    /// quiescence of privileged catalog, role, ACL, and uncooperative writes.
    pub async fn inspect_dark_pinned_read_only_entry(&self) -> Result<(), DarkPinnedEntryError> {
        tokio::time::timeout(ENTRY_DEADLINE, self.inspect_entry(None))
            .await
            .map_err(|_| DarkPinnedEntryError::Timeout)?
    }

    /// Pause a fixed test observation after all three relation barriers are
    /// held. This exposes only the backend PID for independent lock inspection.
    #[cfg(feature = "test-support")]
    pub async fn inspect_dark_pinned_read_only_entry_paused_for_test(
        &self,
        ready: tokio::sync::oneshot::Sender<i32>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(), DarkPinnedEntryError> {
        tokio::time::timeout(ENTRY_DEADLINE, self.inspect_entry(Some((ready, release))))
            .await
            .map_err(|_| DarkPinnedEntryError::Timeout)?
    }

    async fn inspect_entry(
        &self,
        pause: Option<(
            tokio::sync::oneshot::Sender<i32>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    ) -> Result<(), DarkPinnedEntryError> {
        let AttestationMode::Pinned(authority) = &self.writer_capability.mode else {
            return Err(DarkPinnedEntryError::NotPinned);
        };
        // The private wrapper owns both this exact SQLx pool and its immutable
        // operator authority. A temporary adapter preserves PgLease rollback
        // and admission behavior without permitting a pinned Database to
        // escape or weakening the ordinary into_parts refusal.
        let database =
            Database::from_postgres_pool("dark pinned read-only entry", self.pool.clone());
        let mut tx = database.begin().await?;
        let observed = prepare_entry(&mut tx, authority, pause).await;
        tx.rollback()
            .await
            .map_err(|_| DarkPinnedEntryError::RollbackUnconfirmed)?;
        observed
    }
}

async fn prepare_entry(
    tx: &mut Transaction<'_>,
    authority: &super::PinnedRuntimeAuthority,
    pause: Option<(
        tokio::sync::oneshot::Sender<i32>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
) -> Result<(), DarkPinnedEntryError> {
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await?;
    bound_postgres_waits(tx)
        .await
        .map_err(|_| DarkPinnedEntryError::InvalidTimeout)?;
    bound_remaining_timeouts(tx).await?;
    let schema = &authority.config.postgres().schema.name.0;
    let restore_path = format!("{}, pg_temp", quote_identifier(schema));
    attest_dark_catalog_pinned_db_transaction(
        tx,
        &authority.config,
        authority.attestor,
        &restore_path,
    )
    .await?;
    {
        let mut ledger = acquire_dark_shared_migration_ledger_v1030_read(
            tx,
            schema,
            authority.ledger,
            &restore_path,
        )
        .await?;
        prelock_pinned_epoch(ledger.transaction(), authority).await?;
    }
    if let Some((ready, release)) = pause {
        let connection = tx
            .postgres_connection()
            .ok_or(PhysicalAttestationError::PostgresRequired)?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(connection)
            .await?;
        let _ = ready.send(pid);
        release.await.map_err(|_| DarkPinnedEntryError::Timeout)?;
    }
    Ok(())
}

async fn bound_remaining_timeouts(tx: &mut Transaction<'_>) -> Result<(), DarkPinnedEntryError> {
    for (name, cap) in [
        ("idle_in_transaction_session_timeout", 10_000_u64),
        ("transaction_timeout", 20_000_u64),
    ] {
        let connection = tx
            .postgres_connection()
            .ok_or(PhysicalAttestationError::PostgresRequired)?;
        let value: String = sqlx::query_scalar("SELECT pg_catalog.current_setting($1)")
            .bind(name)
            .fetch_one(&mut *connection)
            .await?;
        let millis = timeout_millis(&value).ok_or(DarkPinnedEntryError::InvalidTimeout)?;
        if millis == 0 || millis > cap {
            sqlx::query("SELECT pg_catalog.set_config($1, $2, true)")
                .bind(name)
                .bind(format!("{cap}ms"))
                .execute(&mut *connection)
                .await?;
        }
    }
    Ok(())
}

async fn prelock_pinned_epoch(
    tx: &mut Transaction<'_>,
    authority: &super::PinnedRuntimeAuthority,
) -> Result<(), DarkPinnedEntryError> {
    verify_pinned_epoch_heap(tx, authority).await?;
    let qualified = format!(
        "{}.ingress_protocol_epoch",
        quote_identifier(&authority.config.postgres().schema.name.0)
    );
    let connection = tx
        .postgres_connection()
        .ok_or(PhysicalAttestationError::PostgresRequired)?;
    sqlx::query(&format!("LOCK TABLE ONLY {qualified} IN ACCESS SHARE MODE"))
        .execute(connection)
        .await?;
    // A name-based lock can wait through replacement. The fresh READ
    // COMMITTED catalog statement must still identify the independent OID.
    verify_pinned_epoch_heap(tx, authority).await
}

async fn verify_pinned_epoch_heap(
    tx: &mut Transaction<'_>,
    authority: &super::PinnedRuntimeAuthority,
) -> Result<(), DarkPinnedEntryError> {
    let connection = tx
        .postgres_connection()
        .ok_or(PhysicalAttestationError::PostgresRequired)?;
    let valid: bool = sqlx::query_scalar(
        "SELECT c.relnamespace = $2 AND n.nspname = $3 \
         AND c.relname = 'ingress_protocol_epoch' \
         AND c.relkind = 'r' AND c.relpersistence = 'p' \
         AND am.amname = 'heap' \
         AND c.relowner = $4 \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_catalog.pg_am am ON am.oid = c.relam \
         WHERE c.oid = $1",
    )
    .bind(Oid(authority.epoch_relation_oid))
    .bind(Oid(authority.config.postgres().schema.oid))
    .bind(&authority.config.postgres().schema.name.0)
    .bind(Oid(authority.attestor.schema_owner_role_oid))
    .fetch_optional(connection)
    .await?
    .unwrap_or(false);
    if !valid {
        return Err(DarkPinnedEntryError::EpochRelationDrift);
    }
    Ok(())
}
