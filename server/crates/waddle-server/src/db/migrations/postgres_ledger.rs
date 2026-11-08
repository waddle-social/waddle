//! Ledger-local checks for the ordinary PostgreSQL migration runner.
//!
//! The catalog fence coordinates participating mutators only. Privileged
//! nonparticipants, including role, ownership and event-trigger changes, must
//! be quiescent. Observed object OIDs are used for revalidation, not as an
//! independently provisioned identity or a serving authorization.

use sqlx::postgres::types::Oid;
use sqlx::PgConnection;

use super::MigrationLedgerError;
use crate::db::read_only_timeout::timeout_millis;
use crate::db::{DatabaseError, Transaction};

const LOCK_TIMEOUT_MS: u64 = 5_000;
const STATEMENT_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, PartialEq, Eq)]
struct Schema {
    name: String,
    oid: Oid,
    owner: Oid,
}

#[derive(Debug, PartialEq, Eq)]
struct Ledger {
    oid: Oid,
    owner: Oid,
    index: Oid,
    has_checksum: bool,
}

pub(super) struct PreparedLedger {
    pub(super) table: String,
    pub(super) checksum_column_added: bool,
    search_path: String,
}

impl PreparedLedger {
    pub(super) async fn set_search_path(
        &self,
        tx: &mut Transaction<'_>,
    ) -> Result<(), DatabaseError> {
        let connection = postgres_connection(tx)?;
        sqlx::query("SELECT pg_catalog.set_config('search_path', $1, true)")
            .bind(&self.search_path)
            .execute(connection)
            .await?;
        Ok(())
    }
}

/// Prepare a ledger after the runner has acquired catalog-X and ledger-X.
/// The transaction must stay alive through all dependent migration work.
pub(super) async fn prepare(tx: &mut Transaction<'_>) -> Result<PreparedLedger, DatabaseError> {
    let connection = postgres_connection(tx)?;
    let (lock_timeout, statement_timeout): (String, String) = sqlx::query_as(
        "SELECT pg_catalog.current_setting('lock_timeout'), \
         pg_catalog.current_setting('statement_timeout')",
    )
    .fetch_one(&mut *connection)
    .await?;
    set_timeout(connection, "lock_timeout", &lock_timeout, LOCK_TIMEOUT_MS).await?;
    set_timeout(
        connection,
        "statement_timeout",
        &statement_timeout,
        STATEMENT_TIMEOUT_MS,
    )
    .await?;

    // This runs before the catalog-only search path: qualify the cast type as
    // well as the function, so a leading schema cannot inject a domain check.
    // The resulting schema is observed on this backend, not independently pinned.
    let name: Option<String> =
        sqlx::query_scalar("SELECT pg_catalog.current_schema()::pg_catalog.text")
            .fetch_one(&mut *connection)
            .await?;
    let name = name.ok_or(MigrationLedgerError::UnsafeTargetSchema)?;
    if name.is_empty() || name.len() > 63 || name.starts_with("pg_") || name == "information_schema"
    {
        return Err(MigrationLedgerError::UnsafeTargetSchema.into());
    }
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await?;

    let (oid, owner): (Oid, Oid) = sqlx::query_as(
        "SELECT n.oid, n.nspowner FROM pg_catalog.pg_namespace n \
         WHERE n.nspname = $1 AND n.oid <> pg_catalog.pg_my_temp_schema()",
    )
    .bind(&name)
    .fetch_optional(&mut *connection)
    .await?
    .ok_or(MigrationLedgerError::UnsafeTargetSchema)?;
    let schema = Schema { name, oid, owner };
    let quoted_schema = format!("\"{}\"", schema.name.replace('"', "\"\""));
    let table = format!("{quoted_schema}._migrations");

    // Event triggers execute around CREATE and ALTER before table-local
    // checks can protect those operations. No DDL precedes this check.
    let no_event_triggers: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_event_trigger \
         WHERE evtenabled <> 'D' LIMIT 1)",
    )
    .fetch_one(&mut *connection)
    .await?;
    if !no_event_triggers {
        return Err(MigrationLedgerError::EnabledEventTrigger.into());
    }

    let initial = inspect_catalog(connection, &schema).await?;
    if initial.is_none() {
        // Deliberately no IF NOT EXISTS: an unexpected concurrent replacement
        // must not be silently adopted. Noncooperating DDL is still outside
        // the catalog-fence guarantee and must be externally quiescent.
        sqlx::query(&format!(
            "CREATE TABLE {table} (version pg_catalog.int8 PRIMARY KEY, \
             description pg_catalog.text NOT NULL, checksum pg_catalog.text, \
             applied_at pg_catalog.timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP) USING heap"
        ))
        .execute(&mut *connection)
        .await?;
    }

    let before_lock = inspect_catalog(connection, &schema)
        .await?
        .ok_or(MigrationLedgerError::UnsafeCatalog)?;
    if initial
        .as_ref()
        .is_some_and(|ledger| ledger != &before_lock)
    {
        return Err(MigrationLedgerError::UnsafeCatalog.into());
    }
    let lock = sqlx::query(&format!("LOCK TABLE ONLY {table} IN ACCESS EXCLUSIVE MODE"))
        .execute(&mut *connection)
        .await;
    if let Err(error) = lock {
        if error
            .as_database_error()
            .and_then(|database| database.code())
            .as_deref()
            == Some("55P03")
        {
            return Err(MigrationLedgerError::RelationLockTimeout.into());
        }
        return Err(error.into());
    }
    let after_lock = inspect_catalog(connection, &schema)
        .await?
        .ok_or(MigrationLedgerError::UnsafeCatalog)?;
    if before_lock != after_lock {
        return Err(MigrationLedgerError::UnsafeCatalog.into());
    }

    let checksum_column_added = !after_lock.has_checksum;
    if checksum_column_added {
        sqlx::query(&format!(
            "ALTER TABLE ONLY {table} ADD COLUMN checksum pg_catalog.text"
        ))
        .execute(&mut *connection)
        .await?;
        let upgraded = inspect_catalog(connection, &schema)
            .await?
            .ok_or(MigrationLedgerError::UnsafeCatalog)?;
        if upgraded
            != (Ledger {
                has_checksum: true,
                ..after_lock
            })
        {
            return Err(MigrationLedgerError::UnsafeCatalog.into());
        }
    }

    // Do not impose the catalog-inspection deadlines on migration bodies.
    for (setting, value) in [
        ("lock_timeout", lock_timeout.as_str()),
        ("statement_timeout", statement_timeout.as_str()),
    ] {
        sqlx::query("SELECT pg_catalog.set_config($1, $2, true)")
            .bind(setting)
            .bind(value)
            .execute(&mut *connection)
            .await?;
    }
    let prepared = PreparedLedger {
        table,
        checksum_column_added,
        search_path: format!("{quoted_schema}, pg_temp"),
    };
    prepared.set_search_path(tx).await?;
    Ok(prepared)
}

