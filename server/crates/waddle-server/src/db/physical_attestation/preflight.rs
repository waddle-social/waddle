//! Opt-in catalog proof for the physical attestor. This is not a pool hook.

use sqlx::postgres::types::Oid;
use sqlx::{PgConnection, Postgres, Transaction};
use thiserror::Error;
use uuid::Uuid;

use super::installer::{marker, verify_function, verify_prerequisites};
use super::{
    parse_snapshot, quote_identifier, verify_identity, PhysicalAttestationConfig,
    PhysicalAttestationError,
};
use crate::db::lineage::LINEAGE_ADVISORY_LOCK_KEY;
use crate::db::read_only_timeout::timeout_millis;

/// Object identities supplied through an independent, trusted operator handoff.
/// They must not be learned from the connection being attested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DarkPhysicalAttestationPins {
    pub database_owner_role_oid: u32,
    pub schema_owner_role_oid: u32,
    pub runtime_role_oid: u32,
    pub installer_role_oid: u32,
    pub definer_role_oid: u32,
    pub trusted_schema_oid: u32,
    pub lineage_relation_oid: u32,
    pub lineage_primary_index_oid: u32,
    pub attestor_function_oid: u32,
    pub installation_operation_id: Uuid,
}

impl DarkPhysicalAttestationPins {
    fn valid(self) -> bool {
        [
            self.database_owner_role_oid,
            self.schema_owner_role_oid,
            self.runtime_role_oid,
            self.installer_role_oid,
            self.definer_role_oid,
            self.trusted_schema_oid,
            self.lineage_relation_oid,
            self.lineage_primary_index_oid,
            self.attestor_function_oid,
        ]
        .into_iter()
        .all(|oid| oid != 0)
            && !self.installation_operation_id.is_nil()
            && self.runtime_role_oid != self.installer_role_oid
            && self.runtime_role_oid != self.definer_role_oid
            && self.installer_role_oid != self.definer_role_oid
            && ![
                self.runtime_role_oid,
                self.installer_role_oid,
                self.definer_role_oid,
            ]
            .contains(&self.database_owner_role_oid)
            && ![
                self.runtime_role_oid,
                self.installer_role_oid,
                self.definer_role_oid,
            ]
            .contains(&self.schema_owner_role_oid)
    }
}

