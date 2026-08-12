use crate::{
    config::LineageConfig,
    db::{ConnectionGuard, Database, DatabaseDriver, DatabaseError, Row, Rows, Transaction, Value},
};
use sqlx::{postgres::PgPool, sqlite::SqlitePool, Row as SqlxRow};

use super::{
    sql::{
        lineage_table_sql, LINEAGE_COLUMNS, LINEAGE_FORMAT, READ_POSTGRES_LIVE_IDENTITY_SQL,
        READ_POSTGRES_ROW_WITH_LIVE_IDENTITY_SQL, READ_ROW_SQL,
    },
    DeploymentUuid, LineageError, LineageRecord, LineageUuid, PgDatabaseIdentity, PgIdentity,
    PgSchemaIdentity,
};

/// Dedicated PostgreSQL advisory lock for serializing lineage enrollment and adoption.
pub const LINEAGE_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_993;

/// A lineage record successfully attested against the database currently serving the query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestedLineage {
    pub lineage_uuid: LineageUuid,
    pub deployment_uuid: DeploymentUuid,
    pub postgres_identity: Option<PgIdentity>,
}

/// Create and validate the lineage bootstrap table without writing its row.
///
/// Takes the lineage advisory lock (SQLite: immediate write lock) for the
/// same reason the migration runner locks its own bootstrap DDL: Postgres's
/// `CREATE TABLE IF NOT EXISTS` is not race-free, and two replicas starting
/// against a fresh schema would otherwise crash one of them with a spurious
/// `pg_type` unique-constraint violation.
pub async fn ensure_table(db: &Database) -> Result<(), DatabaseError> {
    let driver = db.driver();
    let mut tx = begin_locked(db).await?;
    ensure_table_in_transaction(&mut tx, driver).await?;
    tx.commit().await
}

/// Enroll this durable database boundary, or attest the existing enrollment without modifying it.
pub async fn enroll(
    db: &Database,
    config: &LineageConfig,
) -> Result<AttestedLineage, DatabaseError> {
    let deployment_uuid = required_deployment_uuid(config)?;
    let driver = db.driver();
    let mut tx = begin_locked(db).await?;
    ensure_table_in_transaction(&mut tx, driver).await?;
    ensure_single_row(&mut tx).await?;

    let result = match read_record(&mut tx, driver).await? {
        Some(_) => verify_in_transaction(&mut tx, driver, config).await?,
        None => {
            let lineage_uuid = LineageUuid::new();
            let postgres_identity = read_live_identity(&mut tx, driver).await?;
            insert_record(
                &mut tx,
                LineageRecord {
                    format: LINEAGE_FORMAT,
                    lineage_uuid,
                    deployment_uuid,
                    postgres_identity: postgres_identity.clone(),
                },
            )
            .await?;
            AttestedLineage {
                lineage_uuid,
                deployment_uuid,
                postgres_identity,
            }
        }
    };

    tx.commit().await?;
    Ok(result)
}

/// Outcome of [`adopt_if_matched`] for one durable boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptOutcome {
    Adopted {
        /// Which `adopt=` list entry this boundary's row held.
        matched: LineageUuid,
        attested: AttestedLineage,
    },
    /// This boundary's row holds none of the expected lineage UUIDs (or has
    /// no row). No write happened; the boundary proceeds to normal verify.
    NotMatched,
}

