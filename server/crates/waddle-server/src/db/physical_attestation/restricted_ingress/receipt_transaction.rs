//! Fixture-only closed receipt transaction. This is not a serving selector.
//!
//! Trusted catalog and role mutation, and the separate physical attestor, must
//! remain quiescent. The shared epoch lock fences only cooperating mutators.

use std::time::Duration;

use sqlx::Row;
use tokio::sync::oneshot;
use waddle_xmpp::ingress::{MessageKey, ProtocolEpoch};

use super::{inspect_profile_on_connection, PgRoleName, RestrictedIngressProfileError};
use super::{RestrictedIngressProfileObservation, Result};
use crate::db::lineage::LINEAGE_ADVISORY_LOCK_KEY;
use crate::db::physical_attestation::{
    attest_connection, quote_identifier, AttestedPostgresWriterCapability, PhysicalAttestationError,
};
use crate::db::read_only_timeout::timeout_millis;
use crate::db::{Database, Transaction, MIGRATION_LEDGER_ADVISORY_LOCK_KEY};
use crate::ingress_substrate::{supported_protocol_epoch, EffectReceiptKind};

/// Immutable exact identity retained by the caller even if a future is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestrictedEffectReceipt {
    pub message: MessageKey,
    pub kind: EffectReceiptKind,
    pub semantic_identity_hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestrictedReceiptWrite {
    /// PostgreSQL acknowledged COMMIT. `inserted` is false for a duplicate.
    Committed { inserted: bool },
    /// COMMIT may have reached PostgreSQL, or the operation timed out.
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestrictedReceiptReadback {
    /// A fresh, verified transaction observed this exact receipt.
    Exists,
    /// Absence or a failed inspection never proves the earlier outcome.
    Indeterminate,
}

/// Current state observed under the canonical message lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestrictedTerminalization {
    Incomplete,
    Terminalized,
    AlreadyTerminal,
    MessageVanished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestrictedTerminalizationWrite {
    /// PostgreSQL acknowledged the transaction COMMIT.
    Committed { outcome: RestrictedTerminalization },
    /// The COMMIT may have reached PostgreSQL, or the operation timed out.
    Indeterminate,
}

#[derive(Clone, Copy)]
enum Limits {
    Receipt,
    Completion,
}

// Neither the factory nor its transaction can be cloned, and neither exposes
// SQL, callbacks, GUCs, a transaction or a separable writer capability.
struct Factory {
    database: Database,
    capability: AttestedPostgresWriterCapability,
    api_owner: PgRoleName,
    expected: RestrictedIngressProfileObservation,
}

struct VerifiedReceiptTransaction<'a> {
    tx: Transaction<'a>,
    factory: &'a Factory,
}

/// Dark fixture entry for one exact main pool and one fixed receipt operation.
pub struct RestrictedReceiptPilot {
    factory: Factory,
}

impl RestrictedReceiptPilot {
    pub fn new(
        database: Database,
        authority: &Database,
        api_owner: PgRoleName,
        expected: RestrictedIngressProfileObservation,
    ) -> Result<Self> {
        let capability = authority
            .attested_pg_writer_capability()
            .ok_or(PhysicalAttestationError::WrongPool)?;
        database.verify_attested_pg_writer_capability(&capability)?;
        if !database.is_main_lane()
            || expected.manifest_version != super::MANIFEST_VERSION
            || expected.schema_oid != capability.identity().schema.oid
        {
            return Err(RestrictedIngressProfileError::Mismatch("receipt binding"));
        }
        Ok(Self {
            factory: Factory {
                database,
                capability,
                api_owner,
                expected,
            },
        })
    }