#[derive(Debug, Error)]
pub enum DarkPhysicalAttestationError {
    #[error("pre-attestor proof requires independently supplied nonzero object pins")]
    InvalidPins,
    #[error("pre-attestor restore path must be at most 1024 bytes and contain no NUL")]
    InvalidRestorePath,
    #[error("pre-attestor database name must contain 1 to 63 bytes and no NUL")]
    InvalidDatabaseName,
    #[error("pre-attestor proof requires PostgreSQL 17 and a bounded READ COMMITTED transaction")]
    TransactionBoundary,
    #[error("pre-attestor early catalog identity or metadata guard failed")]
    EarlyCatalogDrift,
    #[error("pre-attestor catalog expression tree exceeds the size bound")]
    OversizedCatalogExpression,
    #[error("pre-attestor catalog, ownership, ACL, or object identity drifted")]
    CatalogDrift,
    #[error(transparent)]
    Attestation(#[from] PhysicalAttestationError),
    #[error("pre-attestor PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// Prove the installed function and its lineage relation before invoking it.
///
/// The caller supplies independent pins, a direct authenticated PG17 primary,
/// and one caller-owned READ COMMITTED transaction. Its lock, statement,
/// idle-in-transaction and total transaction timeouts must be at most 5s,
/// 10s, 10s and 20s respectively. The runtime login must have exact SELECT
/// access to `_lineage` to acquire its ACCESS SHARE lock. The caller must take
/// no later-order lock before this function and must roll back on error or
/// cancellation. A separate network/overall deadline remains necessary. The
/// caller supplies the desired post-proof `search_path`, which is limited to
/// 1024 bytes and must contain no NUL. This function does not inspect or infer
/// the inherited path; the caller owns the desired post-proof session state.
///
/// Catalogs and built-in PostgreSQL objects are trusted. Privileged DDL, role
/// and ACL changes, function replacement, and lineage writes must be quiescent
/// throughout this proof and subsequent use, or follow an independently
/// enforced mutation protocol holding the exclusive lineage barrier. The
/// advisory lock does not fence uncooperative owners, and PostgreSQL offers no
/// function lock that could close that gap. This dark proof is not installed
/// in the existing pool hooks and is not serving authorization.
pub async fn attest_dark_catalog_pinned(
    tx: &mut Transaction<'_, Postgres>,
    config: &PhysicalAttestationConfig,
    pins: DarkPhysicalAttestationPins,
    restore_search_path: &str,
) -> Result<(), DarkPhysicalAttestationError> {
    if !pins.valid() {
        return Err(DarkPhysicalAttestationError::InvalidPins);
    }
    if restore_search_path.len() > 1024 || restore_search_path.contains('\0') {
        return Err(DarkPhysicalAttestationError::InvalidRestorePath);
    }
    let database_name = &config.postgres().database.name.0;
    if database_name.is_empty() || database_name.len() > 63 || database_name.contains('\0') {
        return Err(DarkPhysicalAttestationError::InvalidDatabaseName);
    }
    let connection = &mut **tx;
    // SET is not resolved through a caller-controlled search path. Do not read
    // the inherited setting: even a server-side prefix may materialize it.
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await?;
    let result = verify_and_attest(connection, config, pins).await;
    let restored = sqlx::query("SELECT pg_catalog.set_config('search_path', $1, true)")
        .bind(restore_search_path)
        .execute(&mut *connection)
        .await;
    result?;
    restored?;
    Ok(())
}

async fn verify_and_attest(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
    pins: DarkPhysicalAttestationPins,
) -> Result<(), DarkPhysicalAttestationError> {
    let version: String = sqlx::query_scalar("SHOW server_version_num")
        .fetch_one(&mut *connection)
        .await?;
    let isolation: String = sqlx::query_scalar("SHOW transaction_isolation")
        .fetch_one(&mut *connection)
        .await?;
    let lock_timeout: String = sqlx::query_scalar("SHOW lock_timeout")
        .fetch_one(&mut *connection)
        .await?;
    let statement_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&mut *connection)
        .await?;
    let idle_timeout: String = sqlx::query_scalar("SHOW idle_in_transaction_session_timeout")
        .fetch_one(&mut *connection)
        .await?;
    let transaction_timeout: String = sqlx::query_scalar("SHOW transaction_timeout")
        .fetch_one(&mut *connection)
        .await?;
    if !version
        .parse::<u32>()
        .is_ok_and(|version| (170_000..180_000).contains(&version))
        || isolation != "read committed"
        || !timeout_millis(&lock_timeout).is_some_and(|value| (1..=5_000).contains(&value))
        || !timeout_millis(&statement_timeout).is_some_and(|value| (1..=10_000).contains(&value))
        || !timeout_millis(&idle_timeout).is_some_and(|value| (1..=10_000).contains(&value))
        || !timeout_millis(&transaction_timeout).is_some_and(|value| (1..=20_000).contains(&value))
    {
        return Err(DarkPhysicalAttestationError::TransactionBoundary);
    }

    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock_shared($1)")
        .bind(LINEAGE_ADVISORY_LOCK_KEY)
        .execute(&mut *connection)
        .await?;
    verify_catalog(connection, config, pins).await?;
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}._lineage IN ACCESS SHARE MODE",
        quote_identifier(&config.postgres().schema.name.0)
    ))
    .execute(&mut *connection)
    .await?;
    // A name-based LOCK can wait while the name is replaced. READ COMMITTED
    // gives these statements a fresh snapshot after that wait.
    verify_catalog(connection, config, pins).await?;
    // The installed function returns text fields. Bound the result before it
    // crosses the client boundary, while the proven heap is still locked.
    let bounded: bool = sqlx::query_scalar(&format!(
        "SELECT pg_catalog.count(*) = 1 AND COALESCE(pg_catalog.bool_and(\
           COALESCE(pg_catalog.octet_length(l.lineage_uuid) <= 128, false) AND \
           COALESCE(pg_catalog.octet_length(l.deployment_uuid) <= 128, false) AND \
           COALESCE(pg_catalog.octet_length(l.pg_system_identifier) <= 128, false) AND \
           COALESCE(pg_catalog.octet_length(l.pg_database_oid) <= 128, false) AND \
           COALESCE(pg_catalog.octet_length(l.pg_database_name) <= 128, false) AND \
           COALESCE(pg_catalog.octet_length(l.pg_schema_oid) <= 128, false) AND \
           COALESCE(pg_catalog.octet_length(l.pg_schema_name) <= 128, false)), false) \
         FROM ONLY {}._lineage l WHERE l.id = 1",
        quote_identifier(&config.postgres().schema.name.0)
    ))
    .persistent(false)
    .fetch_one(&mut *connection)
    .await?;
    if !bounded {
        return Err(PhysicalAttestationError::InvalidValue {
            field: "bounded lineage fields",
        }
        .into());
    }
    let query = format!("SELECT * FROM {}()", config.function_path());
    let row = sqlx::query(&query)
        .persistent(false)
        .fetch_optional(&mut *connection)
        .await?
        .ok_or(PhysicalAttestationError::MissingLineage)?;
    let snapshot = parse_snapshot(&row)?;
    verify_identity(&snapshot, config)?;
    Ok(())
}

