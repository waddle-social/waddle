//! Dark, transaction-bound entry proofs for committed migration ledger prefixes.
//!
//! This checks only the migration ledger. It is not a physical attestation,
//! serving authorization, role-graph audit, or proof of operational fencing.

use sqlx::postgres::types::Oid;
use sqlx::PgConnection;
use thiserror::Error;

use super::{MigrationRunner, MIGRATION_LEDGER_ADVISORY_LOCK_KEY};
use crate::db::lineage::LINEAGE_ADVISORY_LOCK_KEY;
use crate::db::{DatabaseError, Transaction};

const MAX_SCHEMA_NAME_BYTES: usize = 63;
const MAX_RESTORE_PATH_BYTES: usize = 1024;
const MAX_ACL_BYTES: i32 = 8192;
const MAX_ACL_ITEMS: i32 = 64;

/// Independently provisioned PostgreSQL object identities, not catalog-derived
/// observations. The reader is the current, unassumed runtime login.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DarkMigrationLedgerPins {
    pub schema_oid: u32,
    pub schema_owner_role_oid: u32,
    pub runtime_role_oid: u32,
    pub ledger_relation_oid: u32,
    pub ledger_primary_index_oid: u32,
}

impl DarkMigrationLedgerPins {
    fn valid(self) -> bool {
        self.schema_oid != 0
            && self.schema_owner_role_oid != 0
            && self.runtime_role_oid != 0
            && self.ledger_relation_oid != 0
            && self.ledger_primary_index_oid != 0
            && self.schema_owner_role_oid != self.runtime_role_oid
    }
}