    /// The identity must be retained independently of this cancellable future.
    pub async fn record(
        &self,
        receipt: &RestrictedEffectReceipt,
    ) -> Result<RestrictedReceiptWrite> {
        match tokio::time::timeout(Duration::from_secs(15), async {
            let mut tx = self.factory.begin(Limits::Receipt).await?;
            let inserted = tx.record(receipt).await?;
            Ok(match tx.tx.commit().await {
                Ok(()) => RestrictedReceiptWrite::Committed { inserted },
                Err(_) => RestrictedReceiptWrite::Indeterminate,
            })
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Ok(RestrictedReceiptWrite::Indeterminate),
        }
    }

    /// Fresh exact readback is positive-only; failure and absence are unknown.
    pub async fn readback(&self, receipt: &RestrictedEffectReceipt) -> RestrictedReceiptReadback {
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            let mut tx = self.factory.begin(Limits::Receipt).await?;
            let exists = tx.exists(receipt).await?;
            tx.tx.rollback().await.map_err(|_| {
                RestrictedIngressProfileError::Mismatch("receipt readback rollback")
            })?;
            Ok::<_, RestrictedIngressProfileError>(exists)
        })
        .await;
        if matches!(result, Ok(Ok(true))) {
            RestrictedReceiptReadback::Exists
        } else {
            RestrictedReceiptReadback::Indeterminate
        }
    }

    /// Fixture-only acknowledged rollback of the same fixed candidate write.
    pub async fn record_rolled_back(&self, receipt: &RestrictedEffectReceipt) -> Result<bool> {
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut tx = self.factory.begin(Limits::Receipt).await?;
            let inserted = tx.record(receipt).await?;
            tx.tx.rollback().await.map_err(|_| {
                RestrictedIngressProfileError::Mismatch("receipt rollback unacknowledged")
            })?;
            Ok(inserted)
        })
        .await
        .map_err(|_| RestrictedIngressProfileError::Timeout)?
    }

    /// Pause a fixed write after verification to observe its locks in fixtures.
    pub async fn record_paused(
        &self,
        receipt: &RestrictedEffectReceipt,
        ready: oneshot::Sender<i32>,
        release: oneshot::Receiver<()>,
    ) -> Result<RestrictedReceiptWrite> {
        match tokio::time::timeout(Duration::from_secs(15), async {
            let mut tx = self.factory.begin(Limits::Receipt).await?;
            let connection = tx
                .tx
                .postgres_connection()
                .ok_or(PhysicalAttestationError::PostgresRequired)?;
            let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
                .fetch_one(connection)
                .await?;
            ready.send(pid).map_err(|_| {
                RestrictedIngressProfileError::Mismatch("receipt fixture cancelled")
            })?;
            release.await.map_err(|_| {
                RestrictedIngressProfileError::Mismatch("receipt fixture cancelled")
            })?;
            let inserted = tx.record(receipt).await?;
            Ok(match tx.tx.commit().await {
                Ok(()) => RestrictedReceiptWrite::Committed { inserted },
                Err(_) => RestrictedReceiptWrite::Indeterminate,
            })
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Ok(RestrictedReceiptWrite::Indeterminate),
        }
    }

    /// A later acknowledged call establishes current state only; it cannot
    /// establish whether an earlier unknown COMMIT succeeded.
    pub async fn terminalize_if_complete(
        &self,
        message: MessageKey,
    ) -> Result<RestrictedTerminalizationWrite> {
        match tokio::time::timeout(Duration::from_secs(15), async {
            let mut tx = self.factory.begin(Limits::Completion).await?;
            let outcome = tx.terminalize_if_complete(message).await?;
            Ok(match tx.tx.commit().await {
                Ok(()) => RestrictedTerminalizationWrite::Committed { outcome },
                Err(_) => RestrictedTerminalizationWrite::Indeterminate,
            })
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Ok(RestrictedTerminalizationWrite::Indeterminate),
        }
    }

    /// Fixture-only acknowledged rollback of the closed completion operation.
    pub async fn terminalize_rolled_back(
        &self,
        message: MessageKey,
    ) -> Result<RestrictedTerminalization> {
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut tx = self.factory.begin(Limits::Completion).await?;
            let outcome = tx.terminalize_if_complete(message).await?;
            tx.tx.rollback().await.map_err(|_| {
                RestrictedIngressProfileError::Mismatch("terminalization rollback unacknowledged")
            })?;
            Ok(outcome)
        })
        .await
        .map_err(|_| RestrictedIngressProfileError::Timeout)?
    }
}