async fn verify_catalog(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
    pins: DarkPhysicalAttestationPins,
) -> Result<(), DarkPhysicalAttestationError> {
    // Privilege helpers and array comparisons may otherwise detoast an
    // attacker-provisioned catalog array. Restrict their input sizes, and
    // establish the role-membership graph, before calling those helpers.
    let bounded: bool = sqlx::query_scalar(EARLY_CATALOG_QUERY)
        .bind(&config.postgres().database.name.0)
        .bind(Oid(config.postgres().database.oid))
        .bind(&config.postgres().schema.name.0)
        .bind(Oid(config.postgres().schema.oid))
        .bind(&config.trusted_schema.0)
        .bind(Oid(pins.trusted_schema_oid))
        .bind(Oid(pins.lineage_relation_oid))
        .bind(Oid(pins.attestor_function_oid))
        .bind(config.runtime_login.as_str())
        .bind(Oid(pins.runtime_role_oid))
        .bind(config.installer_login.as_str())
        .bind(Oid(pins.installer_role_oid))
        .bind(config.definer_role.as_str())
        .bind(Oid(pins.definer_role_oid))
        .fetch_optional(&mut *connection)
        .await?
        .unwrap_or(false);
    if !bounded {
        return Err(DarkPhysicalAttestationError::EarlyCatalogDrift);
    }
    // This installer check is catalog-only and is repeated after the lock.
    match verify_prerequisites(connection, config).await {
        Ok(()) => {}
        Err(PhysicalAttestationError::Database(error)) => return Err(error.into()),
        Err(_) => return Err(DarkPhysicalAttestationError::CatalogDrift),
    }
    let exact: bool = sqlx::query_scalar(CATALOG_QUERY)
        .bind(&config.postgres().database.name.0)
        .bind(Oid(config.postgres().database.oid))
        .bind(Oid(pins.database_owner_role_oid))
        .bind(&config.postgres().schema.name.0)
        .bind(Oid(config.postgres().schema.oid))
        .bind(Oid(pins.schema_owner_role_oid))
        .bind(config.runtime_login.as_str())
        .bind(Oid(pins.runtime_role_oid))
        .bind(config.installer_login.as_str())
        .bind(Oid(pins.installer_role_oid))
        .bind(config.definer_role.as_str())
        .bind(Oid(pins.definer_role_oid))
        .bind(&config.trusted_schema.0)
        .bind(Oid(pins.trusted_schema_oid))
        .bind(Oid(pins.lineage_relation_oid))
        .bind(Oid(pins.lineage_primary_index_oid))
        .bind(Oid(pins.attestor_function_oid))
        .fetch_optional(&mut *connection)
        .await?
        .unwrap_or(false);
    if !exact {
        return Err(DarkPhysicalAttestationError::CatalogDrift);
    }
    // Check the uncompressed node-tree lengths in a separate statement before
    // deparsing, so optimizer predicate reordering cannot bypass the bound.
    // pg_node_tree is binary-compatible with text; octet_length reads its raw
    // datum size without decompressing a large value.
    let bounded_expressions: bool = sqlx::query_scalar(EXPRESSION_SIZE_QUERY)
        .bind(Oid(pins.lineage_relation_oid))
        .fetch_one(&mut *connection)
        .await?;
    if !bounded_expressions {
        return Err(DarkPhysicalAttestationError::OversizedCatalogExpression);
    }
    // Do not deparse until the preceding catalog checks have proved that the
    // trees are bounded and their dependencies have no non-system objects.
    let expressions: bool = sqlx::query_scalar(EXPRESSION_QUERY)
        .bind(Oid(pins.lineage_relation_oid))
        .fetch_one(&mut *connection)
        .await?;
    if !expressions {
        return Err(DarkPhysicalAttestationError::CatalogDrift);
    }
    match verify_function(
        connection,
        config,
        &marker(config, pins.installation_operation_id),
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(PhysicalAttestationError::Database(error)) => Err(error.into()),
        Err(_) => Err(DarkPhysicalAttestationError::CatalogDrift),
    }
}