#[derive(Debug, Error)]
pub enum DarkLedgerPreflightError {
    #[error("ledger preflight requires independently supplied nonzero object pins")]
    InvalidPins,
    #[error("ledger preflight schema name is empty, invalid, or exceeds PostgreSQL's identifier byte limit")]
    InvalidSchema,
    #[error("ledger preflight restore search path is invalid or exceeds its byte limit")]
    InvalidRestorePath,
    #[error("ledger preflight requires a PostgreSQL READ COMMITTED transaction with the requested access mode")]
    TransactionBoundary,
    #[error("could not validate the PostgreSQL wait bounds")]
    InvalidWaitBound,
    #[error("ledger catalog ACL is compressed and is rejected before expansion")]
    CompressedAcl,
    #[error("ledger catalog ACL exceeds its bounded raw size")]
    OversizedAcl,
    #[error("ledger catalog ACL exceeds its bounded entry count")]
    TooManyAclEntries,
    #[error("migration ledger catalog or ACL does not match the pinned shape")]
    CatalogDrift,
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("migration ledger PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// A checked ledger prefix on the caller-owned transaction.
///
/// This wrapper does not prevent the trusted caller from issuing raw COMMIT or
/// ROLLBACK through the transaction, nor does it enforce that the caller keeps
/// the required advisory/relation locks until all dependent work is complete.
/// Trusted callers must preserve that contract and must not acquire the
/// exclusive migration advisory lock while this proof is in use.
pub struct SharedMigrationLedger<'a, 'connection> {
    tx: &'a mut Transaction<'connection>,
}

impl<'a, 'connection> SharedMigrationLedger<'a, 'connection> {
    /// Continue observation on the same transaction while holding the proof.
    /// Do not acquire the exclusive migration advisory lock, including after an
    /// epoch prelock: upgrading the shared lock can deadlock the owner.
    pub fn transaction(&mut self) -> &mut Transaction<'connection> {
        self.tx
    }
}

/// Acquire a shared-ledger proof and verify the committed V1023 catalog prefix.
///
/// The caller must acquire any lineage advisory lock first, use a bounded
/// READ COMMITTED READ ONLY transaction, and supply independent pins, schema
/// name, and a bounded path to restore. This acquires the shared ledger advisory lock, validates the pinned
/// table through `pg_catalog`, locks `ONLY` that table in ACCESS SHARE mode,
/// revalidates after any wait, and then reads and checks the ledger. An epoch
/// relation lock, if needed, must be acquired only after this function returns.
///
/// Privileged DDL, role membership, ACL changes, and writes by the owner must
/// be independently quiescent throughout the observation. Advisory locks only
/// coordinate participants that honor them. This check neither attests a
/// connection or its catalog provider nor authorizes a serving selector.
/// Errors or cancellation require the caller to roll back the transaction.
pub async fn acquire_dark_shared_migration_ledger_v1023<'a, 'connection>(
    tx: &'a mut Transaction<'connection>,
    schema: &str,
    pins: DarkMigrationLedgerPins,
    restore_search_path: &str,
) -> Result<SharedMigrationLedger<'a, 'connection>, DarkLedgerPreflightError> {
    acquire_dark_shared_migration_ledger(tx, schema, pins, restore_search_path, 1023, true, false)
        .await
}

/// Acquire a shared-ledger proof for the complete registered V1030 prefix in
/// a caller-owned READ COMMITTED READ WRITE transaction.
///
/// Call this before dependent reads or application locks. It applies bounded
/// local lock and statement timeouts without loosening tighter values, then
/// acquires the shared lineage advisory lock, shared migration advisory lock,
/// and ledger relation lock in that order before reading the ledger. The caller
/// must keep privileged DDL, role membership, ACL changes, and owner writes
/// quiescent throughout dependent work, and must not acquire the exclusive
/// migration advisory lock while holding this proof. On any error the caller
/// must roll back immediately; on cancellation it must roll back when control
/// resumes. The proof does not attest the connection, apply migrations, or
/// authorize serving.
pub async fn acquire_dark_shared_migration_ledger_v1030_write<'a, 'connection>(
    tx: &'a mut Transaction<'connection>,
    schema: &str,
    pins: DarkMigrationLedgerPins,
    restore_search_path: &str,
) -> Result<SharedMigrationLedger<'a, 'connection>, DarkLedgerPreflightError> {
    acquire_dark_shared_migration_ledger(tx, schema, pins, restore_search_path, 1030, false, true)
        .await
}

/// Acquire a shared-ledger proof for the complete registered V1030 prefix in
/// a caller-owned READ COMMITTED READ ONLY transaction.
///
/// Call this before dependent reads or application locks. It applies bounded
/// local lock and statement timeouts without loosening tighter values, then
/// acquires the shared lineage advisory lock, shared migration advisory lock,
/// and ledger relation lock in that order before reading the ledger. The caller
/// must keep privileged DDL, role membership, ACL changes, and owner writes
/// quiescent throughout dependent work, and must not acquire the exclusive
/// migration advisory lock while holding this proof. On any error the caller
/// must roll back immediately; on cancellation it must roll back when control
/// resumes. The proof does not attest the connection, apply migrations, or
/// authorize serving.
pub async fn acquire_dark_shared_migration_ledger_v1030_read<'a, 'connection>(
    tx: &'a mut Transaction<'connection>,
    schema: &str,
    pins: DarkMigrationLedgerPins,
    restore_search_path: &str,
) -> Result<SharedMigrationLedger<'a, 'connection>, DarkLedgerPreflightError> {
    acquire_dark_shared_migration_ledger(tx, schema, pins, restore_search_path, 1030, true, true)
        .await
}

/// Verify the complete pinned ledger on a caller-owned READ COMMITTED READ ONLY
/// SQLx transaction. This entry is used by the closed selected-transaction
/// candidate, whose first statement is its catalog mutation
/// `pg_try_advisory_xact_lock_shared`. The caller must already have proved
/// physical identity and held the shared lineage barrier in the same
/// transaction.
pub async fn verify_dark_shared_migration_ledger_v1030_read_sqlx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    schema: &str,
    pins: DarkMigrationLedgerPins,
    restore_search_path: &str,
) -> Result<(), DarkLedgerPreflightError> {
    if !pins.valid() {
        return Err(DarkLedgerPreflightError::InvalidPins);
    }
    if schema.is_empty() || schema.len() > MAX_SCHEMA_NAME_BYTES || schema.contains('\0') {
        return Err(DarkLedgerPreflightError::InvalidSchema);
    }
    if restore_search_path.is_empty()
        || restore_search_path.len() > MAX_RESTORE_PATH_BYTES
        || restore_search_path.contains('\0')
    {
        return Err(DarkLedgerPreflightError::InvalidRestorePath);
    }
    let connection: &mut PgConnection = tx;
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await?;
    let result = verify_locked(connection, schema, pins, 1030, true, false).await;
    let restored = sqlx::query("SELECT pg_catalog.set_config('search_path', $1, true)")
        .bind(restore_search_path)
        .execute(&mut *connection)
        .await;
    result?;
    restored?;
    Ok(())
}

