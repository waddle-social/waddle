//! Dark, transaction-bound entry proof for the committed V1023 ledger prefix.
//!
//! This checks only the migration ledger. It is not a physical attestation,
//! serving authorization, role-graph audit, or proof of operational fencing.

use sqlx::postgres::types::Oid;
use sqlx::PgConnection;
use thiserror::Error;

use super::{MigrationRunner, MIGRATION_LEDGER_ADVISORY_LOCK_KEY};
use crate::db::{DatabaseError, Transaction};

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
    #[error("ledger preflight requires a PostgreSQL READ COMMITTED READ ONLY transaction")]
    TransactionBoundary,
    #[error("migration ledger catalog or ACL does not match the pinned shape")]
    CatalogDrift,
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("migration ledger PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// A checked ledger entry held on the same caller-owned transaction as its
/// shared advisory and relation locks. This proof cannot be cloned and its
/// borrow prevents committing or replacing the transaction while it is held.
/// Dropping it does not release the transaction-scoped locks.
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
/// READ COMMITTED READ ONLY transaction, and supply independent pins and schema
/// name. This acquires the shared ledger advisory lock, validates the pinned
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
) -> Result<SharedMigrationLedger<'a, 'connection>, DarkLedgerPreflightError> {
    if !pins.valid() || schema.is_empty() || schema.contains('\0') {
        return Err(DarkLedgerPreflightError::InvalidPins);
    }
    let connection = tx
        .postgres_connection()
        .ok_or(DarkLedgerPreflightError::TransactionBoundary)?;

    // Never resolve catalog operators or types through an attacker-controlled
    // search path. Restore the caller's transaction-local setting on success
    // and on non-SQL errors; cancellation requires rollback by the caller.
    let previous_path: String =
        sqlx::query_scalar("SELECT pg_catalog.current_setting('search_path')")
            .fetch_one(&mut *connection)
            .await?;
    // An implicitly placed temporary schema precedes pg_catalog for type and
    // relation lookup, so list it explicitly after pg_catalog as well.
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await?;
    let result = verify_locked(connection, schema, pins).await;
    let restored = sqlx::query("SELECT pg_catalog.set_config('search_path', $1, true)")
        .bind(previous_path)
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
) -> Result<(), DarkLedgerPreflightError> {
    let boundary: bool = sqlx::query_scalar(
        "SELECT pg_catalog.current_setting('transaction_isolation') = 'read committed' \
         AND pg_catalog.current_setting('transaction_read_only') = 'on'",
    )
    .fetch_one(&mut *connection)
    .await?;
    if !boundary {
        return Err(DarkLedgerPreflightError::TransactionBoundary);
    }
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
        .verify_applied_through_locked_postgres(connection, &table, 1023)
        .await?;
    Ok(())
}

async fn verify_catalog(
    connection: &mut PgConnection,
    schema: &str,
    pins: DarkMigrationLedgerPins,
) -> Result<(), DarkLedgerPreflightError> {
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
  AND pg_catalog.has_schema_privilege(runtime.oid, n.oid, 'USAGE')
  AND NOT pg_catalog.has_schema_privilege(runtime.oid, n.oid, 'CREATE')
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
      COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))) acl
    WHERE acl.privilege_type = 'CREATE' AND acl.grantee <> n.nspowner)
  AND c.relkind = 'r' AND c.relpersistence = 'p' AND NOT c.relispartition
  AND c.relam = (SELECT am.oid FROM pg_catalog.pg_am am
                 WHERE am.amname = 'heap' AND am.amtype = 't')
  AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND NOT c.relhasrules
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i
                  WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite r WHERE r.ev_class = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid)
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
    JOIN pg_catalog.pg_opfamily family ON family.oid = opclass.opcfamily
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
        WHERE p.amprocfamily = family.oid AND f.pronamespace <> 11::pg_catalog.oid)
      AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_amop op
        JOIN pg_catalog.pg_operator o ON o.oid = op.amopopr
        JOIN pg_catalog.pg_proc f ON f.oid = o.oprcode
        WHERE op.amopfamily = family.oid
          AND (o.oprnamespace <> 11::pg_catalog.oid OR f.pronamespace <> 11::pg_catalog.oid)))
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
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
JOIN pg_catalog.pg_roles owner ON owner.oid = $3
JOIN pg_catalog.pg_roles runtime ON runtime.oid = $4
WHERE n.nspname = $1 AND c.relname = '_migrations'
"#;

#[cfg(test)]
#[path = "ledger_preflight_tests.rs"]
mod tests;