/// Boundary-safe adoption: `WADDLE_DB_LINEAGE_ACTION=adopt=<uuid>` is one
/// process-wide value applied at every durable boundary, but the expected
/// UUID can only ever name ONE boundary's row — and several stores usually
/// share a single database (and therefore a single row). Rotating on the
/// first match and erroring on the rest would rotate the shared row and then
/// crash the process against its own rotation. So adoption rotates exactly
/// where the row matches and is a read-only no-op everywhere else; the
/// startup gate separately fails attestation when the action matched no
/// boundary at all (an operator typo must stay loud).
pub async fn adopt_if_matched(
    db: &Database,
    config: &LineageConfig,
    expected: &[LineageUuid],
) -> Result<AdoptOutcome, DatabaseError> {
    let deployment_uuid = required_deployment_uuid(config)?;
    let driver = db.driver();
    let mut tx = begin_locked(db).await?;
    ensure_table_in_transaction(&mut tx, driver).await?;
    ensure_single_row(&mut tx).await?;

    let record = read_record(&mut tx, driver).await?;
    let matched = record
        .as_ref()
        .map(|row| row.lineage_uuid)
        .filter(|current| expected.contains(current));
    let Some(matched) = matched else {
        tx.commit().await?;
        return Ok(AdoptOutcome::NotMatched);
    };

    let lineage_uuid = LineageUuid::new();
    let postgres_identity = read_live_identity(&mut tx, driver).await?;
    update_record(
        &mut tx,
        LineageRecord {
            format: LINEAGE_FORMAT,
            lineage_uuid,
            deployment_uuid,
            postgres_identity: postgres_identity.clone(),
        },
    )
    .await?;
    tx.commit().await?;

    Ok(AdoptOutcome::Adopted {
        matched,
        attested: AttestedLineage {
            lineage_uuid,
            deployment_uuid,
            postgres_identity,
        },
    })
}

/// Verify the persisted lineage record without writing to the database.
pub async fn verify(
    db: &Database,
    config: &LineageConfig,
) -> Result<AttestedLineage, DatabaseError> {
    let driver = db.driver();
    let mut tx = db.begin().await?;
    let result = verify_in_transaction(&mut tx, driver, config).await?;
    tx.commit().await?;
    Ok(result)
}

/// Verify through the dedicated control-plane pool. The verification SQL and
/// comparison logic are shared with [`verify`]; only the physical pool that
/// supplies the connection differs.
pub async fn verify_via_control_plane(
    db: &Database,
    config: &LineageConfig,
) -> Result<AttestedLineage, DatabaseError> {
    let mut guard = db.control_plane_guard().await?;
    verify_via_query(&mut guard, db.driver(), config).await
}

/// Verify through a raw sqlx SQLite pool that owns the physical connection.
pub async fn verify_via_sqlite_pool(
    pool: &SqlitePool,
    config: &LineageConfig,
) -> Result<AttestedLineage, DatabaseError> {
    ensure_single_row_sqlite(pool).await?;
    let row = sqlx::query(READ_ROW_SQL)
        .fetch_optional(pool)
        .await
        .map_err(DatabaseError::from)?;
    let row = row.ok_or(LineageError::MissingRow)?;
    let format: i64 = row.try_get(0).map_err(DatabaseError::from)?;
    if format != LINEAGE_FORMAT {
        return Err(LineageError::UnknownFormat { found: format }.into());
    }
    let lineage_uuid = row
        .try_get::<String, _>(1)
        .map_err(DatabaseError::from)?
        .parse::<LineageUuid>()?;
    let deployment_uuid = row
        .try_get::<String, _>(2)
        .map_err(DatabaseError::from)?
        .parse::<DeploymentUuid>()?;
    verify_deployment_uuid(
        &LineageRecord {
            format,
            lineage_uuid,
            deployment_uuid,
            postgres_identity: None,
        },
        config,
    )?;
    Ok(AttestedLineage {
        lineage_uuid,
        deployment_uuid,
        postgres_identity: None,
    })
}

async fn ensure_single_row_pg(pool: &PgPool) -> Result<(), DatabaseError> {
    let row = sqlx::query(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN id = 1 THEN 0 ELSE 1 END), 0) FROM _lineage",
    )
    .fetch_one(pool)
    .await
    .map_err(DatabaseError::from)?;
    let count: i64 = row.try_get(0).map_err(DatabaseError::from)?;
    let stray: i64 = row.try_get(1).map_err(DatabaseError::from)?;
    ensure_singleton_shape(count, stray)
}