// Both helpers inspect varlena storage metadata without detoasting. Rejecting
// compression is necessary because pg_column_size reports compressed size.
// All later ACL and function-array consumers run only after this statement.
const EARLY_CATALOG_QUERY: &str = r#"
SELECT db.oid = $2 AND n.oid = $4 AND trusted.oid = $6
  AND c.oid = $7 AND p.oid = $8
  AND r.oid = $10 AND i.oid = $12 AND d.oid = $14
  AND r.rolname = session_user AND r.rolname = current_user
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
              WHERE m.roleid = d.oid AND m.member = i.oid
                AND m.set_option AND NOT m.inherit_option AND NOT m.admin_option)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
    WHERE m.member IN (d.oid, r.oid)
       OR (m.member = i.oid AND m.roleid <> d.oid)
       OR m.roleid IN (i.oid, r.oid)
       OR (m.roleid = d.oid AND m.member <> i.oid))
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_proc f
       WHERE f.pronamespace = trusted.oid AND f.proname = 'read_physical_identity') = 1
  AND (db.datacl IS NULL OR (pg_catalog.pg_column_compression(db.datacl) IS NULL
                             AND pg_catalog.pg_column_size(db.datacl) <= 4096))
  AND (n.nspacl IS NULL OR (pg_catalog.pg_column_compression(n.nspacl) IS NULL
                            AND pg_catalog.pg_column_size(n.nspacl) <= 4096))
  AND (trusted.nspacl IS NULL OR (pg_catalog.pg_column_compression(trusted.nspacl) IS NULL
                                  AND pg_catalog.pg_column_size(trusted.nspacl) <= 4096))
  AND (c.relacl IS NULL OR (pg_catalog.pg_column_compression(c.relacl) IS NULL
                            AND pg_catalog.pg_column_size(c.relacl) <= 4096))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
    WHERE a.attrelid = c.oid AND a.attacl IS NOT NULL
      AND (pg_catalog.pg_column_compression(a.attacl) IS NOT NULL
           OR pg_catalog.pg_column_size(a.attacl) > 4096))
  AND (p.proacl IS NULL OR (pg_catalog.pg_column_compression(p.proacl) IS NULL
                            AND pg_catalog.pg_column_size(p.proacl) <= 4096))
  AND (p.proconfig IS NULL OR (pg_catalog.pg_column_compression(p.proconfig) IS NULL
                               AND pg_catalog.pg_column_size(p.proconfig) <= 4096))
  AND (p.proallargtypes IS NULL OR (pg_catalog.pg_column_compression(p.proallargtypes) IS NULL
                                    AND pg_catalog.pg_column_size(p.proallargtypes) <= 4096))
  AND (p.proargmodes IS NULL OR (pg_catalog.pg_column_compression(p.proargmodes) IS NULL
                                 AND pg_catalog.pg_column_size(p.proargmodes) <= 4096))
  AND (p.proargnames IS NULL OR (pg_catalog.pg_column_compression(p.proargnames) IS NULL
                                 AND pg_catalog.pg_column_size(p.proargnames) <= 4096))
  AND (p.protrftypes IS NULL OR (pg_catalog.pg_column_compression(p.protrftypes) IS NULL
                                 AND pg_catalog.pg_column_size(p.protrftypes) <= 4096))
  AND pg_catalog.octet_length(p.prosrc) <= 8192
  AND pg_catalog.pg_column_compression(mark.description) IS NULL
  AND pg_catalog.pg_column_size(mark.description) <= 4096
  AND (control.proacl IS NULL OR (pg_catalog.pg_column_compression(control.proacl) IS NULL
                                  AND pg_catalog.pg_column_size(control.proacl) <= 4096))
