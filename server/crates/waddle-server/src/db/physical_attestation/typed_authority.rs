//! A dark, closed transaction entry for one ingress operation.
//!
//! This is deliberately not wired into server startup. The existing runtime
//! still owns other raw database handles and V1009 still takes an epoch row
//! lock. Neither this type nor its tests establish a live P2.1 boundary.

use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection, Postgres, Row, Transaction};
use thiserror::Error;
use waddle_xmpp::ingress::{MessageKey, ProtocolEpoch};

use super::{attested_pool, quote_identifier, PhysicalAttestationConfig, PhysicalAttestationError};
use crate::db::{Database, DatabaseConfig, DatabaseDriver, DatabaseError};
use crate::ingress::EffectReceiptKey;
use crate::ingress_substrate::supported_protocol_epoch;

/// Bounds on checkout and the first PostgreSQL relation-lock wait.
#[derive(Debug, Clone, Copy)]
pub struct TransactionBounds {
    acquisition: Duration,
    lock_ms: u32,
    statement_ms: u32,
}

impl TransactionBounds {
    /// Construct positive bounds representable by PostgreSQL's timeout GUCs.
    pub fn new(
        acquisition: Duration,
        lock: Duration,
        statement: Duration,
    ) -> Result<Self, DarkIngressAuthorityError> {
        fn milliseconds(duration: Duration) -> Result<u32, DarkIngressAuthorityError> {
            let value = u32::try_from(duration.as_millis())
                .map_err(|_| DarkIngressAuthorityError::InvalidBounds)?;
            if value == 0 || value > i32::MAX as u32 {
                return Err(DarkIngressAuthorityError::InvalidBounds);
            }
            Ok(value)
        }
        if acquisition.is_zero() {
            return Err(DarkIngressAuthorityError::InvalidBounds);
        }
        Ok(Self {
            acquisition,
            lock_ms: milliseconds(lock)?,
            statement_ms: milliseconds(statement)?,
        })
    }
}

/// Failures are deliberately opaque: driver diagnostics can include values.
#[derive(Debug, Error)]
pub enum DarkIngressAuthorityError {
    #[error("ingress transaction bounds or pool size are invalid")]
    InvalidBounds,
    #[error("physical attestation configuration was rejected")]
    Attestation(#[from] PhysicalAttestationError),
    #[error("ingress transaction acquisition or preparation timed out")]
    Timeout,
    #[error("the ingress epoch singleton row is missing")]
    MissingEpoch,
    #[error("the live ingress epoch is unsupported")]
    UnsupportedEpoch,
    #[error("the attested PostgreSQL schema identity changed")]
    SchemaMismatch,
    #[error("the ingress database operation failed")]
    Database,
    #[error("the ingress commit outcome is indeterminate")]
    CommitIndeterminate,
}

enum Backend {
    Postgres {
        pool: PgPool,
        schema: String,
        schema_oid: u32,
    },
    Sqlite(Database),
}

/// An isolated, dark authority with no raw pool, SQL, or transaction accessor.
///
/// Its public operations finish the transaction before returning. There is no
/// generic asynchronous callback that could wait on an external action while a
/// transaction is held. This component is not used by normal server startup.
///
/// ```compile_fail
/// use waddle_server::db::physical_attestation::DarkIngressAuthority;
/// fn escape(authority: &DarkIngressAuthority) {
///     let _pool = authority.pool();
/// }
/// ```
///
/// ```compile_fail
/// use waddle_server::db::physical_attestation::DarkIngressAuthority;
/// fn copy(authority: DarkIngressAuthority) {
///     let _duplicate = authority.clone();
/// }
/// ```
pub struct DarkIngressAuthority {
    backend: Backend,
    bounds: TransactionBounds,
}

impl DarkIngressAuthority {
    /// Create a private PostgreSQL pool that attests each physical checkout.
    pub fn postgres(
        options: PgConnectOptions,
        expected: PhysicalAttestationConfig,
        pool_size: u32,
        bounds: TransactionBounds,
    ) -> Result<Self, DarkIngressAuthorityError> {
        if pool_size == 0 {
            return Err(DarkIngressAuthorityError::InvalidBounds);
        }
        let schema = expected.postgres().schema.name.0.clone();
        let schema_oid = expected.postgres().schema.oid;
        let pool = attested_pool(options, expected, pool_size, bounds.acquisition)?;
        Ok(Self {
            backend: Backend::Postgres {
                pool,
                schema,
                schema_oid,
            },
            bounds,
        })
    }