async fn ensure_single_row_sqlite(pool: &SqlitePool) -> Result<(), DatabaseError> {
    let row = sqlx::query(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN id = 1 THEN 0 ELSE 1 END), 0) FROM _lineage",
    )
    .fetch_one(pool)
    .await
    .map_err(DatabaseError::from)?;
    let count: i64 = row.try_get(0).map_err(DatabaseError::from)?;
    let stray: i64 = row.try_get(1).map_err(DatabaseError::from)?;
    ensure_singleton_shape(count, stray)
}

/// Verify through a raw sqlx Postgres pool that owns the physical connection.
pub async fn verify_via_pg_pool(
    pool: &PgPool,
    config: &LineageConfig,
) -> Result<AttestedLineage, DatabaseError> {
    ensure_single_row_pg(pool).await?;
    let row = sqlx::query(READ_POSTGRES_ROW_WITH_LIVE_IDENTITY_SQL)
        .fetch_optional(pool)
        .await
        .map_err(map_postgres_identity_error)?;
    let row = row.ok_or(LineageError::MissingRow)?;
    // `format` is `INTEGER` (int4) on Postgres; sqlx decodes it strictly, so
    // read i32 here and widen — unlike SQLite, where INTEGER decodes as i64.
    let format: i64 = row
        .try_get::<i32, _>(0)
        .map(i64::from)
        .map_err(DatabaseError::from)?;
    if format != LINEAGE_FORMAT {
        return Err(LineageError::UnknownFormat { found: format }.into());
    }
    let lineage_uuid = row
        .try_get::<String, _>(1)
        .map_err(DatabaseError::from)?
        .parse::<LineageUuid>()?;
    let deployment_uuid = row
        .try_get::<String, _>(2)
        .map_err(DatabaseError::from)?
        .parse::<DeploymentUuid>()?;
    let persisted = postgres_identity_from_sqlx_row(&row, 3)?;
    let live = postgres_identity_from_sqlx_row(&row, 8)?;
    verify_deployment_uuid(
        &LineageRecord {
            format,
            lineage_uuid,
            deployment_uuid,
            postgres_identity: Some(persisted.clone()),
        },
        config,
    )?;
    verify_postgres_identity(&persisted, &live)?;
    Ok(AttestedLineage {
        lineage_uuid,
        deployment_uuid,
        postgres_identity: Some(live),
    })
}

/// Read the live PostgreSQL identity through a [`Database`] handle without
/// consulting the persisted lineage row.
pub async fn live_postgres_identity(db: &Database) -> Result<PgIdentity, DatabaseError> {
    let mut tx = db.begin().await?;
    let identity = read_live_postgres_identity(&mut tx, db.driver()).await?;
    tx.commit().await?;
    Ok(identity)
}

/// Determine whether a raw SQLite pool is backed by SQLite's process-local
/// in-memory store.
pub async fn sqlite_pool_is_in_memory(pool: &SqlitePool) -> Result<bool, DatabaseError> {
    let row = sqlx::query("PRAGMA database_list")
        .fetch_one(pool)
        .await
        .map_err(DatabaseError::from)?;
    let filename: String = row.try_get(2).map_err(DatabaseError::from)?;
    Ok(matches!(filename.as_str(), "" | ":memory:"))
}

/// Read the live PostgreSQL identity through a raw sqlx Postgres pool.
pub async fn live_postgres_identity_via_pg_pool(
    pool: &PgPool,
) -> Result<PgIdentity, DatabaseError> {
    let row = sqlx::query(READ_POSTGRES_LIVE_IDENTITY_SQL)
        .fetch_one(pool)
        .await
        .map_err(map_postgres_identity_error)?;
    postgres_identity_from_sqlx_row(&row, 0)
}

