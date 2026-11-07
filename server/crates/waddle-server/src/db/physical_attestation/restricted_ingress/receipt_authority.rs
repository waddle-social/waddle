//! Closed receipt transaction and positive-only commit reconciliation.
//!
//! This is not a startup selector. PostgreSQL requires separately provisioned
//! roles, the fixed ingress API, and the exact journal profile. Provider and
//! attestor mutation must be independently controlled as documented for v2.
//! The inherited v2 verifier also requires a trusted-clean catalog at entry;
//! it reads the guard manifest before proving its complete structural shape.
//! The SQLite backend has only cooperative application ownership.
//! PostgreSQL runtime access to historical results is through a separately
//! pinned exact-key definer lookup. Bounded retention and the joint role profile
//! remain required before any serving selector can use it.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::ingress::{MessageKey, ProtocolEpoch};

use super::receipt_catalog;
use super::{inspect_profile_with_receipt_journal, RestrictedIngressProfileError};
use super::{PgRoleName, RestrictedIngressProfileObservation};
use crate::db::lineage::LINEAGE_ADVISORY_LOCK_KEY;
use crate::db::physical_attestation::{
    attest_connection, quote_identifier, AttestedPostgresWriterCapability,
    CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use crate::db::read_only_timeout::timeout_millis;
use crate::db::{Database, DatabaseDriver, Transaction, MIGRATION_LEDGER_ADVISORY_LOCK_KEY};
use crate::ingress_substrate::{supported_protocol_epoch, EffectReceiptKind};

const DEADLINE: Duration = Duration::from_secs(15);

/// Independent provisioning identities, not values learned from an observer.
/// The separate physical attestor and provider mutation policy remain trusted
/// prerequisites; these pins do not attest their installation or quiescence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiptOperationPins {
    pub schema_oid: u32,
    pub schema_owner_role_oid: u32,
    pub runtime_role_oid: u32,
    pub api_owner_role_oid: u32,
    pub prepare_function_oid: u32,
    pub epoch_relation_oid: u32,
    pub journal_relation_oid: u32,
    pub effect_receipt_function_oid: u32,
    pub policy_discard_function_oid: u32,
    pub journal_function_oid: u32,
    pub journal_lookup_function_oid: u32,
}

impl ReceiptOperationPins {
    pub(super) fn valid(self) -> bool {
        [
            self.schema_oid,
            self.schema_owner_role_oid,
            self.runtime_role_oid,
            self.api_owner_role_oid,
            self.prepare_function_oid,
            self.epoch_relation_oid,
            self.journal_relation_oid,
            self.effect_receipt_function_oid,
            self.policy_discard_function_oid,
            self.journal_function_oid,
            self.journal_lookup_function_oid,
        ]
        .into_iter()
        .all(|oid| oid != 0)
            && self.schema_owner_role_oid != self.runtime_role_oid
            && self.schema_owner_role_oid != self.api_owner_role_oid
            && self.runtime_role_oid != self.api_owner_role_oid
            && self.prepare_function_oid != self.effect_receipt_function_oid
            && self.prepare_function_oid != self.journal_function_oid
            && self.prepare_function_oid != self.journal_lookup_function_oid
            && self.policy_discard_function_oid != self.prepare_function_oid
            && self.epoch_relation_oid != self.journal_relation_oid
            && self.effect_receipt_function_oid != self.policy_discard_function_oid
            && self.effect_receipt_function_oid != self.journal_function_oid
            && self.effect_receipt_function_oid != self.journal_lookup_function_oid
            && self.policy_discard_function_oid != self.journal_function_oid
            && self.policy_discard_function_oid != self.journal_lookup_function_oid
            && self.journal_function_oid != self.journal_lookup_function_oid
    }
}