async fn acquire_dark_shared_migration_ledger<'a, 'connection>(
    tx: &'a mut Transaction<'connection>,
    schema: &str,
    pins: DarkMigrationLedgerPins,
    restore_search_path: &str,
    required_version: i64,
    require_read_only: bool,
    require_lineage_lock: bool,
) -> Result<SharedMigrationLedger<'a, 'connection>, DarkLedgerPreflightError> {
    if !pins.valid() {
        return Err(DarkLedgerPreflightError::InvalidPins);
    }
    if schema.is_empty() || schema.len() > MAX_SCHEMA_NAME_BYTES || schema.contains('\0') {
        return Err(DarkLedgerPreflightError::InvalidSchema);
    }
    if restore_search_path.is_empty()
        || restore_search_path.len() > MAX_RESTORE_PATH_BYTES
        || restore_search_path.contains('\0')
    {
        return Err(DarkLedgerPreflightError::InvalidRestorePath);
    }
    // Preserve stricter inherited timeouts while capping unbounded waits.
    crate::db::read_only_timeout::bound_postgres_waits(tx)
        .await
        .map_err(|error| match error {
            crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                DarkLedgerPreflightError::Database(error)
            }
            crate::db::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { .. } => {
                DarkLedgerPreflightError::InvalidWaitBound
            }
        })?;
    let connection = tx
        .postgres_connection()
        .ok_or(DarkLedgerPreflightError::TransactionBoundary)?;

    // Never fetch an inherited GUC into Rust. Restore only the caller-supplied,
    // byte-bounded path after the catalog-only proof.
    // An implicitly placed temporary schema precedes pg_catalog for type and
    // relation lookup, so list it explicitly after pg_catalog as well.
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await?;
    let result = verify_locked(
        connection,
        schema,
        pins,
        required_version,
        require_read_only,
        require_lineage_lock,
    )
    .await;
    let restored = sqlx::query("SELECT pg_catalog.set_config('search_path', $1, true)")
        .bind(restore_search_path)
        .execute(&mut *connection)
        .await;
    result?;
    restored?;
    Ok(SharedMigrationLedger { tx })
}

async fn verify_locked(
    connection: &mut PgConnection,
    schema: &str,
    pins: DarkMigrationLedgerPins,
    required_version: i64,
    require_read_only: bool,
    require_lineage_lock: bool,
) -> Result<(), DarkLedgerPreflightError> {
    let boundary: bool = sqlx::query_scalar(
        "SELECT \
         pg_catalog.current_setting('transaction_isolation') = 'read committed' \
         AND pg_catalog.current_setting('transaction_read_only') = $1",
    )
    .bind(if require_read_only { "on" } else { "off" })
    .fetch_one(&mut *connection)
    .await?;
    if !boundary {
        return Err(DarkLedgerPreflightError::TransactionBoundary);
    }
    if require_lineage_lock {
        sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock_shared($1)")
            .bind(LINEAGE_ADVISORY_LOCK_KEY)
            .execute(&mut *connection)
            .await?;
    }
    verify_acl_raw_bounds(connection, schema, pins).await?;
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock_shared($1)")
        .bind(MIGRATION_LEDGER_ADVISORY_LOCK_KEY)
        .execute(&mut *connection)
        .await?;

    verify_catalog(connection, schema, pins).await?;
    let table = format!("\"{}\"._migrations", schema.replace('"', "\"\""));
    sqlx::query(&format!("LOCK TABLE ONLY {table} IN ACCESS SHARE MODE"))
        .execute(&mut *connection)
        .await?;
    // The lock may have waited while the name was replaced. A fresh READ
    // COMMITTED catalog snapshot must still identify the independently pinned
    // relation, and all structure and ACL checks must still hold.
    verify_catalog(connection, schema, pins).await?;
    MigrationRunner::single()
        .verify_applied_through_locked_postgres(connection, &table, required_version)
        .await?;
    Ok(())
}