async fn begin_locked(db: &Database) -> Result<Transaction<'_>, DatabaseError> {
    match db.driver() {
        DatabaseDriver::Postgres => {
            let mut tx = db.begin().await?;
            tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
                .await?;
            tx.query(
                "SELECT pg_advisory_xact_lock(?)",
                crate::db_params![LINEAGE_ADVISORY_LOCK_KEY],
            )
            .await?;
            Ok(tx)
        }
        DatabaseDriver::Sqlite => db.begin_immediate().await,
    }
}

async fn ensure_table_in_transaction(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
) -> Result<(), DatabaseError> {
    tx.execute(lineage_table_sql(driver), ()).await?;
    validate_column_set(tx, driver).await
}

async fn validate_column_set(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
) -> Result<(), DatabaseError> {
    let mut rows = match driver {
        DatabaseDriver::Sqlite => tx.query("PRAGMA table_info('_lineage')", ()).await?,
        DatabaseDriver::Postgres => {
            tx.query(
                "SELECT column_name FROM information_schema.columns \
                 WHERE table_schema = current_schema() AND table_name = ? \
                 ORDER BY ordinal_position",
                crate::db_params!["_lineage"],
            )
            .await?
        }
    };

    let mut found = Vec::new();
    while let Some(row) = rows.next().await? {
        let name: String = match driver {
            DatabaseDriver::Sqlite => row.get(1)?,
            DatabaseDriver::Postgres => row.get(0)?,
        };
        found.push(name);
    }

    // Require format-1's columns as a SUBSET of what exists rather than an
    // exact match: `_lineage` versions itself via `format` (it lives outside
    // the migration ledger), so a future additive format must not make this
    // binary refuse the table before it can even read `format` and produce
    // the typed `UnknownFormat` refusal.
    let missing: Vec<&str> = LINEAGE_COLUMNS
        .iter()
        .filter(|expected| !found.iter().any(|name| name == *expected))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(LineageError::MalformedTable {
            detail: format!("missing columns {missing:?}, found {found:?}"),
        }
        .into());
    }
    Ok(())
}

/// Verify lineage while preserving the caller's existing transaction boundary.
///
/// Callers that compose durable writes with lineage attestation must invoke
/// this before their writes and let the transaction drop on an error.
pub(crate) async fn verify_in_transaction(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    config: &LineageConfig,
) -> Result<AttestedLineage, DatabaseError> {
    verify_via_query(tx, driver, config).await
}

#[async_trait::async_trait]
trait LineageQuery {
    async fn lineage_query(&mut self, sql: &str, params: Vec<Value>)
        -> Result<Rows, DatabaseError>;
}

#[async_trait::async_trait]
impl LineageQuery for Transaction<'_> {
    async fn lineage_query(
        &mut self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<Rows, DatabaseError> {
        self.query(sql, params).await
    }
}

#[async_trait::async_trait]
impl LineageQuery for ConnectionGuard {
    async fn lineage_query(
        &mut self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<Rows, DatabaseError> {
        self.query(sql, params).await
    }
}

/// A `_lineage` table that predates this binary (no PK / no `CHECK (id = 1)`)
/// could hold several rows, or a single row whose id is not 1 — the
/// `WHERE id = 1` reads would miss the latter and enrollment would then
/// write a SECOND row into a table already deemed singleton. Both shapes
/// are typed malformed-table refusals.
async fn ensure_single_row(query: &mut impl LineageQuery) -> Result<(), DatabaseError> {
    let mut rows = query
        .lineage_query(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN id = 1 THEN 0 ELSE 1 END), 0) FROM _lineage",
            Vec::new(),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LineageError::MalformedTable {
            detail: "lineage row count query returned no row".to_string(),
        })?;
    let count: i64 = row.get(0)?;
    let stray: i64 = row.get(1)?;
    ensure_singleton_shape(count, stray)
}

fn ensure_singleton_shape(count: i64, stray: i64) -> Result<(), DatabaseError> {
    if count > 1 {
        return Err(LineageError::MalformedTable {
            detail: format!("expected at most one lineage row, found {count}"),
        }
        .into());
    }
    if stray > 0 {
        return Err(LineageError::MalformedTable {
            detail: "lineage row exists whose id is not 1 (including NULL)".to_string(),
        }
        .into());
    }
    Ok(())
}

