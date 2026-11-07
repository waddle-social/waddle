//! Catalog-first acquisition for single-backend owner transactions.

use std::time::Duration;

use super::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
use super::{Database, DatabaseDriver, DatabaseError, Transaction};

const LOCK_WAIT: Duration = Duration::from_secs(15);
const LOCK_RETRY: Duration = Duration::from_millis(20);
const ROLLBACK_WAIT: Duration = Duration::from_secs(5);

/// A transaction-scoped advisory gate after the catalog fence.
/// Callers supply these in their established lock order.
#[derive(Clone, Copy)]
pub(crate) enum CatalogFenceLock {
    Exclusive(i64),
    ExclusivePair(i32, i32),
    Shared(i64),
    SharedPair(i32, i32),
}

#[derive(Clone, Copy)]
enum LockAttempt {
    Acquired,
    CatalogContended,
    GateContended,
}

#[derive(Clone, Copy)]
enum CatalogFenceMode {
    Exclusive,
    SharedReadOnly,
}

#[derive(Clone, Copy)]
enum IdleTransactionBound {
    Existing,
    Capped,
}

impl Database {
    /// Begin a fresh PostgreSQL owner transaction and acquire catalog-X before
    /// the supplied advisory gates. All checkout hooks finish before catalog-X;
    /// this transaction must remain on the returned connection through its
    /// actual end. Callers must not acquire another hooked checkout while it
    /// is held. This helper does not protect work performed on another session.
    ///
    /// A failed lower-gate try releases catalog-X with acknowledged rollback
    /// before retrying from a fresh transaction. Unacknowledged transaction
    /// ends discard the checkout; a lost COMMIT is still indeterminate and is
    /// never retried here.
    pub(crate) async fn begin_catalog_fenced(
        &self,
        locks: &[CatalogFenceLock],
    ) -> Result<Transaction<'_>, DatabaseError> {
        self.begin_catalog_fenced_with_mode(
            CatalogFenceMode::Exclusive,
            IdleTransactionBound::Existing,
            locks,
        )
        .await
    }

    /// As above, but cap idle transaction retention before acquiring catalog-X.
    pub(crate) async fn begin_catalog_fenced_with_idle_bound(
        &self,
        locks: &[CatalogFenceLock],
    ) -> Result<Transaction<'_>, DatabaseError> {
        self.begin_catalog_fenced_with_mode(
            CatalogFenceMode::Exclusive,
            IdleTransactionBound::Capped,
            locks,
        )
        .await
    }

    /// Begin a fresh READ COMMITTED, READ ONLY PostgreSQL transaction with
    /// catalog-S before the supplied advisory gates. It has the same checkout,
    /// rollback, and single-backend requirements as the exclusive form.
    pub(crate) async fn begin_catalog_fenced_read_only(
        &self,
        locks: &[CatalogFenceLock],
    ) -> Result<Transaction<'_>, DatabaseError> {
        self.begin_catalog_fenced_with_mode(
            CatalogFenceMode::SharedReadOnly,
            IdleTransactionBound::Existing,
            locks,
        )
        .await
    }

    /// As above, but cap idle transaction retention before acquiring catalog-S.
    pub(crate) async fn begin_catalog_fenced_read_only_with_idle_bound(
        &self,
        locks: &[CatalogFenceLock],
    ) -> Result<Transaction<'_>, DatabaseError> {
        self.begin_catalog_fenced_with_mode(
            CatalogFenceMode::SharedReadOnly,
            IdleTransactionBound::Capped,
            locks,
        )
        .await
    }

    async fn begin_catalog_fenced_with_mode(
        &self,
        mode: CatalogFenceMode,
        idle_bound: IdleTransactionBound,
        locks: &[CatalogFenceLock],
    ) -> Result<Transaction<'_>, DatabaseError> {
        if self.driver() != DatabaseDriver::Postgres {
            return Err(DatabaseError::QueryFailed(
                "catalog mutation fencing requires PostgreSQL".to_owned(),
            ));
        }

        let deadline = tokio::time::Instant::now() + LOCK_WAIT;
        loop {
            // The checkout is marked for discard before the cancellable BEGIN.
            let mut tx = tokio::time::timeout_at(deadline, self.begin_discard_postgres_on_drop())
                .await
                .map_err(|_| DatabaseError::CatalogMutationLockTimeout)??;
            let outcome = match tokio::time::timeout_at(
                deadline,
                try_locks(&mut tx, mode, idle_bound, locks),
            )
            .await
            {
                Ok(Ok(LockAttempt::Acquired)) if tokio::time::Instant::now() < deadline => {
                    return Ok(tx);
                }
                Ok(result) => result,
                Err(_) => Err(DatabaseError::CatalogMutationLockTimeout),
            };

            if !tx.rollback_bounded_discard(ROLLBACK_WAIT).await {
                return Err(DatabaseError::CatalogMutationRollbackUnconfirmed);
            }
            let contention = match outcome? {
                LockAttempt::CatalogContended => DatabaseError::CatalogMutationFenceContended,
                LockAttempt::GateContended => DatabaseError::CatalogMutationGateContended,
                LockAttempt::Acquired => return Err(DatabaseError::CatalogMutationLockTimeout),
            };
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(contention);
            }
            tokio::time::sleep(LOCK_RETRY.min(remaining)).await;
            if tokio::time::Instant::now() >= deadline {
                return Err(contention);
            }
        }
    }
}