async fn verify_acl_raw_bounds(
    connection: &mut PgConnection,
    schema: &str,
    pins: DarkMigrationLedgerPins,
) -> Result<(), DarkLedgerPreflightError> {
    let raw: Option<(i32, Option<String>, i32, Option<String>)> = sqlx::query_as(
        "SELECT pg_catalog.pg_column_size(COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))), \
                pg_catalog.pg_column_compression(COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))), \
                pg_catalog.pg_column_size(COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))), \
                pg_catalog.pg_column_compression(COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) \
         FROM pg_catalog.pg_namespace n \
         JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid \
         WHERE n.oid = $1 AND c.oid = $2 AND n.nspname = $3 AND c.relname = '_migrations'",
    )
    .bind(Oid(pins.schema_oid))
    .bind(Oid(pins.ledger_relation_oid))
    .bind(schema)
    .fetch_optional(&mut *connection)
    .await?;
    let Some((schema_size, schema_compression, table_size, table_compression)) = raw else {
        return Err(DarkLedgerPreflightError::CatalogDrift);
    };
    if schema_compression.is_some() || table_compression.is_some() {
        return Err(DarkLedgerPreflightError::CompressedAcl);
    }
    if schema_size > MAX_ACL_BYTES || table_size > MAX_ACL_BYTES {
        return Err(DarkLedgerPreflightError::OversizedAcl);
    }
    Ok(())
}

async fn verify_catalog(
    connection: &mut PgConnection,
    schema: &str,
    pins: DarkMigrationLedgerPins,
) -> Result<(), DarkLedgerPreflightError> {
    // These are deliberately separate catalog statements. The byte and
    // compression guard runs before cardinality, and both run before any
    // privilege helper or ACL expansion can consume the arrays.
    verify_acl_raw_bounds(connection, schema, pins).await?;
    verify_acl_item_counts(connection, schema, pins).await?;
    // Bound the relation inventory and fixed built-in operator family before
    // the exact catalog query can join or scan their members. This must be a
    // separate statement because SQL predicate evaluation order is not fixed.
    let inventory: bool = sqlx::query_scalar(CATALOG_INVENTORY_QUERY)
        .bind(Oid(pins.ledger_relation_oid))
        .bind(Oid(pins.schema_oid))
        .fetch_optional(&mut *connection)
        .await?
        .unwrap_or(false);
    if !inventory {
        return Err(DarkLedgerPreflightError::CatalogDrift);
    }
    let exact: bool = sqlx::query_scalar(CATALOG_QUERY)
        .bind(schema)
        .bind(Oid(pins.schema_oid))
        .bind(Oid(pins.schema_owner_role_oid))
        .bind(Oid(pins.runtime_role_oid))
        .bind(Oid(pins.ledger_relation_oid))
        .bind(Oid(pins.ledger_primary_index_oid))
        .fetch_optional(connection)
        .await?
        .unwrap_or(false);
    if exact {
        Ok(())
    } else {
        Err(DarkLedgerPreflightError::CatalogDrift)
    }
}

async fn verify_acl_item_counts(
    connection: &mut PgConnection,
    schema: &str,
    pins: DarkMigrationLedgerPins,
) -> Result<(), DarkLedgerPreflightError> {
    let counts: Option<(i32, i32)> = sqlx::query_as(
        "SELECT pg_catalog.cardinality(COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))), \
                pg_catalog.cardinality(COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) \
         FROM pg_catalog.pg_namespace n \
         JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid \
         WHERE n.oid = $1 AND c.oid = $2 AND n.nspname = $3 AND c.relname = '_migrations'",
    )
    .bind(Oid(pins.schema_oid))
    .bind(Oid(pins.ledger_relation_oid))
    .bind(schema)
    .fetch_optional(&mut *connection)
    .await?;
    let Some((schema_count, table_count)) = counts else {
        return Err(DarkLedgerPreflightError::CatalogDrift);
    };
    if schema_count > MAX_ACL_ITEMS || table_count > MAX_ACL_ITEMS {
        return Err(DarkLedgerPreflightError::TooManyAclEntries);
    }
    Ok(())
}