async fn verify_via_query(
    query: &mut impl LineageQuery,
    driver: DatabaseDriver,
    config: &LineageConfig,
) -> Result<AttestedLineage, DatabaseError> {
    ensure_single_row(query).await?;
    match driver {
        DatabaseDriver::Sqlite => {
            let record = read_record(query, driver)
                .await?
                .ok_or(LineageError::MissingRow)?;
            attested_sqlite_record(record, config)
        }
        DatabaseDriver::Postgres => {
            let mut rows = match query
                .lineage_query(READ_POSTGRES_ROW_WITH_LIVE_IDENTITY_SQL, Vec::new())
                .await
            {
                Ok(rows) => rows,
                Err(error) if is_system_identifier_permission_error(&error) => {
                    return Err(LineageError::SystemIdentifierUnavailable.into());
                }
                Err(error) => return Err(error),
            };
            let row = rows.next().await?.ok_or(LineageError::MissingRow)?;
            let record = record_from_row(&row, driver)?;
            let live = postgres_identity_from_row(&row, 8)?;
            attested_postgres_record(record, live, config)
        }
    }
}

fn attested_sqlite_record(
    record: LineageRecord,
    config: &LineageConfig,
) -> Result<AttestedLineage, DatabaseError> {
    verify_deployment_uuid(&record, config)?;
    Ok(AttestedLineage {
        lineage_uuid: record.lineage_uuid,
        deployment_uuid: record.deployment_uuid,
        postgres_identity: None,
    })
}

fn attested_postgres_record(
    record: LineageRecord,
    live: PgIdentity,
    config: &LineageConfig,
) -> Result<AttestedLineage, DatabaseError> {
    verify_deployment_uuid(&record, config)?;
    let persisted = record
        .postgres_identity
        .ok_or_else(|| LineageError::MalformedTable {
            detail: "PostgreSQL lineage row has NULL identity fields".to_string(),
        })?;
    verify_postgres_identity(&persisted, &live)?;
    Ok(AttestedLineage {
        lineage_uuid: record.lineage_uuid,
        deployment_uuid: record.deployment_uuid,
        postgres_identity: Some(live),
    })
}

async fn read_live_postgres_identity(
    query: &mut impl LineageQuery,
    driver: DatabaseDriver,
) -> Result<PgIdentity, DatabaseError> {
    if driver != DatabaseDriver::Postgres {
        return Err(LineageError::MalformedTable {
            detail: "PostgreSQL live identity requested for a non-Postgres database".to_string(),
        }
        .into());
    }
    let mut rows = match query
        .lineage_query(READ_POSTGRES_LIVE_IDENTITY_SQL, Vec::new())
        .await
    {
        Ok(rows) => rows,
        Err(error) if is_system_identifier_permission_error(&error) => {
            return Err(LineageError::SystemIdentifierUnavailable.into());
        }
        Err(error) => return Err(error),
    };
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LineageError::MalformedTable {
            detail: "PostgreSQL live identity query returned no row".to_string(),
        })?;
    postgres_identity_from_row(&row, 0)
}

fn verify_deployment_uuid(
    record: &LineageRecord,
    config: &LineageConfig,
) -> Result<(), LineageError> {
    match config.deployment_uuid {
        Some(configured) if configured == record.deployment_uuid => Ok(()),
        Some(configured) => Err(LineageError::DeploymentUuidMismatch {
            configured,
            persisted: record.deployment_uuid,
        }),
        None => Err(LineageError::DeploymentUuidUnconfigured {
            persisted: record.deployment_uuid,
        }),
    }
}

