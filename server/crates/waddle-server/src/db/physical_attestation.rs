//! Dark P2.1 PostgreSQL physical-connection attestation capability.
//!
//! Normal startup does not call this module. The installer and pool hook are
//! exercised against disposable PostgreSQL before they become authority.

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Connection, PgConnection, Row};
use thiserror::Error;

use super::lineage::LINEAGE_FORMAT;
use super::lineage::{
    AttestedLineage, DeploymentUuid, LineageUuid, PgDatabaseIdentity, PgDatabaseName, PgIdentity,
    PgSchemaIdentity, PgSchemaName,
};

const FUNCTION_NAME: &str = "read_physical_identity";

#[derive(Debug, Error)]
pub enum PhysicalAttestationError {
    #[error("invalid PostgreSQL identifier for {field}")]
    InvalidIdentifier { field: &'static str },
    #[error("expected a PostgreSQL lineage identity")]
    MissingPostgresIdentity,
    #[error("configured pool login differs from attested runtime login")]
    WrongConfiguredLogin,
    #[error("physical connection identity differs at {field}")]
    Mismatch { field: &'static str },
    #[error("physical connection returned invalid {field}")]
    InvalidValue { field: &'static str },
    #[error("physical attestation returned no lineage row")]
    MissingLineage,
    #[error("physical attestation installation commit has unknown outcome")]
    CommitUnknown(#[source] sqlx::Error),
    #[error("physical attestation installation committed but fresh-session verification failed")]
    PostCommitUnverified(#[source] Box<PhysicalAttestationError>),
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
    runtime_login: PgRoleName,
    definer_role: PgRoleName,
    trusted_schema: PgSchemaName,
}

impl PhysicalAttestationConfig {
    pub fn new(
        lineage: AttestedLineage,
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
        if runtime_login == definer_role {
            return Err(PhysicalAttestationError::InvalidIdentifier {
                field: "definer role must differ from runtime login",
            });
        }
        Ok(Self {
            lineage,
            postgres,
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

/// Install the restricted function through an owned fresh connection. A
/// failed COMMIT is reported as unknown and never yields an installed handle.
/// A second physical connection proves the committed function and lineage are
/// visible before this method returns.
pub async fn install(
    admin_options: &PgConnectOptions,
    config: PhysicalAttestationConfig,
) -> Result<PhysicalAttestationConfig, PhysicalAttestationError> {
    let mut admin = PgConnection::connect_with(admin_options).await?;
    // Reject a misplaced admin DSN before creating a role or schema there.
    // The post-COMMIT check below is still required: this only checks the
    // owned installer session's starting boundary.
    verify_active_schema(&mut admin, &config).await?;
    let preflight = read_preflight_snapshot(&mut admin, &config).await?;
    verify_identity(&preflight, &config)?;
    let mut transaction = admin.begin().await?;
    let definer = quote_identifier(config.definer_role.as_str());
    let runtime = quote_identifier(config.runtime_login.as_str());
    let trusted = quote_identifier(&config.trusted_schema.0);
    let target = quote_identifier(&config.postgres().schema.name.0);
    let function = config.function_path();

    let statements = [
        format!(
            "CREATE ROLE {definer} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"
        ),
        format!("CREATE SCHEMA {trusted} AUTHORIZATION {definer}"),
        format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {definer}"),
        format!("GRANT USAGE ON SCHEMA {target} TO {definer}"),
        format!("GRANT SELECT ON {target}._lineage TO {definer}"),
    ];
    for statement in statements {
        sqlx::query(&statement).execute(&mut *transaction).await?;
    }

    // The target schema name is validated before interpolation. Every table
    // and catalog reference is qualified; pg_temp is last in the definer's
    // search path so temp objects cannot shadow authority objects.
    // The installed function independently refuses unsupported lineage
    // formats, even when called outside the pool hook.
    let identity_sql = format!(
        "{} AND l.format = {LINEAGE_FORMAT}",
        identity_select(&config)
    );
    let function_sql = format!(
        r#"CREATE FUNCTION {function}()
RETURNS TABLE (
    lineage_uuid text, deployment_uuid text,
    stored_system_identifier text, stored_database_oid text, stored_database_name text,
    stored_schema_oid text, stored_schema_name text,
    live_system_identifier text, live_database_oid text, live_database_name text,
    live_schema_oid text, live_schema_name text,
    stored_lineage_format bigint
)
LANGUAGE sql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $waddle_attestation$
    {identity_sql}
$waddle_attestation$"#,
    );
    sqlx::query(&function_sql)
        .execute(&mut *transaction)
        .await?;
    for statement in [
        format!("REVOKE ALL ON FUNCTION {function}() FROM PUBLIC"),
        format!("ALTER FUNCTION {function}() OWNER TO {definer}"),
        format!("GRANT USAGE ON SCHEMA {trusted} TO {runtime}"),
        format!("GRANT EXECUTE ON FUNCTION {function}() TO {runtime}"),
    ] {
        sqlx::query(&statement).execute(&mut *transaction).await?;
    }
    // Inherited default grants can add unexpected principals to a new
    // function. Check effective ACLs while all installer DDL can still roll
    // back, then repeat the same checks on the committed object below.
    verify_installed_roles(&mut transaction, &config).await?;
    transaction
        .commit()
        .await
        .map_err(PhysicalAttestationError::CommitUnknown)?;
    drop(admin);

    verify_post_commit(admin_options, &config)
        .await
        .map_err(|source| PhysicalAttestationError::PostCommitUnverified(Box::new(source)))?;
    Ok(config)
}

async fn verify_post_commit(
    admin_options: &PgConnectOptions,
    config: &PhysicalAttestationConfig,
) -> Result<(), PhysicalAttestationError> {
    let mut readback = PgConnection::connect_with(admin_options).await?;
    verify_active_schema(&mut readback, config).await?;
    let snapshot = read_snapshot(&mut readback, config).await?;
    verify_identity(&snapshot, config)?;
    verify_installed_roles(&mut readback, config).await?;
    Ok(())
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

async fn verify_installed_roles(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
) -> Result<(), PhysicalAttestationError> {
    let row = sqlx::query(
        "SELECT r.rolcanlogin, r.rolsuper, r.rolcreatedb, r.rolcreaterole, \
         r.rolreplication, r.rolbypassrls, pg_catalog.pg_get_userbyid(p.proowner), \
         pg_catalog.pg_get_userbyid(n.nspowner), p.prosecdef, \
         p.proconfig @> ARRAY['search_path=pg_catalog, pg_temp']::text[], \
         NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                         COALESCE(p.proacl, pg_catalog.acldefault('f', p.proowner))) a \
                     WHERE a.grantee = 0 AND a.privilege_type = 'EXECUTE'), \
         pg_catalog.has_function_privilege($4, p.oid, 'EXECUTE'), \
         pg_catalog.has_schema_privilege($4, n.oid, 'USAGE'), \
         pg_catalog.has_function_privilege($1, 'pg_catalog.pg_control_system()'::pg_catalog.regprocedure, 'EXECUTE'), \
         NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                         COALESCE(p.proacl, pg_catalog.acldefault('f', p.proowner))) a \
                     WHERE a.grantee NOT IN (p.proowner, runtime.oid) \
                        OR (a.grantee = runtime.oid AND a.is_grantable)), \
         NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                         COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))) a \
                     WHERE a.grantee NOT IN (n.nspowner, runtime.oid) \
                        OR (a.grantee = runtime.oid AND \
                            (a.privilege_type <> 'USAGE' OR a.is_grantable))), \
         NOT pg_catalog.has_schema_privilege($4, n.oid, 'CREATE') \
         FROM pg_catalog.pg_roles r \
         JOIN pg_catalog.pg_roles runtime ON runtime.rolname = $4 \
         JOIN pg_catalog.pg_namespace n ON n.nspname = $2 \
         JOIN pg_catalog.pg_proc p ON p.pronamespace = n.oid AND p.proname = $3 \
         WHERE r.rolname = $1 AND p.pronargs = 0",
    )
    .bind(config.definer_role.as_str())
    .bind(&config.trusted_schema.0)
    .bind(FUNCTION_NAME)
    .bind(config.runtime_login.as_str())
    .fetch_optional(&mut *connection)
    .await?
    .ok_or(PhysicalAttestationError::Mismatch {
        field: "installed definer/function",
    })?;
    for index in 0..6 {
        let enabled: bool = row.try_get(index)?;
        if enabled {
            return Err(PhysicalAttestationError::Mismatch {
                field: "definer privileges",
            });
        }
    }
    let owner: String = row.try_get(6)?;
    if owner != config.definer_role.as_str() {
        return Err(PhysicalAttestationError::Mismatch {
            field: "function owner",
        });
    }
    let schema_owner: String = row.try_get(7)?;
    if schema_owner != config.definer_role.as_str() {
        return Err(PhysicalAttestationError::Mismatch {
            field: "trusted schema owner",
        });
    }
    for (index, field) in [
        (8, "SECURITY DEFINER"),
        (9, "function search_path"),
        (10, "PUBLIC EXECUTE revoke"),
        (11, "runtime EXECUTE grant"),
        (12, "runtime schema USAGE grant"),
        (13, "definer pg_control_system EXECUTE grant"),
        (14, "function ACL grantees"),
        (15, "trusted schema ACL grantees"),
        (16, "runtime trusted schema CREATE denied"),
    ] {
        let valid: bool = row.try_get(index)?;
        if !valid {
            return Err(PhysicalAttestationError::Mismatch { field });
        }
    }
    Ok(())
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

async fn verify_active_schema(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
) -> Result<(), PhysicalAttestationError> {
    let active: Option<String> = sqlx::query_scalar("SELECT pg_catalog.current_schema()::text")
        .fetch_one(connection)
        .await?;
    if active.as_deref() != Some(&config.postgres().schema.name.0) {
        return Err(PhysicalAttestationError::Mismatch {
            field: "active schema",
        });
    }
    Ok(())
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
/// SQLx may retry a failed hook until the acquire deadline, so callers must
/// treat a timeout as unavailable, not as a definitive identity diagnosis.
pub fn attested_pool(
    options: PgConnectOptions,
    config: PhysicalAttestationConfig,
    max_connections: u32,
    acquire_timeout: Duration,
) -> Result<PgPool, PhysicalAttestationError> {
    if options.get_username() != config.runtime_login.as_str() {
        return Err(PhysicalAttestationError::WrongConfiguredLogin);
    }
    let on_connect = config.clone();
    Ok(PgPoolOptions::new()
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
            let config = config.clone();
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
        .connect_lazy_with(options))
}