    /// Open an isolated SQLite handle. A shared-memory URL may name an existing
    /// SQLite database, but this dark constructor does not replace the shared
    /// runtime pool or initialize its schema.
    pub async fn sqlite(
        database_url: &str,
        bounds: TransactionBounds,
    ) -> Result<Self, DarkIngressAuthorityError> {
        let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, database_url);
        // A private in-memory SQLite database must stay on one connection.
        config.pool_size = 1;
        config.acquire_timeout = bounds.acquisition;
        let db = Database::from_config("dark ingress authority", &config)
            .await
            .map_err(|_| DarkIngressAuthorityError::Database)?;
        Ok(Self {
            backend: Backend::Sqlite(db),
            bounds,
        })
    }

    /// Probe the supported epoch through the same transaction entry as writes.
    pub async fn current_epoch(&self) -> Result<ProtocolEpoch, DarkIngressAuthorityError> {
        match &self.backend {
            Backend::Postgres {
                pool,
                schema,
                schema_oid,
            } => {
                let mut connection = self.acquire_postgres(pool).await?;
                let prepared = tokio::time::timeout(
                    self.bounds.acquisition,
                    prepare_postgres(&mut connection, schema, *schema_oid, self.bounds),
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Timeout)??;
                let epoch = prepared.epoch;
                prepared.commit().await?;
                Ok(epoch)
            }
            Backend::Sqlite(db) => {
                let mut tx = self.begin_sqlite(db).await?;
                let epoch = read_sqlite_epoch(&mut tx).await?;
                tx.commit()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::CommitIndeterminate)?;
                Ok(epoch)
            }
        }
    }

    /// Record an effect receipt using the existing idempotent key and foreign
    /// key contract. No transport or callback runs while the transaction lives.
    pub async fn record_receipt(
        &self,
        key: MessageKey,
        receipt: &EffectReceiptKey,
    ) -> Result<(), DarkIngressAuthorityError> {
        match &self.backend {
            Backend::Postgres {
                pool,
                schema,
                schema_oid,
            } => {
                let mut connection = self.acquire_postgres(pool).await?;
                let mut prepared = tokio::time::timeout(
                    self.bounds.acquisition,
                    prepare_postgres(&mut connection, schema, *schema_oid, self.bounds),
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Timeout)??;
                let sql = format!(
                    "INSERT INTO {}.ingress_effect_receipts \
                     (message_key, kind, semantic_identity_hash) VALUES ($1, $2, $3) \
                     ON CONFLICT (message_key, kind, semantic_identity_hash) DO NOTHING",
                    quote_identifier(schema)
                );
                sqlx::query(&sql)
                    .bind(key.to_storage())
                    .bind(receipt.kind.to_storage())
                    .bind(receipt.semantic_identity_hash.as_slice())
                    .execute(&mut *prepared.transaction)
                    .await
                    .map_err(|_| DarkIngressAuthorityError::Database)?;
                prepared.commit().await
            }
            Backend::Sqlite(db) => {
                let mut tx = self.begin_sqlite(db).await?;
                read_sqlite_epoch(&mut tx).await?;
                tx.execute(
                    "INSERT INTO ingress_effect_receipts \
                     (message_key, kind, semantic_identity_hash) VALUES (?, ?, ?) \
                     ON CONFLICT (message_key, kind, semantic_identity_hash) DO NOTHING",
                    crate::db_params![
                        key.to_storage().to_string(),
                        receipt.kind.to_storage(),
                        receipt.semantic_identity_hash.to_vec()
                    ],
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Database)?;
                tx.commit()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::CommitIndeterminate)
            }
        }
    }

    async fn acquire_postgres(
        &self,
        pool: &PgPool,
    ) -> Result<sqlx::pool::PoolConnection<Postgres>, DarkIngressAuthorityError> {
        tokio::time::timeout(self.bounds.acquisition, pool.acquire())
            .await
            .map_err(|_| DarkIngressAuthorityError::Timeout)?
            .map_err(postgres_error)
    }

    async fn begin_sqlite<'a>(
        &self,
        db: &'a Database,
    ) -> Result<crate::db::Transaction<'a>, DarkIngressAuthorityError> {
        tokio::time::timeout(self.bounds.acquisition, db.begin_immediate())
            .await
            .map_err(|_| DarkIngressAuthorityError::Timeout)?
            .map_err(|_| DarkIngressAuthorityError::Database)
    }
}

/// Private and borrowed from the checked-out connection, so it cannot be
/// cloned, returned to an API caller, or moved into a `'static` task.
struct PreparedPostgres<'a> {
    transaction: Transaction<'a, Postgres>,
    epoch: ProtocolEpoch,
}