/// The caller must durably retain this identity before the cancellable call.
/// It identifies one receipt transaction, not the success of the source effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiptOperation {
    pub operation_id: Uuid,
    pub message: MessageKey,
    pub kind: EffectReceiptKind,
    pub semantic_identity_hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptCommit {
    /// Historical receipt transaction evidence. It neither proves that the
    /// source effect succeeded nor that the receipt survives later collection.
    Committed {
        operation_id: Uuid,
        inserted: bool,
    },
    Indeterminate {
        operation_id: Uuid,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptReadback {
    /// The exact receipt transaction committed historically. This does not
    /// establish current receipt or source-effect state.
    Committed { inserted: bool },
    /// Absence does not prove rollback or permit a retry.
    Indeterminate,
}

#[derive(Debug, Error)]
pub enum ReceiptAuthorityError {
    #[error("receipt authority requires the exact main database and nonzero independent pins")]
    Binding,
    #[error("receipt operation ID must be nonzero")]
    InvalidOperation,
    #[error("receipt operation ID is already bound to a different receipt")]
    OperationConflict,
    #[error("receipt transaction refused because the catalog mutation fence is held")]
    CatalogFenceBusy,
    #[error("receipt operation transaction could not be prepared")]
    Database,
    #[error(transparent)]
    Profile(#[from] RestrictedIngressProfileError),
}

enum Backend {
    Postgres {
        database: Box<Database>,
        capability: AttestedPostgresWriterCapability,
        api_owner: PgRoleName,
        pins: ReceiptOperationPins,
    },
    Sqlite(Database),
}

/// A closed, pool-bound authority. It exposes no transaction, raw SQL, pool,
/// callback, or separable proof. Its lexical transaction cannot be cloned.
/// This type does not select restricted serving or authorize unrelated stores.
pub struct ReceiptOperationAuthority {
    backend: Backend,
}

struct ReceiptTransaction<'a> {
    transaction: Transaction<'a>,
    backend: &'a Backend,
}

impl ReceiptOperationAuthority {
    pub fn postgres(
        database: Database,
        api_owner: PgRoleName,
        pins: ReceiptOperationPins,
    ) -> Result<Self, ReceiptAuthorityError> {
        let capability = database
            .attested_pg_writer_capability()
            .ok_or(ReceiptAuthorityError::Binding)?;
        if database.driver() != DatabaseDriver::Postgres
            || !database.is_main_lane()
            || !pins.valid()
            || capability.identity().schema.oid != pins.schema_oid
        {
            return Err(ReceiptAuthorityError::Binding);
        }
        database
            .verify_attested_pg_writer_capability(&capability)
            .map_err(|_| ReceiptAuthorityError::Binding)?;
        Ok(Self {
            backend: Backend::Postgres {
                database: Box::new(database),
                capability,
                api_owner,
                pins,
            },
        })
    }

    /// Use the caller's existing SQLite backend, including its serialization.
    pub fn sqlite(database: Database) -> Result<Self, ReceiptAuthorityError> {
        if database.driver() != DatabaseDriver::Sqlite || !database.is_main_lane() {
            return Err(ReceiptAuthorityError::Binding);
        }
        Ok(Self {
            backend: Backend::Sqlite(database),
        })
    }

    /// A dropped future has no result: retain the operation ID independently.
    /// A timeout or failed COMMIT is indeterminate even if it occurred early.
    pub async fn record(
        &self,
        operation: &ReceiptOperation,
    ) -> Result<ReceiptCommit, ReceiptAuthorityError> {
        validate_operation(operation)?;
        match tokio::time::timeout(DEADLINE, async {
            let mut transaction = self.begin().await?;
            let inserted = transaction.record(operation).await?;
            transaction.attest_before_finish().await?;
            Ok(match transaction.transaction.commit().await {
                Ok(()) => ReceiptCommit::Committed {
                    operation_id: operation.operation_id,
                    inserted,
                },
                Err(_) => ReceiptCommit::Indeterminate {
                    operation_id: operation.operation_id,
                },
            })
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Ok(ReceiptCommit::Indeterminate {
                operation_id: operation.operation_id,
            }),
        }
    }

    /// Only an exact positive journal row establishes that this receipt
    /// transaction committed. Missing evidence never establishes rollback.
    pub async fn readback(
        &self,
        operation: &ReceiptOperation,
    ) -> Result<ReceiptReadback, ReceiptAuthorityError> {
        validate_operation(operation)?;
        match tokio::time::timeout(DEADLINE, async {
            let mut transaction = self.begin().await?;
            let result = transaction.find(operation).await?;
            transaction.attest_before_finish().await?;
            transaction
                .transaction
                .rollback()
                .await
                .map_err(|_| ReceiptAuthorityError::Database)?;
            Ok(match result {
                Some(inserted) => ReceiptReadback::Committed { inserted },
                None => ReceiptReadback::Indeterminate,
            })
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Ok(ReceiptReadback::Indeterminate),
        }
    }

    async fn begin(&self) -> Result<ReceiptTransaction<'_>, ReceiptAuthorityError> {
        let mut tx = match &self.backend {
            Backend::Postgres {
                database,
                capability,
                api_owner,
                pins,
            } => {
                database
                    .verify_attested_pg_writer_capability(capability)
                    .map_err(|_| ReceiptAuthorityError::Binding)?;
                let mut tx = database
                    .begin()
                    .await
                    .map_err(|_| ReceiptAuthorityError::Database)?;
                let connection = tx
                    .postgres_connection()
                    .ok_or(ReceiptAuthorityError::Binding)?;
                sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE")
                    .execute(&mut *connection)
                    .await
                    .map_err(RestrictedIngressProfileError::from)?;
                let catalog_fence: bool =
                    sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
                        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(RestrictedIngressProfileError::from)?;
                if !catalog_fence {
                    return Err(ReceiptAuthorityError::CatalogFenceBusy);
                }
                for (name, cap) in [("lock_timeout", 3_000), ("statement_timeout", 5_000)] {
                    let current: String =
                        sqlx::query_scalar("SELECT pg_catalog.current_setting($1)")
                            .bind(name)
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(RestrictedIngressProfileError::from)?;
                    let current =
                        timeout_millis(&current).ok_or(ReceiptAuthorityError::Database)?;
                    if current == 0 || current > cap {
                        sqlx::query("SELECT pg_catalog.set_config($1,$2,true)")
                            .bind(name)
                            .bind(format!("{cap}ms"))
                            .execute(&mut *connection)
                            .await
                            .map_err(RestrictedIngressProfileError::from)?;
                    }
                }
                for key in [
                    LINEAGE_ADVISORY_LOCK_KEY,
                    MIGRATION_LEDGER_ADVISORY_LOCK_KEY,
                ] {
                    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock_shared($1)")
                        .bind(key)
                        .execute(&mut *connection)
                        .await
                        .map_err(RestrictedIngressProfileError::from)?;
                }
                let config = &capability.config;
                let schema = &config.postgres().schema.name.0;
                // Catalog-only preflight avoids locking through a replacement
                // view or inheritance tree. Recheck the OID after any wait.
                let epoch_is_heap: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
                     JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
                     WHERE n.nspname=$1 AND n.oid=$2 AND n.nspowner=$3 \
                       AND c.relname='ingress_protocol_epoch' AND c.oid=$4 \
                       AND c.relkind='r' AND c.relpersistence='p' AND NOT c.relispartition \
                       AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i \
                           WHERE i.inhrelid=c.oid OR i.inhparent=c.oid))",
                )
                .bind(schema)
                .bind(Oid(pins.schema_oid))
                .bind(Oid(pins.schema_owner_role_oid))
                .bind(Oid(pins.epoch_relation_oid))
                .fetch_one(&mut *connection)
                .await
                .map_err(RestrictedIngressProfileError::from)?;
                if !epoch_is_heap {
                    return Err(ReceiptAuthorityError::Binding);
                }
                sqlx::query(&format!(
                    "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS SHARE MODE",
                    quote_identifier(schema)
                ))
                .execute(&mut *connection)
                .await
                .map_err(RestrictedIngressProfileError::from)?;
                attest_connection(connection, config)
                    .await
                    .map_err(RestrictedIngressProfileError::from)?;
                let observed = inspect_profile_with_receipt_journal(
                    connection,
                    schema,
                    pins.schema_oid,
                    config.runtime_login.as_str(),
                    api_owner.as_str(),
                    pins.epoch_relation_oid,
                    true,
                    false,
                )
                .await?;
                let expected = RestrictedIngressProfileObservation {
                    manifest_version: super::MANIFEST_VERSION,
                    schema_oid: pins.schema_oid,
                    epoch_relation_oid: pins.epoch_relation_oid,
                    runtime_role_oid: pins.runtime_role_oid,
                    api_owner_oid: pins.api_owner_role_oid,
                };
                if observed != expected {
                    return Err(ReceiptAuthorityError::Binding);
                }
                receipt_catalog::verify(
                    connection,
                    schema,
                    config.runtime_login.as_str(),
                    api_owner.as_str(),
                    pins,
                    false,
                )
                .await?;
                let row = sqlx::query(&format!(
                    "SELECT epoch, pg_catalog.current_setting('transaction_isolation'), \
                     pg_catalog.current_setting('transaction_read_only'), pg_catalog.pg_is_in_recovery() \
                     FROM {}.ingress_protocol_epoch WHERE id=1",
                    quote_identifier(schema)
                ))
                .fetch_optional(&mut *connection)
                .await
                .map_err(RestrictedIngressProfileError::from)?
                .ok_or(ReceiptAuthorityError::Database)?;
                let epoch = row
                    .try_get::<i64, _>(0)
                    .map_err(RestrictedIngressProfileError::from)?;
                validate_epoch(epoch)?;
                if row
                    .try_get::<String, _>(1)
                    .map_err(RestrictedIngressProfileError::from)?
                    != "read committed"
                    || row
                        .try_get::<String, _>(2)
                        .map_err(RestrictedIngressProfileError::from)?
                        != "off"
                    || row
                        .try_get::<bool, _>(3)
                        .map_err(RestrictedIngressProfileError::from)?
                {
                    return Err(ReceiptAuthorityError::Binding);
                }
                tx
            }
            Backend::Sqlite(database) => {
                let mut tx = database
                    .begin_immediate()
                    .await
                    .map_err(|_| ReceiptAuthorityError::Database)?;
                let mut rows = tx
                    .query(
                        "SELECT epoch FROM main.ingress_protocol_epoch WHERE id=1",
                        (),
                    )
                    .await
                    .map_err(|_| ReceiptAuthorityError::Database)?;
                let row = rows
                    .next()
                    .await
                    .map_err(|_| ReceiptAuthorityError::Database)?
                    .ok_or(ReceiptAuthorityError::Database)?;
                let epoch: i64 = row.get(0).map_err(|_| ReceiptAuthorityError::Database)?;
                validate_epoch(epoch)?;
                drop(rows);
                tx
            }
        };
        // Recheck the physical identity after every profile/lock wait.
        if let Backend::Postgres { capability, .. } = &self.backend {
            let connection = tx
                .postgres_connection()
                .ok_or(ReceiptAuthorityError::Binding)?;
            attest_connection(connection, &capability.config)
                .await
                .map_err(RestrictedIngressProfileError::from)?;
        }
        Ok(ReceiptTransaction {
            transaction: tx,
            backend: &self.backend,
        })
    }
}

