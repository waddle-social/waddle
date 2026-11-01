//! Dark P2.1 PostgreSQL physical-connection attestation capability.
//!
//! Normal startup does not call this module. The installer and pool hook are
//! exercised against disposable PostgreSQL before they become authority.

use std::str::FromStr;
use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{PgConnection, Row};
use thiserror::Error;

#[cfg(any(test, feature = "test-support"))]
use super::lineage::LineageError;
use super::lineage::LINEAGE_FORMAT;
use super::lineage::{
    AttestedLineage, DeploymentUuid, LineageUuid, PgDatabaseIdentity, PgDatabaseName, PgIdentity,
    PgSchemaIdentity, PgSchemaName,
};
#[cfg(any(test, feature = "test-support"))]
use super::DatabaseError;

mod installer;
pub use installer::{inspect_installation, install, InstallationStatus};

const FUNCTION_NAME: &str = "read_physical_identity";

#[derive(Debug, Error)]
pub enum PhysicalAttestationError {
    #[error("invalid PostgreSQL identifier for {field}")]
    InvalidIdentifier { field: &'static str },
    #[error("expected a PostgreSQL lineage identity")]
    MissingPostgresIdentity,
    #[error("configured pool login differs from attested runtime login")]
    WrongConfiguredLogin,
    #[error("configured installer login differs from the provisioned installer login")]
    WrongInstallerLogin,
    #[error("physical attestation capability belongs to a different PostgreSQL pool")]
    WrongPool,
    #[error("physical attestation capability requires PostgreSQL")]
    PostgresRequired,
    #[error("physical connection identity differs at {field}")]
    Mismatch { field: &'static str },
    #[error("physical connection returned invalid {field}")]
    InvalidValue { field: &'static str },
    #[error("physical attestation returned no lineage row")]
    MissingLineage,
    #[error("physical attestation installation {operation_id} has an unknown commit outcome")]
    CommitUnknown { operation_id: uuid::Uuid },
    #[error("physical attestation installation {operation_id} committed but fresh-session verification failed")]
    PostCommitUnverified {
        operation_id: uuid::Uuid,
        #[source]
        source: Box<PhysicalAttestationError>,
    },
    #[error("physical attestation installation conflicts with an existing object or operation")]
    InstallationConflict,
    #[error("physical attestation installation is not visible and remains indeterminate")]
    InstallationIndeterminate,
    #[error("physical attestation requires a writable primary")]
    NotPrimary,
    #[error("PostgreSQL attestation query failed")]
    Database(#[from] sqlx::Error),
}

/// A role name restricted to simple, case-stable PostgreSQL identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgRoleName(String);

impl PgRoleName {
    pub fn new(value: impl Into<String>) -> Result<Self, PhysicalAttestationError> {
        let value = value.into();
        validate_identifier(&value, "role")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The complete identity expected before a physical connection is lent out.
#[derive(Debug, Clone)]
pub struct PhysicalAttestationConfig {
    lineage: AttestedLineage,
    postgres: PgIdentity,
    installer_login: PgRoleName,
    runtime_login: PgRoleName,
    definer_role: PgRoleName,
    trusted_schema: PgSchemaName,
}

/// PostgreSQL pool whose connections are checked by the physical attestor
/// before SQLx lends them. The private capability is consumed with this pool
/// when it is wrapped as a [`Database`](super::Database); callers cannot
/// attach it to a different pool. The configured endpoint must preserve a
/// server session across checkout and transaction start; transaction- and
/// statement-pooling proxies are unsupported unless session pinning is
/// independently guaranteed.
#[derive(Clone)]
pub struct AttestedPgPool {
    pool: PgPool,
    #[cfg(any(test, feature = "test-support"))]
    writer_capability: AttestedPostgresWriterCapability,
}

/// Opaque authority for dark PostgreSQL ingress writers. It is created only
/// together with the hook-bearing pool and carries the full physical identity
/// and lineage expected by the trusted SECURITY DEFINER attestor.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
pub(crate) struct AttestedPostgresWriterCapability {
    binding: std::sync::Arc<()>,
    config: PhysicalAttestationConfig,
}

#[cfg(any(test, feature = "test-support"))]
impl AttestedPostgresWriterCapability {
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn identity(&self) -> &PgIdentity {
        self.config.postgres()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn belongs_to(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.binding, &other.binding)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn attest_transaction(
        &self,
        transaction: &mut super::Transaction<'_>,
    ) -> Result<(), DatabaseError> {
        let connection = transaction
            .postgres_connection()
            .ok_or(PhysicalAttestationError::PostgresRequired)?;
        attest_connection(connection, &self.config)
            .await
            .map_err(DatabaseError::from)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn attested_lineage_for_deployment(
        &self,
        configured: Option<DeploymentUuid>,
    ) -> Result<AttestedLineage, DatabaseError> {
        let persisted = self.config.lineage.deployment_uuid;
        match configured {
            Some(configured) if configured == persisted => Ok(self.config.lineage.clone()),
            Some(configured) => Err(LineageError::DeploymentUuidMismatch {
                configured,
                persisted,
            }
            .into()),
            None => Err(LineageError::DeploymentUuidUnconfigured { persisted }.into()),
        }
    }
}

impl AttestedPgPool {
    /// Acquire through the same hook-bearing SQLx pool that carries this
    /// wrapper's capability.
    pub async fn acquire(&self) -> Result<PoolConnection<sqlx::Postgres>, sqlx::Error> {
        self.pool.acquire().await
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub fn num_idle(&self) -> usize {
        self.pool.num_idle()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn into_parts(self) -> (PgPool, AttestedPostgresWriterCapability) {
        (self.pool, self.writer_capability)
    }
}

impl PhysicalAttestationConfig {
    pub fn new(
        lineage: AttestedLineage,
        installer_login: PgRoleName,
        runtime_login: PgRoleName,
        definer_role: PgRoleName,
        trusted_schema: PgSchemaName,
    ) -> Result<Self, PhysicalAttestationError> {
        let postgres = lineage
            .postgres_identity
            .as_ref()
            .ok_or(PhysicalAttestationError::MissingPostgresIdentity)?
            .clone();
        validate_identifier(&postgres.schema.name.0, "lineage schema")?;
        validate_identifier(&trusted_schema.0, "trusted schema")?;
        if postgres.schema.name == trusted_schema {
            return Err(PhysicalAttestationError::InvalidIdentifier {
                field: "trusted schema must differ from lineage schema",
            });
        }
        if runtime_login == definer_role
            || installer_login == definer_role
            || installer_login == runtime_login
        {
            return Err(PhysicalAttestationError::InvalidIdentifier {
                field: "installer, definer and runtime roles must differ",
            });
        }
        Ok(Self {
            lineage,
            postgres,
            installer_login,
            runtime_login,
            definer_role,
            trusted_schema,
        })
    }

    fn postgres(&self) -> &PgIdentity {
        &self.postgres
    }

    fn function_path(&self) -> String {
        format!(
            "{}.{}",
            quote_identifier(&self.trusted_schema.0),
            FUNCTION_NAME
        )
    }
}

fn validate_identifier(value: &str, field: &'static str) -> Result<(), PhysicalAttestationError> {
    let mut bytes = value.bytes();
    let valid_start = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte == b'_');
    if !valid_start
        || value.len() > 63
        || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(PhysicalAttestationError::InvalidIdentifier { field });
    }
    Ok(())
}

fn quote_identifier(value: &str) -> String {
    // Every caller validates the narrow identifier grammar before this point.
    format!("\"{value}\"")
}

fn identity_select(config: &PhysicalAttestationConfig) -> String {
    let target = quote_identifier(&config.postgres().schema.name.0);
    let schema = &config.postgres().schema.name.0;
    format!(
        "SELECT l.lineage_uuid, l.deployment_uuid, \
         l.pg_system_identifier, l.pg_database_oid, l.pg_database_name, \
         l.pg_schema_oid, l.pg_schema_name, \
         (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
         (SELECT oid::text FROM pg_catalog.pg_database WHERE datname = pg_catalog.current_database()), \
         pg_catalog.current_database()::text, \
         (SELECT oid::text FROM pg_catalog.pg_namespace WHERE nspname = '{schema}'), \
         '{schema}'::text, l.format::bigint \
         FROM {target}._lineage AS l WHERE l.id = 1"
    )
}

async fn read_preflight_snapshot(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
) -> Result<Snapshot, PhysicalAttestationError> {
    let row = sqlx::query(&identity_select(config))
        .fetch_optional(connection)
        .await?
        .ok_or(PhysicalAttestationError::MissingLineage)?;
    parse_snapshot(&row)
}

#[derive(Debug)]
struct Snapshot {
    stored: PhysicalIdentity,
    live: PgIdentity,
    lineage_format: i64,
}

#[derive(Debug)]
struct PhysicalIdentity {
    deployment_uuid: DeploymentUuid,
    lineage_uuid: LineageUuid,
    postgres: PgIdentity,
}

async fn read_snapshot(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
) -> Result<Snapshot, PhysicalAttestationError> {
    let sql = format!("SELECT * FROM {}()", config.function_path());
    let row = sqlx::query(&sql)
        .fetch_optional(connection)
        .await?
        .ok_or(PhysicalAttestationError::MissingLineage)?;
    parse_snapshot(&row)
}

fn parse_snapshot(row: &sqlx::postgres::PgRow) -> Result<Snapshot, PhysicalAttestationError> {
    let field = |index: usize, name| -> Result<String, PhysicalAttestationError> {
        row.try_get::<Option<String>, _>(index)?
            .ok_or(PhysicalAttestationError::InvalidValue { field: name })
    };
    let stored = PhysicalIdentity {
        lineage_uuid: parse(&field(0, "lineage UUID")?, "lineage UUID")?,
        deployment_uuid: parse(&field(1, "deployment UUID")?, "deployment UUID")?,
        postgres: PgIdentity {
            system_identifier: parse(
                &field(2, "stored system identifier")?,
                "stored system identifier",
            )?,
            database: PgDatabaseIdentity {
                oid: parse(&field(3, "stored database OID")?, "stored database OID")?,
                name: PgDatabaseName(field(4, "stored database name")?),
            },
            schema: PgSchemaIdentity {
                oid: parse(&field(5, "stored schema OID")?, "stored schema OID")?,
                name: PgSchemaName(field(6, "stored schema name")?),
            },
        },
    };
    let live = PgIdentity {
        system_identifier: parse(
            &field(7, "live system identifier")?,
            "live system identifier",
        )?,
        database: PgDatabaseIdentity {
            oid: parse(&field(8, "live database OID")?, "live database OID")?,
            name: PgDatabaseName(field(9, "live database name")?),
        },
        schema: PgSchemaIdentity {
            oid: parse(&field(10, "live schema OID")?, "live schema OID")?,
            name: PgSchemaName(field(11, "live schema name")?),
        },
    };
    let lineage_format: i64 = row.try_get(12)?;
    Ok(Snapshot {
        stored,
        live,
        lineage_format,
    })
}

fn parse<T: FromStr>(value: &str, field: &'static str) -> Result<T, PhysicalAttestationError> {
    value
        .parse()
        .map_err(|_| PhysicalAttestationError::InvalidValue { field })
}

fn verify_identity(
    snapshot: &Snapshot,
    config: &PhysicalAttestationConfig,
) -> Result<(), PhysicalAttestationError> {
    if snapshot.lineage_format != LINEAGE_FORMAT {
        return Err(PhysicalAttestationError::Mismatch {
            field: "lineage format",
        });
    }
    let expected = config.postgres();
    let stored = &snapshot.stored;
    if stored.deployment_uuid != config.lineage.deployment_uuid {
        return Err(PhysicalAttestationError::Mismatch {
            field: "deployment UUID",
        });
    }
    if stored.lineage_uuid != config.lineage.lineage_uuid {
        return Err(PhysicalAttestationError::Mismatch {
            field: "lineage UUID",
        });
    }
    if &stored.postgres != expected || &snapshot.live != expected {
        return Err(PhysicalAttestationError::Mismatch {
            field: "PostgreSQL system/database/schema identity",
        });
    }
    Ok(())
}

/// Check the exact physical connection used by a prospective pool member.
pub async fn attest_connection(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
) -> Result<(), PhysicalAttestationError> {
    let row = sqlx::query(
        "SELECT session_user::text, current_user::text, pg_catalog.current_schema()::text",
    )
    .fetch_one(&mut *connection)
    .await?;
    let session_user: String = row.try_get(0)?;
    let current_user: String = row.try_get(1)?;
    let current_schema: Option<String> = row.try_get(2)?;
    if session_user != config.runtime_login.as_str()
        || current_user != config.runtime_login.as_str()
    {
        return Err(PhysicalAttestationError::Mismatch {
            field: "runtime login/session role",
        });
    }
    if current_schema.as_deref() != Some(&config.postgres().schema.name.0) {
        return Err(PhysicalAttestationError::Mismatch {
            field: "active schema",
        });
    }
    let snapshot = read_snapshot(connection, config).await?;
    verify_identity(&snapshot, config)
}

/// A dark pool constructor: no production caller currently invokes it.
///
/// The endpoint must preserve a PostgreSQL server session across the
/// before-acquire hook and transaction start. Transaction- and
/// statement-pooling proxies are unsupported unless session pinning is
/// independently guaranteed.
/// SQLx may retry a failed hook until the acquire deadline, so callers must
/// treat a timeout as unavailable, not as a definitive identity diagnosis.
pub fn attested_pool(
    options: PgConnectOptions,
    config: PhysicalAttestationConfig,
    max_connections: u32,
    acquire_timeout: Duration,
) -> Result<AttestedPgPool, PhysicalAttestationError> {
    if options.get_username() != config.runtime_login.as_str() {
        return Err(PhysicalAttestationError::WrongConfiguredLogin);
    }
    let on_connect = config.clone();
    let before_acquire = config.clone();
    let pool = PgPoolOptions::new()
        .min_connections(0)
        .max_connections(max_connections)
        .acquire_timeout(acquire_timeout)
        .after_connect(move |connection, _| {
            let config = on_connect.clone();
            Box::pin(async move {
                attest_connection(connection, &config)
                    .await
                    .map_err(|_| sqlx::Error::Protocol("physical attestation refused".to_owned()))
            })
        })
        .before_acquire(move |connection, _| {
            let config = before_acquire.clone();
            Box::pin(async move {
                // SQLx tracks ReadyForQuery transaction state internally but
                // does not expose it to pool hooks. A borrower can issue raw
                // BEGIN and return a pinned REPEATABLE READ snapshot. End any
                // such transaction before reading the current lineage. A
                // failed ROLLBACK hard-closes the connection.
                sqlx::query("ROLLBACK").execute(&mut *connection).await?;
                attest_connection(connection, &config)
                    .await
                    .map(|()| true)
                    .map_err(|_| sqlx::Error::Protocol("physical attestation refused".to_owned()))
            })
        })
        .connect_lazy_with(options);
    Ok(AttestedPgPool {
        pool,
        #[cfg(any(test, feature = "test-support"))]
        writer_capability: AttestedPostgresWriterCapability {
            binding: std::sync::Arc::new(()),
            config,
        },
    })
}