impl PreparedPostgres<'_> {
    async fn commit(self) -> Result<(), DarkIngressAuthorityError> {
        self.transaction
            .commit()
            .await
            .map_err(|_| DarkIngressAuthorityError::CommitIndeterminate)
    }
}

async fn prepare_postgres<'a>(
    connection: &'a mut PgConnection,
    schema: &str,
    expected_schema_oid: u32,
    bounds: TransactionBounds,
) -> Result<PreparedPostgres<'a>, DarkIngressAuthorityError> {
    // Inspect session defaults before BEGIN so no SELECT can precede the epoch
    // relation lock in the transaction. Never loosen a tighter session bound.
    let (lock_ms, statement_ms): (i64, i64) = sqlx::query_as(
        "SELECT (EXTRACT(EPOCH FROM pg_catalog.current_setting('lock_timeout')::interval) * 1000)::bigint, \
                (EXTRACT(EPOCH FROM pg_catalog.current_setting('statement_timeout')::interval) * 1000)::bigint",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(|_| DarkIngressAuthorityError::Database)?;
    let bounded = |configured: u32, current: i64| -> u32 {
        if current <= 0 {
            configured
        } else {
            configured.min(u32::try_from(current).unwrap_or(u32::MAX))
        }
    };
    let lock_ms = bounded(bounds.lock_ms, lock_ms);
    let statement_ms = bounded(bounds.statement_ms, statement_ms);
    let mut transaction = connection
        .begin()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *transaction)
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    for setting in [
        format!("SET LOCAL lock_timeout = '{lock_ms}ms'"),
        format!("SET LOCAL statement_timeout = '{statement_ms}ms'"),
    ] {
        sqlx::query(&setting)
            .execute(&mut *transaction)
            .await
            .map_err(|_| DarkIngressAuthorityError::Database)?;
    }
    let qualified = quote_identifier(schema);
    let lock = format!("LOCK TABLE ONLY {qualified}.ingress_protocol_epoch IN ACCESS SHARE MODE");
    sqlx::query(&lock)
        .execute(&mut *transaction)
        .await
        .map_err(postgres_error)?;
    let read = format!(
        "SELECT epoch, (SELECT oid::bigint FROM pg_catalog.pg_namespace WHERE nspname = $1) \
         FROM {qualified}.ingress_protocol_epoch WHERE id = 1"
    );
    let row = sqlx::query(&read)
        .bind(schema)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
        .ok_or(DarkIngressAuthorityError::MissingEpoch)?;
    let live_schema: Option<i64> = row
        .try_get(1)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    if live_schema != Some(i64::from(expected_schema_oid)) {
        return Err(DarkIngressAuthorityError::SchemaMismatch);
    }
    let value: i64 = row
        .try_get(0)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let epoch = supported_epoch(value)?;
    sqlx::query(
        "SELECT pg_catalog.set_config('waddle.protocol_epoch', $1, true), \
         pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)",
    )
    .bind(epoch.to_storage().to_string())
    .execute(&mut *transaction)
    .await
    .map_err(|_| DarkIngressAuthorityError::Database)?;
    Ok(PreparedPostgres { transaction, epoch })
}

async fn read_sqlite_epoch(
    tx: &mut crate::db::Transaction<'_>,
) -> Result<ProtocolEpoch, DarkIngressAuthorityError> {
    let mut rows = tx
        .query("SELECT epoch FROM ingress_protocol_epoch WHERE id = 1", ())
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let row = rows
        .next()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
        .ok_or(DarkIngressAuthorityError::MissingEpoch)?;
    let value: i64 = row
        .get(0)
        .map_err(|_: DatabaseError| DarkIngressAuthorityError::Database)?;
    supported_epoch(value)
}

fn supported_epoch(value: i64) -> Result<ProtocolEpoch, DarkIngressAuthorityError> {
    let value = u32::try_from(value).map_err(|_| DarkIngressAuthorityError::UnsupportedEpoch)?;
    let epoch = ProtocolEpoch::from_storage(value);
    if epoch > supported_protocol_epoch() {
        return Err(DarkIngressAuthorityError::UnsupportedEpoch);
    }
    Ok(epoch)
}

fn postgres_error(error: sqlx::Error) -> DarkIngressAuthorityError {
    match error {
        sqlx::Error::PoolTimedOut => DarkIngressAuthorityError::Timeout,
        sqlx::Error::Database(ref database)
            if matches!(database.code().as_deref(), Some("55P03" | "57014")) =>
        {
            DarkIngressAuthorityError::Timeout
        }
        _ => DarkIngressAuthorityError::Database,
    }
}
