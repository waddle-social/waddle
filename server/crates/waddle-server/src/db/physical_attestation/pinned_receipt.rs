//! Closed pinned-pool receipt transactions. Selection remains test-only.
//!
//! Privileged catalog mutators and old backends must independently honor the
//! catalog fence or be quiescent. A caller must preserve the full operation
//! before the first await. A dropped future produces no outcome, and closing a
//! timed-out connection does not prove that its server backend has terminated.

use std::time::Duration;

use sqlx::{Connection, PgConnection, Row};
use thiserror::Error;
use uuid::Uuid;

use super::pinned_catalog::verify_pinned_epoch_manifest_catalog_sqlx;
use super::pinned_selected_entry::{rollback_transaction, DiscardOnDrop};
use super::preflight::{attest_dark_catalog_pinned, DarkPhysicalAttestationError};
use super::{
    quote_identifier, read_epoch_after_prelock, verify_guarded_receipt_profile, AttestationMode,
    AttestedPgPool, DarkIngressAuthorityError, GuardedReceiptOperationPins, PgRoleName,
    PinnedCatalogError, PinnedRuntimeAuthority, ReceiptOperation, ReceiptReadback,
    RestrictedIngressProfileError, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use crate::clustering::singleton_lease::{GuardBackend, LeaseGrant};
use crate::db::migrations::{
    verify_dark_shared_migration_ledger_v1030_write_sqlx, DarkLedgerPreflightError,
};
use crate::db::read_only_timeout::timeout_millis;

const DEADLINE: Duration = Duration::from_secs(20);
const RETRY_INTERVAL: Duration = Duration::from_millis(20);

/// A commit acknowledgement or an unknown result for one durable operation.
/// Backend details are observations on the caller's independently fenced
/// primary, not a durable custody mechanism or proof of server termination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinnedReceiptCommit {
    Committed {
        operation_id: Uuid,
        inserted: bool,
    },
    Indeterminate {
        operation_id: Uuid,
        backend_pid: Option<i32>,
        backend: Option<GuardBackend>,
    },
}