impl ReceiptTransaction<'_> {
    async fn attest_before_finish(&mut self) -> Result<(), ReceiptAuthorityError> {
        if let Backend::Postgres { capability, .. } = self.backend {
            let connection = self
                .transaction
                .postgres_connection()
                .ok_or(ReceiptAuthorityError::Binding)?;
            attest_connection(connection, &capability.config)
                .await
                .map_err(RestrictedIngressProfileError::from)?;
        }
        Ok(())
    }

    async fn record(
        &mut self,
        operation: &ReceiptOperation,
    ) -> Result<bool, ReceiptAuthorityError> {
        match self.backend {
            Backend::Postgres { capability, .. } => {
                let schema = &capability.config.postgres().schema.name.0;
                let query = format!(
                    "SELECT {}.waddle_p21_record_effect_receipt($1,$2,$3,$4)",
                    quote_identifier(schema)
                );
                let connection = self
                    .transaction
                    .postgres_connection()
                    .ok_or(ReceiptAuthorityError::Binding)?;
                sqlx::query_scalar(&query)
                    .bind(operation.operation_id)
                    .bind(operation.message.to_storage())
                    .bind(operation.kind.to_storage())
                    .bind(operation.semantic_identity_hash.as_slice())
                    .fetch_one(connection)
                    .await
                    .map_err(|error| match &error {
                        sqlx::Error::Database(database)
                            if database.code().as_deref() == Some("23505") =>
                        {
                            ReceiptAuthorityError::OperationConflict
                        }
                        _ => ReceiptAuthorityError::Profile(
                            RestrictedIngressProfileError::Database(error),
                        ),
                    })
            }
            Backend::Sqlite(_) => {
                if let Some(inserted) = self.find(operation).await? {
                    return Ok(inserted);
                }
                let inserted = self.transaction.execute(
                    "INSERT INTO main.ingress_effect_receipts (message_key,kind,semantic_identity_hash) VALUES (?,?,?) \
                     ON CONFLICT (message_key,kind,semantic_identity_hash) DO NOTHING",
                    crate::db_params![operation.message.to_storage().to_string(), operation.kind.to_storage(), operation.semantic_identity_hash.to_vec()],
                ).await.map_err(|_| ReceiptAuthorityError::Database)? == 1;
                self.transaction.execute(
                    "INSERT INTO main.p21_receipt_operation_results \
                     (operation_id,message_key,receipt_kind,semantic_identity_hash,inserted) VALUES (?,?,?,?,?)",
                    crate::db_params![operation.operation_id.to_string(), operation.message.to_storage().to_string(), operation.kind.to_storage(), operation.semantic_identity_hash.to_vec(), i32::from(inserted)],
                ).await.map_err(|_| ReceiptAuthorityError::Database)?;
                Ok(inserted)
            }
        }
    }

    async fn find(
        &mut self,
        operation: &ReceiptOperation,
    ) -> Result<Option<bool>, ReceiptAuthorityError> {
        match self.backend {
            Backend::Postgres { capability, .. } => {
                let schema = &capability.config.postgres().schema.name.0;
                let query = format!(
                    "SELECT message_key, receipt_kind, semantic_identity_hash, inserted \
                     FROM {}.waddle_p21_lookup_receipt_operation($1)",
                    quote_identifier(schema)
                );
                let connection = self
                    .transaction
                    .postgres_connection()
                    .ok_or(ReceiptAuthorityError::Binding)?;
                let row = sqlx::query(&query)
                    .bind(operation.operation_id)
                    .fetch_optional(connection)
                    .await
                    .map_err(RestrictedIngressProfileError::from)?;
                let Some(row) = row else {
                    return Ok(None);
                };
                let message: Uuid = row
                    .try_get(0)
                    .map_err(RestrictedIngressProfileError::from)?;
                let kind: i32 = row
                    .try_get(1)
                    .map_err(RestrictedIngressProfileError::from)?;
                let hash: Vec<u8> = row
                    .try_get(2)
                    .map_err(RestrictedIngressProfileError::from)?;
                if message != operation.message.to_storage()
                    || kind != operation.kind.to_storage()
                    || hash != operation.semantic_identity_hash
                {
                    return Err(ReceiptAuthorityError::OperationConflict);
                }
                Ok(Some(
                    row.try_get(3)
                        .map_err(RestrictedIngressProfileError::from)?,
                ))
            }
            Backend::Sqlite(_) => {
                let mut rows = self.transaction.query(
                    "SELECT message_key,receipt_kind,semantic_identity_hash,inserted FROM main.p21_receipt_operation_results WHERE operation_id=?",
                    crate::db_params![operation.operation_id.to_string()],
                ).await.map_err(|_| ReceiptAuthorityError::Database)?;
                let Some(row) = rows
                    .next()
                    .await
                    .map_err(|_| ReceiptAuthorityError::Database)?
                else {
                    return Ok(None);
                };
                let message: String = row.get(0).map_err(|_| ReceiptAuthorityError::Database)?;
                let kind: i64 = row.get(1).map_err(|_| ReceiptAuthorityError::Database)?;
                let hash: Vec<u8> = row.get(2).map_err(|_| ReceiptAuthorityError::Database)?;
                let inserted: i64 = row.get(3).map_err(|_| ReceiptAuthorityError::Database)?;
                if message != operation.message.to_storage().to_string()
                    || kind != i64::from(operation.kind.to_storage())
                    || hash != operation.semantic_identity_hash
                {
                    return Err(ReceiptAuthorityError::OperationConflict);
                }
                match inserted {
                    0 => Ok(Some(false)),
                    1 => Ok(Some(true)),
                    _ => Err(ReceiptAuthorityError::Database),
                }
            }
        }
    }
}

fn validate_operation(operation: &ReceiptOperation) -> Result<(), ReceiptAuthorityError> {
    if operation.operation_id.is_nil() {
        Err(ReceiptAuthorityError::InvalidOperation)
    } else {
        Ok(())
    }
}

fn validate_epoch(value: i64) -> Result<(), ReceiptAuthorityError> {
    let epoch = u32::try_from(value)
        .map(ProtocolEpoch::from_storage)
        .map_err(|_| ReceiptAuthorityError::Database)?;
    if epoch > supported_protocol_epoch() {
        Err(ReceiptAuthorityError::Binding)
    } else {
        Ok(())
    }
}
