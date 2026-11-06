//! Fixture-only first slice of transaction entry. No production selector.
//!
//! Provider catalog/role mutation and the pre-transaction physical attestor
//! remain independently trusted and quiescent. AS only fences cooperating AX
//! mutators. This probe performs no ingress call, sets no ingress proof GUC,
//! exposes no transaction, and always rolls back before returning success.

use std::time::Duration;

use sqlx::Row;
use tokio::sync::oneshot;
use waddle_xmpp::ingress::ProtocolEpoch;

use super::{inspect_profile_on_connection, PgRoleName, RestrictedIngressProfileError};
use super::{RestrictedIngressProfileObservation, Result};
use crate::db::lineage::LINEAGE_ADVISORY_LOCK_KEY;
use crate::db::physical_attestation::{
    attest_connection, quote_identifier, AttestedPostgresWriterCapability, PhysicalAttestationError,
};
use crate::db::read_only_timeout::timeout_millis;
use crate::db::{Database, MIGRATION_LEDGER_ADVISORY_LOCK_KEY};
use crate::ingress_substrate::supported_protocol_epoch;

/// Fixture-only diagnostic, never a transaction or serving capability.
#[derive(Debug)]
pub struct RestrictedTransactionProbeObservation {
    pub profile: RestrictedIngressProfileObservation,
    pub epoch: ProtocolEpoch,
    pub backend_pid: i32,
}

// No Clone and no raw database/transaction accessor. The capability's private
// Arc identity binds this factory to one existing Database pool, not a URL.
struct Factory {
    database: Database,
    capability: AttestedPostgresWriterCapability,
    api_owner: PgRoleName,
    expected: RestrictedIngressProfileObservation,
}

/// Available only with `test-support`; production builds have no probe entry.
/// Independently trusted OIDs must be supplied by the fixture's owner setup.
/// An observer receipt is diagnostic and cannot authorize serving.
pub struct RestrictedTransactionProbe {
    factory: Factory,
}

impl RestrictedTransactionProbe {
    /// Bind an exact existing main Database to an authority from that same
    /// pool. A separately opened pool with identical configuration is refused.
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
            || expected.manifest_version != 1
            || expected.schema_oid != capability.identity().schema.oid
        {
            return Err(RestrictedIngressProfileError::Mismatch("probe binding"));
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

    /// Finish one fixed read-only probe, including acknowledged ROLLBACK.
    pub async fn inspect(&self) -> Result<RestrictedTransactionProbeObservation> {
        self.run(None).await
    }

    /// Fixed fixture synchronization after verification, under the same total
    /// deadline. Cancellation drops the lexical transaction; it is not itself
    /// a server-side rollback acknowledgement. No SQL or callback is accepted.
    pub async fn inspect_paused(
        &self,
        ready: oneshot::Sender<i32>,
        release: oneshot::Receiver<()>,
    ) -> Result<RestrictedTransactionProbeObservation> {
        self.run(Some((ready, release))).await
    }

    async fn run(
        &self,
        pause: Option<(oneshot::Sender<i32>, oneshot::Receiver<()>)>,
    ) -> Result<RestrictedTransactionProbeObservation> {
        tokio::time::timeout(Duration::from_secs(15), self.factory.inspect(pause))
            .await
            .map_err(|_| RestrictedIngressProfileError::Timeout)?
    }
}

impl Factory {
    async fn inspect(
        &self,
        pause: Option<(oneshot::Sender<i32>, oneshot::Receiver<()>)>,
    ) -> Result<RestrictedTransactionProbeObservation> {
        self.database
            .verify_attested_pg_writer_capability(&self.capability)?;
        let mut tx =
            self.database.begin().await.map_err(|_| {
                RestrictedIngressProfileError::Mismatch("probe checkout/transaction")
            })?;
        let connection = tx
            .postgres_connection()
            .ok_or(PhysicalAttestationError::PostgresRequired)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY")
            .execute(&mut *connection)
            .await?;
        for (name, cap) in [("lock_timeout", 3_000), ("statement_timeout", 5_000)] {
            let current: String = sqlx::query_scalar("SELECT pg_catalog.current_setting($1)")
                .bind(name)
                .fetch_one(&mut *connection)
                .await?;
            let current = timeout_millis(&current).ok_or(
                RestrictedIngressProfileError::Mismatch("probe session timeout"),
            )?;
            if current == 0 || current > cap {
                sqlx::query("SELECT pg_catalog.set_config($1, $2, true)")
                    .bind(name)
                    .bind(format!("{cap}ms"))
                    .execute(&mut *connection)
                    .await?;
            }
        }
        // No protected relation read before the ordered advisory locks and
        // epoch relation AS. The pool hook's attestor is a separate boundary.
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
            quote_identifier(schema),
        ))
        .execute(&mut *connection)
        .await?;
        // Fresh RC statements after LOCK: do not reuse checkout observations
        // or any previous diagnostic receipt to approve the transaction.
        attest_connection(connection, config).await?;
        let row = sqlx::query(&format!(
            "SELECT epoch, pg_catalog.pg_backend_pid(), \
             pg_catalog.current_setting('transaction_isolation'), \
             pg_catalog.current_setting('transaction_read_only'), pg_catalog.pg_is_in_recovery() \
             FROM {}.ingress_protocol_epoch WHERE id=1",
            quote_identifier(schema),
        ))
        .fetch_optional(&mut *connection)
        .await?
        .ok_or(RestrictedIngressProfileError::Mismatch(
            "probe missing epoch",
        ))?;
        let epoch = u32::try_from(row.try_get::<i64, _>(0)?)
            .map(ProtocolEpoch::from_storage)
            .map_err(|_| RestrictedIngressProfileError::Mismatch("probe unsupported epoch"))?;
        if epoch > supported_protocol_epoch()
            || row.try_get::<String, _>(2)? != "read committed"
            || row.try_get::<String, _>(3)? != "on"
            || row.try_get::<bool, _>(4)?
        {
            return Err(RestrictedIngressProfileError::Mismatch(
                "probe epoch/transaction mode",
            ));
        }
        let backend_pid = row.try_get(1)?;
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
                "probe pinned identities",
            ));
        }
        // Full verifier restored and checked application search_path. Recheck
        // physical/session identity on that same connection before rollback.
        attest_connection(connection, config).await?;
        if let Some((ready, release)) = pause {
            ready
                .send(backend_pid)
                .map_err(|_| RestrictedIngressProfileError::Mismatch("probe fixture cancelled"))?;
            release
                .await
                .map_err(|_| RestrictedIngressProfileError::Mismatch("probe fixture cancelled"))?;
        }
        tx.rollback().await.map_err(|_| {
            RestrictedIngressProfileError::Mismatch("probe rollback unacknowledged")
        })?;
        Ok(RestrictedTransactionProbeObservation {
            profile,
            epoch,
            backend_pid,
        })
    }
}