// Count at most one over each bound, without consuming variable-size catalog
// fields. Include system attributes because the later ACL check inspects them.
// PostgreSQL 17's built-in btree integer_ops family has OID 1976.
const CATALOG_INVENTORY_QUERY: &str = r#"
SELECT c.relnamespace = $2 AND c.relname = '_migrations'
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_attribute a
       WHERE a.attrelid = $1 LIMIT 33) bounded) <= 32
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_index i
       WHERE i.indrelid = $1 LIMIT 2) bounded) <= 1
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_trigger t
       WHERE t.tgrelid = $1 LIMIT 1) bounded) = 0
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_amop o
       WHERE o.amopfamily = 1976::pg_catalog.oid LIMIT 129) bounded) <= 128
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_amproc p
       WHERE p.amprocfamily = 1976::pg_catalog.oid LIMIT 129) bounded) <= 128
FROM pg_catalog.pg_class c WHERE c.oid = $1
"#;

// No target relation, expression, type output function, or rule is touched by
// this query. Built-in OIDs are matched directly, not via user-defined casts.
const CATALOG_QUERY: &str = r#"
SELECT n.oid = $2 AND n.nspowner = $3 AND c.oid = $5 AND c.relowner = $3
  AND owner.oid <> runtime.oid AND runtime.rolcanlogin
  AND NOT runtime.rolsuper AND NOT runtime.rolbypassrls
  AND (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = session_user) = runtime.oid
  AND (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = current_user) = runtime.oid
  AND NOT pg_catalog.pg_has_role(runtime.oid, owner.oid, 'SET')
  AND NOT pg_catalog.pg_has_role(runtime.oid, owner.oid, 'USAGE')
  AND CASE WHEN pg_catalog.pg_column_size(
      COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))) <= 8192
      THEN pg_catalog.cardinality(
        COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))) <= 64
       AND pg_catalog.has_schema_privilege(runtime.oid, n.oid, 'USAGE')
       AND NOT pg_catalog.has_schema_privilege(runtime.oid, n.oid, 'CREATE')
       AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
          COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))) acl
         WHERE acl.privilege_type = 'CREATE' AND acl.grantee <> n.nspowner)
      ELSE false END
  AND c.relkind = 'r' AND c.relpersistence = 'p' AND NOT c.relispartition
  AND c.relam = (SELECT am.oid FROM pg_catalog.pg_am am
                 WHERE am.amname = 'heap' AND am.amtype = 't')
  AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND NOT c.relhasrules
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i
                  WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite r WHERE r.ev_class = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
                  WHERE a.attrelid = c.oid AND a.attacl IS NOT NULL)
  AND (SELECT count(*) FROM pg_catalog.pg_attribute a
       WHERE a.attrelid = c.oid AND a.attnum > 0) = 4
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
    WHERE a.attrelid = c.oid AND a.attnum > 0 AND (
      a.attisdropped OR a.atthasmissing OR a.attgenerated <> '' OR a.attidentity <> ''
      OR a.attndims <> 0 OR a.attacl IS NOT NULL OR a.attoptions IS NOT NULL
      OR a.attfdwoptions IS NOT NULL OR a.atttypmod <> -1
      OR (a.attname, a.atttypid, a.attnotnull, a.attcollation) NOT IN (
        ('version', 20::pg_catalog.oid, true, 0::pg_catalog.oid),
        ('description', 25::pg_catalog.oid, true, 100::pg_catalog.oid),
        ('checksum', 25::pg_catalog.oid, false, 100::pg_catalog.oid),
        ('applied_at', 1184::pg_catalog.oid, true, 0::pg_catalog.oid))))
  -- A planner may inspect or choose an index even without WHERE or ORDER BY.
  -- Only the pinned, ordinary built-in primary index is allowed.
  AND (SELECT count(*) FROM pg_catalog.pg_index i WHERE i.indrelid = c.oid) = 1
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_index i
    JOIN pg_catalog.pg_class idx ON idx.oid = i.indexrelid
    JOIN pg_catalog.pg_opclass opclass ON opclass.oid = i.indclass[0]
      AND opclass.oid = 3124::pg_catalog.oid
    JOIN pg_catalog.pg_opfamily family ON family.oid = opclass.opcfamily
      AND family.oid = 1976::pg_catalog.oid
    WHERE i.indrelid = c.oid AND idx.oid = $6 AND idx.relnamespace = n.oid
      AND idx.relowner = owner.oid AND idx.relkind = 'i' AND idx.relpersistence = 'p'
      AND idx.relam = 403::pg_catalog.oid AND opclass.opcmethod = 403::pg_catalog.oid
      AND opclass.opcnamespace = 11::pg_catalog.oid AND opclass.opcname = 'int8_ops'
      AND opclass.opcintype = 20::pg_catalog.oid AND opclass.opcdefault
      AND family.opfnamespace = 11::pg_catalog.oid AND family.opfname = 'integer_ops'
      AND family.opfmethod = 403::pg_catalog.oid
      AND i.indisprimary AND i.indisunique AND i.indisvalid AND i.indisready
      AND i.indislive AND i.indimmediate AND NOT i.indisexclusion
      AND NOT i.indnullsnotdistinct AND i.indnatts = 1 AND i.indnkeyatts = 1
      AND i.indkey[0] = (SELECT a.attnum FROM pg_catalog.pg_attribute a
                         WHERE a.attrelid = c.oid AND a.attname = 'version')
      AND i.indcollation[0] = 0::pg_catalog.oid AND i.indoption[0] = 0
      AND i.indexprs IS NULL AND i.indpred IS NULL
      AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_amproc p
        JOIN pg_catalog.pg_proc f ON f.oid = p.amproc
        WHERE p.amprocfamily = 1976::pg_catalog.oid AND f.pronamespace <> 11::pg_catalog.oid)
      AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_amop op
        JOIN pg_catalog.pg_operator o ON o.oid = op.amopopr
        JOIN pg_catalog.pg_proc f ON f.oid = o.oprcode
        WHERE op.amopfamily = 1976::pg_catalog.oid
          AND (o.oprnamespace <> 11::pg_catalog.oid OR f.pronamespace <> 11::pg_catalog.oid)))
  AND CASE WHEN pg_catalog.pg_column_size(
      COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) <= 8192
      THEN pg_catalog.cardinality(
        COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) <= 64
       AND pg_catalog.has_table_privilege(runtime.oid, c.oid, 'SELECT')
       AND NOT pg_catalog.has_table_privilege(runtime.oid, c.oid,
          'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
       AND NOT pg_catalog.has_table_privilege(runtime.oid, c.oid, 'SELECT WITH GRANT OPTION')
       AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
          COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) acl
         WHERE acl.grantee NOT IN (owner.oid, runtime.oid)
            OR (acl.grantee = runtime.oid AND
                (acl.grantor <> owner.oid OR acl.is_grantable OR acl.privilege_type <> 'SELECT')))
       AND (SELECT count(*) FROM pg_catalog.aclexplode(
          COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) acl
         WHERE acl.grantee = runtime.oid AND acl.grantor = owner.oid
           AND acl.privilege_type = 'SELECT' AND NOT acl.is_grantable) = 1
      ELSE false END
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
JOIN pg_catalog.pg_roles owner ON owner.oid = $3
JOIN pg_catalog.pg_roles runtime ON runtime.oid = $4
WHERE n.nspname = $1 AND c.relname = '_migrations' AND n.oid = $2 AND c.oid = $5
"#;

#[cfg(test)]
#[path = "ledger_preflight_tests.rs"]
mod tests;
