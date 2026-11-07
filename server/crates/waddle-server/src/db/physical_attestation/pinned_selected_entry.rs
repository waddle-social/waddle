//! Closed read-only selected-transaction candidate guarded against cooperative
//! catalog mutation. P21 receipt writes need independently provisioned
//! `ReceiptOperationPins`, the A-owner V1030 receipt profile, and in-transaction
//! exact journal/function verification; the existing `ReceiptOperationAuthority`
//! starts its own transaction and cannot be composed here. This candidate is
//! read-only and is not selected by server startup. The lock order is catalog
//! fence, attestor lineage barrier, migration-ledger barrier, ledger relation,
//! epoch relation, then guard-manifest relation. Cooperative mutators must take
//! the exclusive catalog fence before any of those subordinate locks.

use std::time::Duration;

use sqlx::postgres::PgConnection;
use sqlx::{Connection, Postgres, Transaction};
use thiserror::Error;
use waddle_xmpp::ingress::ProtocolEpoch;

use super::pinned_catalog::verify_pinned_epoch_manifest_catalog_sqlx;
use super::preflight::{attest_dark_catalog_pinned, DarkPhysicalAttestationError};
use super::DarkIngressAuthorityError;
use super::{
    read_epoch_after_prelock, AttestationMode, AttestedPgPool, PhysicalAttestationError,
    PinnedCatalogError, PinnedRuntimeAuthority, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use crate::db::migrations::{
    verify_dark_shared_migration_ledger_v1030_read_sqlx, DarkLedgerPreflightError,
};

const ENTRY_DEADLINE: Duration = Duration::from_secs(20);
const RETRY_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, Error)]
pub enum PinnedSelectedEntryError {
    #[error("selected transaction entry requires the pinned PostgreSQL pool")]
    NotPinned,
    #[error("catalog mutation guard remained contended through the bounded retry period")]
    CatalogContended,
    #[error("selected transaction entry timed out")]
    Timeout,
    #[error("selected transaction rollback was not acknowledged")]
    RollbackUnconfirmed,
    #[error("selected transaction test pause was cancelled")]
    PauseCancelled,
    #[error(transparent)]
    Attestation(#[from] DarkPhysicalAttestationError),
    #[error(transparent)]
    Ledger(#[from] DarkLedgerPreflightError),
    #[error(transparent)]
    Catalog(#[from] PinnedCatalogError),
    #[error(transparent)]
    Epoch(#[from] DarkIngressAuthorityError),
    #[error(transparent)]
    Physical(#[from] PhysicalAttestationError),
    #[error("selected transaction PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// A closed API over one exact pinned pool. It exposes only a typed, pinned
/// protocol-epoch observation; no SQL, connection, transaction, callback, or
/// pool accessor escapes. It does not claim all-store authority or select
/// production serving.
pub struct PinnedSelectedEntry {
    pool: AttestedPgPool,
}

impl PinnedSelectedEntry {
    pub fn new(pool: AttestedPgPool) -> Result<Self, PinnedSelectedEntryError> {
        if !matches!(&pool.writer_capability.mode, AttestationMode::Pinned(_)) {
            return Err(PinnedSelectedEntryError::NotPinned);
        }
        Ok(Self { pool })
    }

    /// Observe the pinned protocol epoch on a READ COMMITTED transaction that
    /// acquired the shared catalog fence as its first statement. Contention
    /// rolls back before backoff and starts a fresh transaction, so no stale
    /// transaction snapshot waits on the fence. Every successful observation
    /// ends with an acknowledged rollback.
    pub async fn observe_protocol_epoch(&self) -> Result<ProtocolEpoch, PinnedSelectedEntryError> {
        self.observe_protocol_epoch_inner(None).await
    }

    #[cfg(feature = "test-support")]
    pub async fn observe_protocol_epoch_paused_for_test(
        &self,
        ready: tokio::sync::oneshot::Sender<i32>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<ProtocolEpoch, PinnedSelectedEntryError> {
        self.observe_protocol_epoch_inner(Some((ready, release)))
            .await
    }

    async fn observe_protocol_epoch_inner(
        &self,
        pause: Option<(
            tokio::sync::oneshot::Sender<i32>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    ) -> Result<ProtocolEpoch, PinnedSelectedEntryError> {
        let AttestationMode::Pinned(authority) = &self.pool.writer_capability.mode else {
            return Err(PinnedSelectedEntryError::NotPinned);
        };
        let deadline = tokio::time::Instant::now() + ENTRY_DEADLINE;
        let mut connection = tokio::time::timeout(ENTRY_DEADLINE, self.pool.acquire())
            .await
            .map_err(|_| PinnedSelectedEntryError::Timeout)??;

        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(PinnedSelectedEntryError::CatalogContended);
            }
            let mut tx = tokio::time::timeout_at(
                deadline,
                connection.begin_with("BEGIN ISOLATION LEVEL READ COMMITTED READ ONLY"),
            )
            .await
            .map_err(|_| PinnedSelectedEntryError::Timeout)??;

            // Keep this as the first SQL statement after BEGIN. A false result
            // is not waited on inside the transaction: rollback releases the
            // snapshot before the next bounded try.
            let acquired = sqlx::query_scalar::<_, bool>(
                "SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)",
            )
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&mut *tx)
            .await;
            match acquired {
                Ok(true) => {}
                Ok(false) => {
                    if rollback_transaction(tx).await.is_err() {
                        let _ = connection.close().await;
                        return Err(PinnedSelectedEntryError::RollbackUnconfirmed);
                    }
                    tokio::time::sleep(
                        RETRY_INTERVAL
                            .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
                    )
                    .await;
                    continue;
                }
                Err(error) => {
                    if rollback_transaction(tx).await.is_err() {
                        let _ = connection.close().await;
                        return Err(PinnedSelectedEntryError::RollbackUnconfirmed);
                    }
                    return Err(PinnedSelectedEntryError::Postgres(error));
                }
            }

            let epoch = match prepare_selected_transaction(&mut tx, authority).await {
                Ok(epoch) => epoch,
                Err(error) => {
                    if rollback_transaction(tx).await.is_err() {
                        let _ = connection.close().await;
                        return Err(PinnedSelectedEntryError::RollbackUnconfirmed);
                    }
                    return Err(error);
                }
            };

            if let Some((ready, release)) = pause {
                let backend_pid: i32 =
                    match sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
                        .fetch_one(&mut *tx)
                        .await
                    {
                        Ok(pid) => pid,
                        Err(error) => {
                            if rollback_transaction(tx).await.is_err() {
                                let _ = connection.close().await;
                                return Err(PinnedSelectedEntryError::RollbackUnconfirmed);
                            }
                            return Err(PinnedSelectedEntryError::Postgres(error));
                        }
                    };
                if ready.send(backend_pid).is_err() || release.await.is_err() {
                    if rollback_transaction(tx).await.is_err() {
                        let _ = connection.close().await;
                        return Err(PinnedSelectedEntryError::RollbackUnconfirmed);
                    }
                    return Err(PinnedSelectedEntryError::PauseCancelled);
                }
            }

            if rollback_transaction(tx).await.is_err() {
                let _ = connection.close().await;
                return Err(PinnedSelectedEntryError::RollbackUnconfirmed);
            }
            return Ok(epoch);
        }
    }
}

async fn rollback_transaction(
    tx: Transaction<'_, Postgres>,
) -> Result<(), PinnedSelectedEntryError> {
    match tokio::time::timeout(Duration::from_secs(5), tx.rollback()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(PinnedSelectedEntryError::RollbackUnconfirmed),
    }
}

async fn prepare_selected_transaction(
    tx: &mut Transaction<'_, Postgres>,
    authority: &PinnedRuntimeAuthority,
) -> Result<ProtocolEpoch, PinnedSelectedEntryError> {
    for statement in [
        "SET LOCAL lock_timeout = '5s'",
        "SET LOCAL statement_timeout = '10s'",
        "SET LOCAL idle_in_transaction_session_timeout = '10s'",
        "SET LOCAL transaction_timeout = '20s'",
    ] {
        sqlx::query(statement).execute(&mut **tx).await?;
    }
    let schema_name = &authority.config.postgres().schema.name.0;
    let restore_path = format!("{}, pg_temp", super::quote_identifier(schema_name));
    attest_dark_catalog_pinned(tx, &authority.config, authority.attestor, &restore_path).await?;
    verify_dark_shared_migration_ledger_v1030_read_sqlx(
        tx,
        schema_name,
        authority.ledger,
        &restore_path,
    )
    .await?;
    verify_pinned_epoch_manifest_catalog_sqlx(tx, authority).await?;
    let connection: &mut PgConnection = &mut **tx;
    let epoch = read_epoch_after_prelock(
        connection,
        schema_name,
        authority.config.postgres().schema.oid,
    )
    .await?;
    Ok(epoch)
}