impl Factory {
    async fn begin(&self, limits: Limits) -> Result<VerifiedReceiptTransaction<'_>> {
        self.database
            .verify_attested_pg_writer_capability(&self.capability)?;
        // The PostgreSQL lease owns cleanup even if BEGIN is cancelled before
        // it returns. Completion must bound checkout/BEGIN as well as SQL lock
        // and statement waits, matching its existing 100ms caller bound.
        let acquisition = self.database.begin();
        let mut tx = match limits {
            Limits::Receipt => acquisition.await,
            Limits::Completion => tokio::time::timeout(Duration::from_millis(100), acquisition)
                .await
                .map_err(|_| RestrictedIngressProfileError::Timeout)?,
        }
        .map_err(|_| RestrictedIngressProfileError::Mismatch("receipt checkout/transaction"))?;
        let connection = tx
            .postgres_connection()
            .ok_or(PhysicalAttestationError::PostgresRequired)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE")
            .execute(&mut *connection)
            .await?;
        let caps = match limits {
            Limits::Receipt => [("lock_timeout", 3_000), ("statement_timeout", 5_000)],
            Limits::Completion => [("lock_timeout", 100), ("statement_timeout", 250)],
        };
        for (name, cap) in caps {
            let current: String = sqlx::query_scalar("SELECT pg_catalog.current_setting($1)")
                .bind(name)
                .fetch_one(&mut *connection)
                .await?;
            let current = timeout_millis(&current).ok_or(
                RestrictedIngressProfileError::Mismatch("receipt session timeout"),
            )?;
            if current == 0 || current > cap {
                sqlx::query("SELECT pg_catalog.set_config($1, $2, true)")
                    .bind(name)
                    .bind(format!("{cap}ms"))
                    .execute(&mut *connection)
                    .await?;
            }
        }
        for key in [
            LINEAGE_ADVISORY_LOCK_KEY,
            MIGRATION_LEDGER_ADVISORY_LOCK_KEY,
        ] {
            sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock_shared($1)")
                .bind(key)
                .execute(&mut *connection)
                .await?;
        }
        let config = &self.capability.config;
        let schema = &config.postgres().schema.name.0;
        sqlx::query(&format!(
            "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS SHARE MODE",
            quote_identifier(schema)
        ))
        .execute(&mut *connection)
        .await?;
        // These RC statements run after any lock wait, before resolving epoch
        // contents or invoking either candidate function.
        attest_connection(connection, config).await?;
        let profile = inspect_profile_on_connection(
            connection,
            schema,
            self.expected.schema_oid,
            config.runtime_login.as_str(),
            self.api_owner.as_str(),
            self.expected.epoch_relation_oid,
        )
        .await?;
        if profile != self.expected {
            return Err(RestrictedIngressProfileError::Mismatch(
                "receipt pinned identities",
            ));
        }
        let row = sqlx::query(&format!(
            "SELECT epoch, pg_catalog.current_setting('transaction_isolation'), \
             pg_catalog.current_setting('transaction_read_only'), pg_catalog.pg_is_in_recovery() \
             FROM {}.ingress_protocol_epoch WHERE id=1",
            quote_identifier(schema),
        ))
        .fetch_optional(&mut *connection)
        .await?
        .ok_or(RestrictedIngressProfileError::Mismatch(
            "receipt missing epoch",
        ))?;
        let epoch = u32::try_from(row.try_get::<i64, _>(0)?)
            .map(ProtocolEpoch::from_storage)
            .map_err(|_| RestrictedIngressProfileError::Mismatch("receipt unsupported epoch"))?;
        if epoch > supported_protocol_epoch()
            || row.try_get::<String, _>(1)? != "read committed"
            || row.try_get::<String, _>(2)? != "off"
            || row.try_get::<bool, _>(3)?
        {
            return Err(RestrictedIngressProfileError::Mismatch(
                "receipt epoch/transaction mode",
            ));
        }
        attest_connection(connection, config).await?;
        Ok(VerifiedReceiptTransaction { tx, factory: self })
    }
}

impl VerifiedReceiptTransaction<'_> {
    async fn terminalize_if_complete(
        &mut self,
        message: MessageKey,
    ) -> Result<RestrictedTerminalization> {
        let schema = &self.factory.capability.config.postgres().schema.name.0;
        let query = format!(
            "SELECT {}.waddle_ingress_terminalize_if_complete($1)",
            quote_identifier(schema)
        );
        let connection = self
            .tx
            .postgres_connection()
            .ok_or(PhysicalAttestationError::PostgresRequired)?;
        let outcome: i32 = sqlx::query_scalar(&query)
            .bind(message.to_storage())
            .fetch_one(connection)
            .await?;
        match outcome {
            0 => Ok(RestrictedTerminalization::Incomplete),
            1 => Ok(RestrictedTerminalization::Terminalized),
            2 => Ok(RestrictedTerminalization::AlreadyTerminal),
            3 => Ok(RestrictedTerminalization::MessageVanished),
            _ => Err(RestrictedIngressProfileError::Mismatch(
                "terminalization result",
            )),
        }
    }

    async fn record(&mut self, receipt: &RestrictedEffectReceipt) -> Result<bool> {
        let schema = &self.factory.capability.config.postgres().schema.name.0;
        let query = format!(
            "SELECT {}.waddle_ingress_record_effect_receipt($1, $2, $3)",
            quote_identifier(schema)
        );
        let connection = self
            .tx
            .postgres_connection()
            .ok_or(PhysicalAttestationError::PostgresRequired)?;
        Ok(sqlx::query_scalar(&query)
            .bind(receipt.message.to_storage())
            .bind(receipt.kind.to_storage())
            .bind(receipt.semantic_identity_hash.as_slice())
            .fetch_one(connection)
            .await?)
    }

    async fn exists(&mut self, receipt: &RestrictedEffectReceipt) -> Result<bool> {
        let schema = &self.factory.capability.config.postgres().schema.name.0;
        let query = format!(
            "SELECT {}.waddle_ingress_effect_receipt_exists($1, $2, $3, NULL::text)",
            quote_identifier(schema)
        );
        let connection = self
            .tx
            .postgres_connection()
            .ok_or(PhysicalAttestationError::PostgresRequired)?;
        Ok(sqlx::query_scalar(&query)
            .bind(receipt.message.to_storage())
            .bind(receipt.kind.to_storage())
            .bind(receipt.semantic_identity_hash.as_slice())
            .fetch_one(connection)
            .await?)
    }
}