fn verify_postgres_identity(persisted: &PgIdentity, live: &PgIdentity) -> Result<(), LineageError> {
    if persisted.system_identifier != live.system_identifier {
        return Err(LineageError::SystemIdentifierMismatch {
            live: live.system_identifier,
            persisted: persisted.system_identifier,
        });
    }
    if persisted.database != live.database {
        return Err(LineageError::DatabaseIdentityMismatch {
            live: live.database.clone(),
            persisted: persisted.database.clone(),
        });
    }
    if persisted.schema != live.schema {
        return Err(LineageError::SchemaIdentityMismatch {
            live: live.schema.clone(),
            persisted: persisted.schema.clone(),
        });
    }
    Ok(())
}

fn required_deployment_uuid(config: &LineageConfig) -> Result<DeploymentUuid, DatabaseError> {
    config
        .deployment_uuid
        .ok_or(LineageError::DeploymentUuidRequired.into())
}

async fn read_record(
    query: &mut impl LineageQuery,
    driver: DatabaseDriver,
) -> Result<Option<LineageRecord>, DatabaseError> {
    let mut rows = query.lineage_query(READ_ROW_SQL, Vec::new()).await?;
    match rows.next().await? {
        Some(row) => Ok(Some(record_from_row(&row, driver)?)),
        None => Ok(None),
    }
}

fn record_from_row(row: &Row, driver: DatabaseDriver) -> Result<LineageRecord, DatabaseError> {
    let format: i64 = row.get(0)?;
    if format != LINEAGE_FORMAT {
        return Err(LineageError::UnknownFormat { found: format }.into());
    }
    let lineage_uuid: String = row.get(1)?;
    let deployment_uuid: String = row.get(2)?;
    let lineage_uuid = lineage_uuid.parse()?;
    let deployment_uuid = parse_deployment_uuid(&deployment_uuid)?;
    let postgres_identity = match driver {
        DatabaseDriver::Sqlite => None,
        DatabaseDriver::Postgres => Some(postgres_identity_from_row(row, 3)?),
    };
    Ok(LineageRecord {
        format,
        lineage_uuid,
        deployment_uuid,
        postgres_identity,
    })
}

fn parse_deployment_uuid(value: &str) -> Result<DeploymentUuid, DatabaseError> {
    value.parse::<DeploymentUuid>().map_err(DatabaseError::from)
}

fn postgres_identity_from_row(row: &Row, offset: usize) -> Result<PgIdentity, DatabaseError> {
    let system_identifier: Option<String> = row.get(offset)?;
    let database_oid: Option<String> = row.get(offset + 1)?;
    let database_name: Option<String> = row.get(offset + 2)?;
    let schema_oid: Option<String> = row.get(offset + 3)?;
    let schema_name: Option<String> = row.get(offset + 4)?;

    let system_identifier = required_identity_value(system_identifier, "pg_system_identifier")?;
    let database_oid = required_identity_value(database_oid, "pg_database_oid")?;
    let database_name = required_identity_value(database_name, "pg_database_name")?;
    let schema_oid = required_identity_value(schema_oid, "pg_schema_oid")?;
    let schema_name = required_identity_value(schema_name, "pg_schema_name")?;

    Ok(PgIdentity {
        system_identifier: system_identifier.parse()?,
        database: PgDatabaseIdentity {
            oid: parse_oid(&database_oid, "pg_database_oid")?,
            name: super::PgDatabaseName(database_name),
        },
        schema: PgSchemaIdentity {
            oid: parse_oid(&schema_oid, "pg_schema_oid")?,
            name: super::PgSchemaName(schema_name),
        },
    })
}

fn required_identity_value(
    value: Option<String>,
    field: &'static str,
) -> Result<String, DatabaseError> {
    value.ok_or_else(|| {
        LineageError::MalformedTable {
            detail: format!("PostgreSQL lineage row has NULL {field}"),
        }
        .into()
    })
}

fn parse_oid(value: &str, field: &'static str) -> Result<u32, DatabaseError> {
    value.parse().map_err(|_| {
        LineageError::InvalidPostgresIdentity {
            field,
            value: value.to_string(),
        }
        .into()
    })
}

