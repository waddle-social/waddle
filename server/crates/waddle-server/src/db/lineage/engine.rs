use crate::{
    config::LineageConfig,
    db::{Database, DatabaseDriver, DatabaseError, Row, Transaction},
};

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
pub async fn ensure_table(db: &Database) -> Result<(), DatabaseError> {
    let driver = db.driver();
    let mut tx = db.begin().await?;
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

/// Rotate an existing lineage record after an operator-verified clone or restore.
pub async fn adopt(
    db: &Database,
    config: &LineageConfig,
    expected: LineageUuid,
) -> Result<AttestedLineage, DatabaseError> {
    let deployment_uuid = required_deployment_uuid(config)?;
    let driver = db.driver();
    let mut tx = begin_locked(db).await?;
    ensure_table_in_transaction(&mut tx, driver).await?;

    let record = read_record(&mut tx, driver).await?;
    let found = record.as_ref().map(|row| row.lineage_uuid);
    if found != Some(expected) {
        return Err(LineageError::AdoptExpectationFailed { expected, found }.into());
    }

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

    Ok(AttestedLineage {
        lineage_uuid,
        deployment_uuid,
        postgres_identity,
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

    let expected = LINEAGE_COLUMNS.map(str::to_string);
    if found != expected {
        return Err(LineageError::MalformedTable {
            detail: format!("expected columns {expected:?}, found {found:?}"),
        }
        .into());
    }
    Ok(())
}

async fn verify_in_transaction(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    config: &LineageConfig,
) -> Result<AttestedLineage, DatabaseError> {
    match driver {
        DatabaseDriver::Sqlite => {
            let record = read_record(tx, driver)
                .await?
                .ok_or(LineageError::MissingRow)?;
            verify_deployment_uuid(&record, config)?;
            Ok(AttestedLineage {
                lineage_uuid: record.lineage_uuid,
                deployment_uuid: record.deployment_uuid,
                postgres_identity: None,
            })
        }
        DatabaseDriver::Postgres => {
            let mut rows = match tx.query(READ_POSTGRES_ROW_WITH_LIVE_IDENTITY_SQL, ()).await {
                Ok(rows) => rows,
                Err(error) if is_system_identifier_permission_error(&error) => {
                    return Err(LineageError::SystemIdentifierUnavailable.into());
                }
                Err(error) => return Err(error),
            };
            let row = rows.next().await?.ok_or(LineageError::MissingRow)?;
            let record = record_from_row(&row, driver)?;
            verify_deployment_uuid(&record, config)?;
            let live = postgres_identity_from_row(&row, 8)?;
            let persisted =
                record
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
    }
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
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
) -> Result<Option<LineageRecord>, DatabaseError> {
    let mut rows = tx.query(READ_ROW_SQL, ()).await?;
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
            name: database_name,
        },
        schema: PgSchemaIdentity {
            oid: parse_oid(&schema_oid, "pg_schema_oid")?,
            name: schema_name,
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

async fn read_live_identity(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
) -> Result<Option<PgIdentity>, DatabaseError> {
    if driver == DatabaseDriver::Sqlite {
        return Ok(None);
    }
    let mut rows = match tx.query(READ_POSTGRES_LIVE_IDENTITY_SQL, ()).await {
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
    Ok(Some(postgres_identity_from_row(&row, 0)?))
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
        identity.map(|value| value.database.name.clone()),
        identity.map(|value| value.schema.oid.to_string()),
        identity.map(|value| value.schema.name.clone()),
    ]
}

fn is_system_identifier_permission_error(error: &DatabaseError) -> bool {
    match error {
        DatabaseError::Internal(sqlx::Error::Database(database_error)) => {
            database_error.code().as_deref() == Some("42501")
        }
        _ => false,
    }
}