#[derive(Debug, Error)]
pub enum PinnedReceiptError {
    #[error("receipt pins do not match the independently pinned pool")]
    Binding,
    #[error("receipt operation ID must be nonzero")]
    InvalidOperation,
    #[error("receipt operation ID is bound to another tuple")]
    OperationConflict,
    #[error("receipt transaction completion is unconfirmed on backend {backend_pid:?}")]
    CompletionUnconfirmed { backend_pid: Option<i32> },
    #[error("receipt test pause was cancelled")]
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
    Profile(#[from] RestrictedIngressProfileError),
    #[error("pinned receipt PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// One exact pinned pool with a fixed receipt operation and exact-key readback.
/// No pool, transaction, raw SQL, or callback escapes. The supplied lease grant
/// identifies a contender; only the guarded SQL operation validates it under
/// the singleton slot lock. This authority does not select serving.
pub struct PinnedReceiptAuthority {
    pool: AttestedPgPool,
    api_owner: PgRoleName,
    pins: GuardedReceiptOperationPins,
}

/// Observation of the original attempt, even when it returned an error.
/// None means the checkout or observation did not supply that identity; it
/// does not mean that no backend exists or that a transaction rolled back.
#[derive(Debug)]
pub(crate) struct CustodiedReceiptAttempt {
    pub result: Result<PinnedReceiptCommit, PinnedReceiptError>,
    pub backend_pid: Option<i32>,
    pub backend: Option<GuardBackend>,
}

enum Action<'a> {
    Record(&'a LeaseGrant),
    Readback,
}

enum Outcome {
    Record(bool),
    Readback(ReceiptReadback),
    CommitUnknown,
}

type Pause = Option<(
    tokio::sync::oneshot::Sender<(GuardBackend, GuardBackend)>,
    tokio::sync::oneshot::Receiver<()>,
)>;

impl PinnedReceiptAuthority {
    pub fn new(
        pool: AttestedPgPool,
        api_owner: PgRoleName,
        pins: GuardedReceiptOperationPins,
    ) -> Result<Self, PinnedReceiptError> {
        let AttestationMode::Pinned(authority) = &pool.writer_capability.mode else {
            return Err(PinnedReceiptError::Binding);
        };
        if !pins_match(authority, &pins) {
            return Err(PinnedReceiptError::Binding);
        }
        Ok(Self {
            pool,
            api_owner,
            pins,
        })
    }

    /// An unknown result and a missing readback never prove rollback. The
    /// caller must keep the original primary/backend fence independently.
    pub async fn record(
        &self,
        grant: &LeaseGrant,
        operation: &ReceiptOperation,
    ) -> Result<PinnedReceiptCommit, PinnedReceiptError> {
        let mut backend_pid = None;
        let mut backend = None;
        self.record_with_deadline(
            grant,
            operation,
            DEADLINE,
            None,
            &mut backend_pid,
            &mut backend,
        )
        .await
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn matches_lineage(&self, lineage: &crate::db::lineage::AttestedLineage) -> bool {
        &self.pool.writer_capability.config.lineage == lineage
    }

    pub(crate) async fn record_for_custody(
        &self,
        grant: &LeaseGrant,
        operation: &ReceiptOperation,
        pause: Pause,
    ) -> CustodiedReceiptAttempt {
        let mut backend_pid = None;
        let mut backend = None;
        let result = self
            .record_with_deadline(
                grant,
                operation,
                DEADLINE,
                pause,
                &mut backend_pid,
                &mut backend,
            )
            .await;
        CustodiedReceiptAttempt {
            result,
            backend_pid,
            backend,
        }
    }

    #[cfg(feature = "test-support")]
    pub async fn record_paused_for_test(
        &self,
        grant: &LeaseGrant,
        operation: &ReceiptOperation,
        timeout: Duration,
        ready: tokio::sync::oneshot::Sender<(GuardBackend, GuardBackend)>,
        resume: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<PinnedReceiptCommit, PinnedReceiptError> {
        let mut backend_pid = None;
        let mut backend = None;
        self.record_with_deadline(
            grant,
            operation,
            timeout,
            Some((ready, resume)),
            &mut backend_pid,
            &mut backend,
        )
        .await
    }

    async fn record_with_deadline(
        &self,
        grant: &LeaseGrant,
        operation: &ReceiptOperation,
        timeout: Duration,
        pause: Pause,
        backend_pid: &mut Option<i32>,
        backend: &mut Option<GuardBackend>,
    ) -> Result<PinnedReceiptCommit, PinnedReceiptError> {
        validate(operation)?;
        if grant.generation() <= 0 {
            return Err(PinnedReceiptError::Binding);
        }
        let outcome = tokio::time::timeout(
            timeout,
            self.run(
                operation,
                Action::Record(grant),
                backend_pid,
                backend,
                pause,
            ),
        )
        .await;
        match outcome {
            Ok(Ok(Outcome::Record(inserted))) => Ok(PinnedReceiptCommit::Committed {
                operation_id: operation.operation_id,
                inserted,
            }),
            Ok(Ok(Outcome::CommitUnknown)) | Err(_) => Ok(PinnedReceiptCommit::Indeterminate {
                operation_id: operation.operation_id,
                backend_pid: *backend_pid,
                backend: *backend,
            }),
            Ok(Err(error)) => Err(error),
            Ok(Ok(Outcome::Readback(_))) => Err(PinnedReceiptError::Binding),
        }
    }

    /// Only an exact positive journal row proves a historical receipt commit;
    /// it does not prove current receipt state or an external effect.
    pub async fn readback(
        &self,
        operation: &ReceiptOperation,
    ) -> Result<ReceiptReadback, PinnedReceiptError> {
        self.readback_with_pause(operation, None).await
    }

    #[cfg(feature = "test-support")]
    pub async fn readback_paused_for_test(
        &self,
        operation: &ReceiptOperation,
        ready: tokio::sync::oneshot::Sender<(GuardBackend, GuardBackend)>,
        resume: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<ReceiptReadback, PinnedReceiptError> {
        self.readback_with_pause(operation, Some((ready, resume)))
            .await
    }

    async fn readback_with_pause(
        &self,
        operation: &ReceiptOperation,
        pause: Pause,
    ) -> Result<ReceiptReadback, PinnedReceiptError> {
        validate(operation)?;
        let mut backend_pid = None;
        let mut backend = None;
        match tokio::time::timeout(
            DEADLINE,
            self.run(
                operation,
                Action::Readback,
                &mut backend_pid,
                &mut backend,
                pause,
            ),
        )
        .await
        {
            Ok(Ok(Outcome::Readback(outcome))) => Ok(outcome),
            Ok(Err(error)) => Err(error),
            Err(_) => Ok(ReceiptReadback::Indeterminate),
            Ok(Ok(Outcome::Record(_) | Outcome::CommitUnknown)) => Err(PinnedReceiptError::Binding),
        }
    }

    async fn run(
        &self,
        operation: &ReceiptOperation,
        action: Action<'_>,
        backend_pid: &mut Option<i32>,
        backend: &mut Option<GuardBackend>,
        pause: Pause,
    ) -> Result<Outcome, PinnedReceiptError> {
        let AttestationMode::Pinned(authority) = &self.pool.writer_capability.mode else {
            return Err(PinnedReceiptError::Binding);
        };
        let mut connection = DiscardOnDrop::new(self.pool.acquire().await?);
        let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| PinnedReceiptError::CompletionUnconfirmed { backend_pid: None })?;
        *backend_pid = Some(pid);
        // Server-side transaction limits are installed before BEGIN. Checkout
        // itself remains a separate backend-reconciliation boundary.
        let bounds = SessionBounds::install(&mut connection).await?;

        loop {
            let mut tx = connection
                .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED READ WRITE")
                .await
                .map_err(|_| PinnedReceiptError::CompletionUnconfirmed {
                    backend_pid: Some(pid),
                })?;
            // First statement in the transaction. A failed try is rolled back
            // before backoff, so a snapshot never waits on the catalog fence.
            let acquired = sqlx::query_scalar::<_, bool>(
                "SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)",
            )
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&mut *tx)
            .await;
            match acquired {
                Ok(true) => {}
                Ok(false) => {
                    if !rollback_transaction(tx).await {
                        return Err(PinnedReceiptError::CompletionUnconfirmed {
                            backend_pid: Some(pid),
                        });
                    }
                    tokio::time::sleep(RETRY_INTERVAL).await;
                    continue;
                }
                Err(error) => {
                    if !rollback_transaction(tx).await {
                        return Err(PinnedReceiptError::CompletionUnconfirmed {
                            backend_pid: Some(pid),
                        });
                    }
                    bounds.keep_if_restored(&mut connection).await;
                    return Err(error.into());
                }
            }

            let schema = &authority.config.postgres().schema.name.0;
            let mut observed_pair = None;
            let result = async {
                prepare(&mut tx, authority).await?;
                let observed = observe_backend(&mut tx).await?;
                if observed.pid != pid
                    || observed.database_oid != authority.config.postgres().database.oid
                {
                    return Err(PinnedReceiptError::Binding);
                }
                *backend = Some(observed);
                verify_guarded_receipt_profile(
                    &mut tx,
                    schema,
                    authority.config.runtime_login.as_str(),
                    self.api_owner.as_str(),
                    &self.pins,
                )
                .await?;
                let result = match &action {
                    Action::Record(grant) => record(&mut tx, schema, grant, operation)
                        .await
                        .map(Outcome::Record),
                    Action::Readback => find(&mut tx, schema, operation)
                        .await
                        .map(Outcome::Readback),
                }?;
                if pause.is_some() {
                    observed_pair = Some((observed, observe_backend(&mut tx).await?));
                }
                Ok::<_, PinnedReceiptError>(result)
            }
            .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    if !rollback_transaction(tx).await {
                        return Err(PinnedReceiptError::CompletionUnconfirmed {
                            backend_pid: Some(pid),
                        });
                    }
                    bounds.keep_if_restored(&mut connection).await;
                    return Err(error);
                }
            };
            if let Some((ready, resume)) = pause {
                let pair = observed_pair.ok_or(PinnedReceiptError::Binding)?;
                if ready.send(pair).is_err() || resume.await.is_err() {
                    if !rollback_transaction(tx).await {
                        return Err(PinnedReceiptError::CompletionUnconfirmed {
                            backend_pid: Some(pid),
                        });
                    }
                    bounds.keep_if_restored(&mut connection).await;
                    return Err(PinnedReceiptError::PauseCancelled);
                }
            }
            match action {
                Action::Record(_) => match tx.commit().await {
                    Ok(()) => {
                        bounds.keep_if_restored(&mut connection).await;
                        return Ok(result);
                    }
                    Err(_) => return Ok(Outcome::CommitUnknown),
                },
                Action::Readback => {
                    if !rollback_transaction(tx).await {
                        return Err(PinnedReceiptError::CompletionUnconfirmed {
                            backend_pid: Some(pid),
                        });
                    }
                    bounds.keep_if_restored(&mut connection).await;
                    return Ok(result);
                }
            }
        }
    }
}

struct SessionBounds(Vec<(&'static str, String)>);

impl SessionBounds {
    async fn install(connection: &mut PgConnection) -> Result<Self, PinnedReceiptError> {
        let mut restore = Vec::new();
        for (name, maximum) in [
            ("lock_timeout", 5_000),
            ("statement_timeout", 10_000),
            ("idle_in_transaction_session_timeout", 10_000),
            ("transaction_timeout", 20_000),
        ] {
            let value: String = sqlx::query_scalar("SELECT pg_catalog.current_setting($1)")
                .bind(name)
                .fetch_one(&mut *connection)
                .await?;
            let millis = timeout_millis(&value).ok_or(PinnedReceiptError::Binding)?;
            if millis == 0 || millis > maximum {
                sqlx::query("SELECT pg_catalog.set_config($1,$2,false)")
                    .bind(name)
                    .bind(format!("{maximum}ms"))
                    .execute(&mut *connection)
                    .await?;
                restore.push((name, value));
            }
        }
        Ok(Self(restore))
    }

    async fn keep_if_restored(self, connection: &mut DiscardOnDrop) {
        for (name, value) in self.0 {
            if sqlx::query("SELECT pg_catalog.set_config($1,$2,false)")
                .bind(name)
                .bind(value)
                .execute(&mut **connection)
                .await
                .is_err()
            {
                return;
            }
        }
        connection.keep();
    }
}

async fn observe_backend(
    connection: &mut PgConnection,
) -> Result<GuardBackend, PinnedReceiptError> {
    let row = sqlx::query(
        "SELECT activity.pid, activity.backend_start, activity.xact_start, \
                activity.datid::bigint, pg_catalog.pg_postmaster_start_time(), \
                pg_catalog.pg_current_xact_id()::text \
         FROM pg_catalog.pg_stat_activity AS activity \
         WHERE activity.pid=pg_catalog.pg_backend_pid()",
    )
    .fetch_optional(connection)
    .await?
    .ok_or(PinnedReceiptError::Binding)?;
    let database_oid =
        u32::try_from(row.try_get::<i64, _>(3)?).map_err(|_| PinnedReceiptError::Binding)?;
    let transaction_id = row
        .try_get::<String, _>(5)?
        .parse()
        .map_err(|_| PinnedReceiptError::Binding)?;
    Ok(GuardBackend {
        pid: row.try_get(0)?,
        backend_start: row.try_get(1)?,
        transaction_start: row.try_get(2)?,
        postmaster_start: row.try_get(4)?,
        database_oid,
        transaction_id,
    })
}

fn validate(operation: &ReceiptOperation) -> Result<(), PinnedReceiptError> {
    if operation.operation_id.is_nil() {
        Err(PinnedReceiptError::InvalidOperation)
    } else {
        Ok(())
    }
}

fn distinct_nonzero(values: &[u32]) -> bool {
    values
        .iter()
        .enumerate()
        .all(|(index, oid)| *oid != 0 && !values[..index].contains(oid))
}

fn pins_match(authority: &PinnedRuntimeAuthority, pins: &GuardedReceiptOperationPins) -> bool {
    let receipt = &pins.receipt;
    receipt.schema_oid == authority.config.postgres().schema.oid
        && receipt.schema_owner_role_oid == authority.attestor.schema_owner_role_oid
        && receipt.runtime_role_oid == authority.attestor.runtime_role_oid
        && receipt.epoch_relation_oid == authority.epoch_relation_oid
        && ![
            authority.attestor.database_owner_role_oid,
            authority.attestor.schema_owner_role_oid,
            authority.attestor.runtime_role_oid,
            authority.attestor.installer_role_oid,
            authority.attestor.definer_role_oid,
        ]
        .contains(&receipt.api_owner_role_oid)
        && receipt.api_owner_role_oid != 0
        && distinct_nonzero(&[
            authority.attestor.lineage_relation_oid,
            authority.attestor.lineage_primary_index_oid,
            authority.ledger.ledger_relation_oid,
            authority.ledger.ledger_primary_index_oid,
            authority.epoch_relation_oid,
            authority.epoch_primary_index_oid,
            authority.guard_manifest_relation_oid,
            receipt.journal_relation_oid,
            pins.singleton_slot_relation_oid,
            pins.singleton_slot_primary_index_oid,
            pins.singleton_lease_relation_oid,
            pins.singleton_lease_primary_index_oid,
            pins.singleton_lease_generation_index_oid,
            pins.singleton_lease_identity_index_oid,
        ])
        && distinct_nonzero(&[
            authority.attestor.attestor_function_oid,
            receipt.prepare_function_oid,
            receipt.effect_receipt_function_oid,
            receipt.policy_discard_function_oid,
            receipt.journal_function_oid,
            receipt.journal_lookup_function_oid,
            pins.guarded_receipt_function_oid,
        ])
}

async fn prepare(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    authority: &PinnedRuntimeAuthority,
) -> Result<(), PinnedReceiptError> {
    let schema = &authority.config.postgres().schema.name.0;
    let path = format!("{}, pg_temp", quote_identifier(schema));
    attest_dark_catalog_pinned(tx, &authority.config, authority.attestor, &path).await?;
    verify_dark_shared_migration_ledger_v1030_write_sqlx(tx, schema, authority.ledger, &path)
        .await?;
    verify_pinned_epoch_manifest_catalog_sqlx(tx, authority).await?;
    read_epoch_after_prelock(tx, schema, authority.config.postgres().schema.oid).await?;
    Ok(())
}

async fn record(
    connection: &mut PgConnection,
    schema: &str,
    grant: &LeaseGrant,
    operation: &ReceiptOperation,
) -> Result<bool, PinnedReceiptError> {
    let sql = format!(
        "SELECT {}.waddle_p21_guarded_record_effect_receipt($1,$2,$3,$4,$5,$6,$7)",
        quote_identifier(schema)
    );
    sqlx::query_scalar(&sql)
        .bind(grant.identity().node_id())
        .bind(grant.identity().incarnation())
        .bind(grant.generation())
        .bind(operation.operation_id)
        .bind(operation.message.to_storage())
        .bind(operation.kind.to_storage())
        .bind(operation.semantic_identity_hash.as_slice())
        .fetch_one(connection)
        .await
        .map_err(|error| match &error {
            sqlx::Error::Database(database) if database.code().as_deref() == Some("23505") => {
                PinnedReceiptError::OperationConflict
            }
            _ => PinnedReceiptError::Postgres(error),
        })
}

async fn find(
    connection: &mut PgConnection,
    schema: &str,
    operation: &ReceiptOperation,
) -> Result<ReceiptReadback, PinnedReceiptError> {
    let sql = format!(
        "SELECT message_key, receipt_kind, semantic_identity_hash, inserted \
         FROM {}.waddle_p21_lookup_receipt_operation($1)",
        quote_identifier(schema)
    );
    let row = sqlx::query(&sql)
        .bind(operation.operation_id)
        .fetch_optional(connection)
        .await?;
    let Some(row) = row else {
        return Ok(ReceiptReadback::Indeterminate);
    };
    if row.try_get::<Uuid, _>(0)? != operation.message.to_storage()
        || row.try_get::<i32, _>(1)? != operation.kind.to_storage()
        || row.try_get::<Vec<u8>, _>(2)? != operation.semantic_identity_hash
    {
        return Err(PinnedReceiptError::OperationConflict);
    }
    Ok(ReceiptReadback::Committed {
        inserted: row.try_get(3)?,
    })
}