fn postgres_identity_from_sqlx_row(
    row: &sqlx::postgres::PgRow,
    offset: usize,
) -> Result<PgIdentity, DatabaseError> {
    let system_identifier = row
        .try_get::<String, _>(offset)
        .map_err(map_postgres_identity_error)?;
    let database_oid = row
        .try_get::<String, _>(offset + 1)
        .map_err(map_postgres_identity_error)?;
    let database_name = row
        .try_get::<String, _>(offset + 2)
        .map_err(map_postgres_identity_error)?;
    let schema_oid = row
        .try_get::<String, _>(offset + 3)
        .map_err(map_postgres_identity_error)?;
    let schema_name = row
        .try_get::<String, _>(offset + 4)
        .map_err(map_postgres_identity_error)?;

    Ok(PgIdentity {
        system_identifier: system_identifier.parse()?,
        database: PgDatabaseIdentity {
            oid: parse_oid(&database_oid, "pg_database_oid")?,
            name: super::PgDatabaseName(database_name),
        },
        schema: PgSchemaIdentity {
            oid: parse_oid(&schema_oid, "pg_schema_oid")?,
            name: super::PgSchemaName(schema_name),
        },
    })
}

fn map_postgres_identity_error(error: sqlx::Error) -> DatabaseError {
    let db_error = DatabaseError::from(error);
    if is_system_identifier_permission_error(&db_error) {
        LineageError::SystemIdentifierUnavailable.into()
    } else {
        db_error
    }
}

async fn read_live_identity(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
) -> Result<Option<PgIdentity>, DatabaseError> {
    if driver == DatabaseDriver::Sqlite {
        return Ok(None);
    }
    Ok(Some(read_live_postgres_identity(tx, driver).await?))
}

async fn insert_record(
    tx: &mut Transaction<'_>,
    record: LineageRecord,
) -> Result<(), DatabaseError> {
    let values = record_values(&record);
    tx.execute(
        "INSERT INTO _lineage (id, format, lineage_uuid, deployment_uuid, pg_system_identifier, \
         pg_database_oid, pg_database_name, pg_schema_oid, pg_schema_name) \
         VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?)",
        values,
    )
    .await?;
    Ok(())
}

async fn update_record(
    tx: &mut Transaction<'_>,
    record: LineageRecord,
) -> Result<(), DatabaseError> {
    let values = record_values(&record);
    tx.execute(
        "UPDATE _lineage SET format = ?, lineage_uuid = ?, deployment_uuid = ?, \
         pg_system_identifier = ?, pg_database_oid = ?, pg_database_name = ?, \
         pg_schema_oid = ?, pg_schema_name = ?, stamped_at = CURRENT_TIMESTAMP WHERE id = 1",
        values,
    )
    .await?;
    Ok(())
}

fn record_values(record: &LineageRecord) -> Vec<crate::db::Value> {
    let identity = record.postgres_identity.as_ref();
    crate::db_params![
        record.format,
        record.lineage_uuid.to_string(),
        record.deployment_uuid.to_string(),
        identity.map(|value| value.system_identifier.to_string()),
        identity.map(|value| value.database.oid.to_string()),
        identity.map(|value| value.database.name.0.clone()),
        identity.map(|value| value.schema.oid.to_string()),
        identity.map(|value| value.schema.name.0.clone()),
    ]
}

/// A `42501` alone is not enough: the combined verify query also reads
/// `_lineage`, so a role missing `SELECT` on the table raises the same
/// SQLSTATE. Only a denial that names `pg_control_system` gets the typed
/// grant-EXECUTE remediation; any other permission failure stays a plain
/// database error so the operator chases the right ACL.
fn is_system_identifier_permission_error(error: &DatabaseError) -> bool {
    match error {
        DatabaseError::Internal(sqlx::Error::Database(database_error)) => {
            database_error.code().as_deref() == Some("42501")
                && database_error.message().contains("pg_control_system")
        }
        _ => false,
    }
}