async fn try_locks(
    tx: &mut Transaction<'_>,
    mode: CatalogFenceMode,
    idle_bound: IdleTransactionBound,
    locks: &[CatalogFenceLock],
) -> Result<LockAttempt, DatabaseError> {
    // Two utility statements both reject a transaction already used at either
    // isolation level, then leave every subsequent statement READ COMMITTED.
    // Neither statement obtains a data snapshot.
    let (initial_mode, final_mode, try_catalog) = match mode {
        CatalogFenceMode::Exclusive => (
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ WRITE",
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
            "SELECT pg_catalog.pg_try_advisory_xact_lock($1)",
        ),
        CatalogFenceMode::SharedReadOnly => (
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
            "SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)",
        ),
    };
    tx.execute(initial_mode, ()).await?;
    tx.execute(final_mode, ()).await?;
    if matches!(idle_bound, IdleTransactionBound::Capped) {
        super::read_only_timeout::bound_postgres_idle_transaction(tx)
            .await
            .map_err(|error| match error {
                super::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { setting } => {
                    DatabaseError::QueryFailed(format!("could not verify PostgreSQL {setting}"))
                }
                super::read_only_timeout::ReadOnlyTimeoutError::Database(error) => error,
            })?;
    }

    let connection = tx.postgres_connection().ok_or_else(|| {
        DatabaseError::QueryFailed("catalog mutation fencing requires PostgreSQL".to_owned())
    })?;
    let acquired: bool = sqlx::query_scalar(try_catalog)
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(&mut *connection)
        .await?;
    if !acquired {
        return Ok(LockAttempt::CatalogContended);
    }
    for lock in locks {
        if !try_catalog_fence_lock(tx, *lock).await? {
            return Ok(LockAttempt::GateContended);
        }
    }
    Ok(LockAttempt::Acquired)
}

/// Try a subordinate gate on an already catalog-fenced transaction. A caller
/// that sees contention must roll back before retrying the whole operation.
pub(crate) async fn try_catalog_fence_lock(
    tx: &mut Transaction<'_>,
    lock: CatalogFenceLock,
) -> Result<bool, DatabaseError> {
    let connection = tx.postgres_connection().ok_or_else(|| {
        DatabaseError::QueryFailed("catalog mutation fencing requires PostgreSQL".to_owned())
    })?;
    let acquired = match lock {
        CatalogFenceLock::Exclusive(key) => {
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                .bind(key)
                .fetch_one(&mut *connection)
                .await?
        }
        CatalogFenceLock::ExclusivePair(first, second) => {
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1, $2)")
                .bind(first)
                .bind(second)
                .fetch_one(&mut *connection)
                .await?
        }
        CatalogFenceLock::Shared(key) => {
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
                .bind(key)
                .fetch_one(&mut *connection)
                .await?
        }
        CatalogFenceLock::SharedPair(first, second) => {
            sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1, $2)")
                .bind(first)
                .bind(second)
                .fetch_one(&mut *connection)
                .await?
        }
    };
    Ok(acquired)
}