FROM pg_catalog.pg_database db
JOIN pg_catalog.pg_namespace n ON n.nspname = $3
JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = '_lineage'
JOIN pg_catalog.pg_namespace trusted ON trusted.nspname = $5
JOIN pg_catalog.pg_proc p ON p.pronamespace = trusted.oid AND p.proname = 'read_physical_identity'
JOIN pg_catalog.pg_description mark ON mark.objoid = p.oid
  AND mark.classoid = 'pg_catalog.pg_proc'::pg_catalog.regclass AND mark.objsubid = 0
JOIN pg_catalog.pg_proc control ON control.oid = 3441::pg_catalog.oid
JOIN pg_catalog.pg_roles r ON r.rolname = $9
JOIN pg_catalog.pg_roles i ON i.rolname = $11
JOIN pg_catalog.pg_roles d ON d.rolname = $13
WHERE db.datname = $1 AND db.datname = pg_catalog.current_database()
"#;

// All target properties here come from system catalogs. Nothing resolves or
// reads the target relation, executes its expressions, or calls the attestor.
const CATALOG_QUERY: &str = r#"
SELECT db.oid = $2 AND db.datdba = $3 AND NOT pg_catalog.pg_is_in_recovery()
  AND n.oid = $5 AND n.nspowner = $6 AND c.oid = $15 AND c.relowner = $6
  AND r.oid = $8 AND i.oid = $10 AND d.oid = $12
  AND r.rolname = session_user AND r.rolname = current_user
  AND trusted.oid = $14 AND trusted.nspowner = d.oid
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_proc p
       WHERE p.pronamespace = trusted.oid AND p.proname = 'read_physical_identity') = 1
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_proc p
              WHERE p.oid = $17 AND p.pronamespace = trusted.oid
                AND p.proname = 'read_physical_identity' AND p.proowner = d.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
      COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))) acl
    WHERE acl.privilege_type = 'CREATE' AND acl.grantee <> n.nspowner)
  AND c.relkind = 'r' AND c.relpersistence = 'p' AND c.relam = 2::pg_catalog.oid
  AND NOT c.relispartition AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity
  AND NOT c.relhasrules
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h
                  WHERE h.inhrelid = c.oid OR h.inhparent = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite w WHERE w.ev_class = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_statistic_ext s WHERE s.stxrelid = c.oid)
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attribute a
       WHERE a.attrelid = c.oid AND a.attnum > 0) = 10
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
    WHERE a.attrelid = c.oid AND a.attnum > 0 AND (
      a.attisdropped OR a.atthasmissing OR a.attgenerated <> '' OR a.attidentity <> ''
      OR a.attndims <> 0 OR a.attacl IS NOT NULL OR a.attoptions IS NOT NULL
      OR a.attfdwoptions IS NOT NULL OR a.atttypmod <> -1
      OR a.atthasdef <> (a.attnum = 10)
      OR (a.attnum, a.attname, a.atttypid, a.attnotnull, a.attcollation) NOT IN (
        (1, 'id', 23::pg_catalog.oid, true, 0::pg_catalog.oid),
        (2, 'format', 23::pg_catalog.oid, true, 0::pg_catalog.oid),
        (3, 'lineage_uuid', 25::pg_catalog.oid, true, 100::pg_catalog.oid),
        (4, 'deployment_uuid', 25::pg_catalog.oid, true, 100::pg_catalog.oid),
        (5, 'pg_system_identifier', 25::pg_catalog.oid, false, 100::pg_catalog.oid),
        (6, 'pg_database_oid', 25::pg_catalog.oid, false, 100::pg_catalog.oid),
        (7, 'pg_database_name', 25::pg_catalog.oid, false, 100::pg_catalog.oid),
        (8, 'pg_schema_oid', 25::pg_catalog.oid, false, 100::pg_catalog.oid),
        (9, 'pg_schema_name', 25::pg_catalog.oid, false, 100::pg_catalog.oid),
        (10, 'stamped_at', 1184::pg_catalog.oid, true, 0::pg_catalog.oid))))
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_index x WHERE x.indrelid = c.oid) = 1
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_index x
    JOIN pg_catalog.pg_class idx ON idx.oid = x.indexrelid
    JOIN pg_catalog.pg_opclass opclass ON opclass.oid = x.indclass[0]
    JOIN pg_catalog.pg_opfamily family ON family.oid = opclass.opcfamily
    WHERE x.indrelid = c.oid AND idx.oid = $16 AND idx.relnamespace = n.oid
      AND idx.relowner = $6 AND idx.relkind = 'i' AND idx.relpersistence = 'p'
      AND idx.relam = 403::pg_catalog.oid AND opclass.oid = 1978::pg_catalog.oid
      AND family.oid = 1976::pg_catalog.oid
      AND x.indisprimary AND x.indisunique AND x.indisvalid AND x.indisready
      AND x.indislive AND x.indimmediate AND NOT x.indisexclusion
      AND NOT x.indnullsnotdistinct AND x.indnatts = 1 AND x.indnkeyatts = 1
      AND x.indkey[0] = 1 AND x.indcollation[0] = 0::pg_catalog.oid AND x.indoption[0] = 0
      AND x.indexprs IS NULL AND x.indpred IS NULL
      AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_amproc ap
        JOIN pg_catalog.pg_proc f ON f.oid = ap.amproc
        WHERE ap.amprocfamily = family.oid AND f.pronamespace <> 11::pg_catalog.oid)
      AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_amop ao
        JOIN pg_catalog.pg_operator o ON o.oid = ao.amopopr
        JOIN pg_catalog.pg_proc f ON f.oid = o.oprcode
        WHERE ao.amopfamily = family.oid
          AND (o.oprnamespace <> 11::pg_catalog.oid OR f.pronamespace <> 11::pg_catalog.oid)))
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint x WHERE x.conrelid = c.oid) = 2
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x WHERE x.conrelid = c.oid
              AND x.contype = 'p' AND x.conkey = ARRAY[1]::pg_catalog.int2[]
              AND x.conindid = $16 AND x.convalidated AND NOT x.condeferrable)
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x WHERE x.conrelid = c.oid
              AND x.contype = 'c' AND x.conkey = ARRAY[1]::pg_catalog.int2[]
              AND x.convalidated AND NOT x.connoinherit)
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attrdef a WHERE a.adrelid = c.oid) = 1
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend dep
    WHERE ((dep.classid = 'pg_catalog.pg_constraint'::pg_catalog.regclass
            AND dep.objid IN (SELECT x.oid FROM pg_catalog.pg_constraint x WHERE x.conrelid = c.oid))
       OR (dep.classid = 'pg_catalog.pg_attrdef'::pg_catalog.regclass
            AND dep.objid IN (SELECT a.oid FROM pg_catalog.pg_attrdef a WHERE a.adrelid = c.oid)))
      AND NOT (
        (dep.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass AND dep.refobjid = c.oid)
        OR (dep.refclassid = 'pg_catalog.pg_proc'::pg_catalog.regclass AND EXISTS
            (SELECT 1 FROM pg_catalog.pg_proc f WHERE f.oid = dep.refobjid AND f.pronamespace = 11::pg_catalog.oid))
        OR (dep.refclassid = 'pg_catalog.pg_operator'::pg_catalog.regclass AND EXISTS
            (SELECT 1 FROM pg_catalog.pg_operator o WHERE o.oid = dep.refobjid AND o.oprnamespace = 11::pg_catalog.oid))
        OR (dep.refclassid = 'pg_catalog.pg_type'::pg_catalog.regclass AND EXISTS
            (SELECT 1 FROM pg_catalog.pg_type t WHERE t.oid = dep.refobjid AND t.typnamespace = 11::pg_catalog.oid))
        OR (dep.refclassid = 'pg_catalog.pg_collation'::pg_catalog.regclass AND EXISTS
            (SELECT 1 FROM pg_catalog.pg_collation co WHERE co.oid = dep.refobjid AND co.collnamespace = 11::pg_catalog.oid))))
  AND pg_catalog.has_table_privilege(r.oid, c.oid, 'SELECT')
  AND pg_catalog.has_table_privilege(d.oid, c.oid, 'SELECT')
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
      COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) acl
    WHERE acl.grantee NOT IN (c.relowner, r.oid, d.oid)
       OR (acl.grantee IN (r.oid, d.oid) AND
           (acl.grantor <> c.relowner OR acl.is_grantable OR acl.privilege_type <> 'SELECT')))