fn postgres_connection<'a>(
    tx: &'a mut Transaction<'_>,
) -> Result<&'a mut PgConnection, DatabaseError> {
    tx.postgres_connection().ok_or_else(|| {
        DatabaseError::QueryFailed("ledger preflight requires PostgreSQL".to_owned())
    })
}

async fn set_timeout(
    connection: &mut PgConnection,
    setting: &str,
    current: &str,
    cap: u64,
) -> Result<(), DatabaseError> {
    let millis = timeout_millis(current)
        .ok_or_else(|| DatabaseError::QueryFailed("invalid PostgreSQL timeout".to_owned()))?;
    if millis == 0 || millis > cap {
        sqlx::query("SELECT pg_catalog.set_config($1, $2, true)")
            .bind(setting)
            .bind(format!("{cap}ms"))
            .execute(connection)
            .await?;
    }
    Ok(())
}

async fn inspect_catalog(
    connection: &mut PgConnection,
    schema: &Schema,
) -> Result<Option<Ledger>, DatabaseError> {
    let schema_matches: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n \
         WHERE n.nspname = $1 AND n.oid = $2 AND n.nspowner = $3)",
    )
    .bind(&schema.name)
    .bind(schema.oid)
    .bind(schema.owner)
    .fetch_one(&mut *connection)
    .await?;
    if !schema_matches {
        return Err(MigrationLedgerError::UnsafeCatalog.into());
    }
    let relation: Option<(Oid, Oid)> = sqlx::query_as(
        "SELECT c.oid, c.relowner FROM pg_catalog.pg_class c \
         WHERE c.relnamespace = $1 AND c.relname = '_migrations'",
    )
    .bind(schema.oid)
    .fetch_optional(&mut *connection)
    .await?;
    let Some((oid, owner)) = relation else {
        return Ok(None);
    };

    // Bound each variable-size inventory before running the exact joins.
    let bounded: bool = sqlx::query_scalar(include_str!("postgres_ledger_inventory.sql"))
        .bind(oid)
        .fetch_one(&mut *connection)
        .await?;
    if !bounded {
        return Err(MigrationLedgerError::UnsafeCatalog.into());
    }
    // The exact query compares these varlena fields. Check their stored size
    // and reject compressed values before asking PostgreSQL to expand them.
    let raw_fields_bounded: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attrdef d \
           WHERE d.adrelid = $1 AND (pg_catalog.pg_column_size(d.adbin) > 256 \
             OR pg_catalog.pg_column_compression(d.adbin) IS NOT NULL)) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint k \
           WHERE k.conrelid = $1 AND k.conkey IS NOT NULL \
             AND (pg_catalog.pg_column_size(k.conkey) > 128 \
               OR pg_catalog.pg_column_compression(k.conkey) IS NOT NULL))",
    )
    .bind(oid)
    .fetch_one(&mut *connection)
    .await?;
    if !raw_fields_bounded {
        return Err(MigrationLedgerError::UnsafeCatalog.into());
    }
    let index: Oid =
        sqlx::query_scalar("SELECT i.indexrelid FROM pg_catalog.pg_index i WHERE i.indrelid = $1")
            .bind(oid)
            .fetch_optional(&mut *connection)
            .await?
            .ok_or(MigrationLedgerError::UnsafeCatalog)?;
    let has_checksum: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a WHERE a.attrelid = $1 \
         AND a.attnum > 0 AND NOT a.attisdropped AND a.attname = 'checksum')",
    )
    .bind(oid)
    .fetch_one(&mut *connection)
    .await?;
    let exact: bool = sqlx::query_scalar(include_str!("postgres_ledger_catalog.sql"))
        .bind(&schema.name)
        .bind(schema.oid)
        .bind(schema.owner)
        .bind(oid)
        .bind(owner)
        .bind(index)
        .bind(has_checksum)
        .fetch_optional(&mut *connection)
        .await?
        .unwrap_or(false);
    if !exact {
        return Err(MigrationLedgerError::UnsafeCatalog.into());
    }
    Ok(Some(Ledger {
        oid,
        owner,
        index,
        has_checksum,
    }))
}