FROM pg_catalog.pg_database db
JOIN pg_catalog.pg_namespace n ON n.nspname = $4
JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = '_lineage'
JOIN pg_catalog.pg_namespace trusted ON trusted.nspname = $13
JOIN pg_catalog.pg_roles r ON r.rolname = $7
JOIN pg_catalog.pg_roles i ON i.rolname = $9
JOIN pg_catalog.pg_roles d ON d.rolname = $11
WHERE db.datname = $1 AND db.datname = pg_catalog.current_database()
"#;

const EXPRESSION_SIZE_QUERY: &str = r#"
SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x
                   WHERE x.conrelid = $1 AND x.contype = 'c'
                     AND COALESCE(pg_catalog.octet_length(x.conbin::pg_catalog.text) > 4096, true))
   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attrdef a
                   WHERE a.adrelid = $1
                     AND COALESCE(pg_catalog.octet_length(a.adbin::pg_catalog.text) > 4096, true))
"#;

const EXPRESSION_QUERY: &str = r#"
SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x
               WHERE x.conrelid = $1 AND x.contype = 'c'
                 AND pg_catalog.pg_get_expr(x.conbin, x.conrelid) = '(id = 1)')
   AND EXISTS (SELECT 1 FROM pg_catalog.pg_attrdef a
               WHERE a.adrelid = $1 AND a.adnum = 10
                 AND pg_catalog.pg_get_expr(a.adbin, a.adrelid) = 'CURRENT_TIMESTAMP')
"#;
