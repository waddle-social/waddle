//! Catalog-first proof for the separately provisioned installer role.

use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};

use super::super::super::read_only_timeout::timeout_millis;
use super::super::{
    identity_select, parse_snapshot, quote_identifier, verify_identity, PhysicalAttestationConfig,
    PhysicalAttestationError,
};
use super::{
    marker, verify_function, verify_prerequisites, DarkInstallerError, DarkInstallerFence,
    DarkInstallerPins, CONNECT_DEADLINE, FUNCTION_NAME, INSTALL_LOCK_NAMESPACE,
};
use crate::db::lineage::LINEAGE_ADVISORY_LOCK_KEY;

pub(super) async fn begin(
    options: &PgConnectOptions,
    config: &PhysicalAttestationConfig,
    pins: DarkInstallerPins,
    read_only: bool,
) -> Result<(PgPool, Transaction<'static, Postgres>, DarkInstallerFence), DarkInstallerError> {
    if !pins.valid() {
        return Err(DarkInstallerError::InvalidPins);
    }
    let database = &config.postgres().database.name.0;
    if database.is_empty()
        || database.len() > 63
        || database.contains('\0')
        || options.get_username() != config.installer_login.as_str()
        || options.get_username().contains('\0')
        || options.get_database() != Some(database.as_str())
        || options.get_host().contains('\0')
        || options
            .get_application_name()
            .is_some_and(|name| name.contains('\0'))
        || options.get_options().is_some()
    {
        return Err(DarkInstallerError::InvalidConnection);
    }
    // This private one-connection pool is never exposed or reused by runtime
    // code. Dropping a cancelled preparation cannot lend its connection.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(CONNECT_DEADLINE)
        .connect_lazy_with(options.clone());
    let mut tx = tokio::time::timeout(CONNECT_DEADLINE, pool.begin())
        .await
        .map_err(|_| DarkInstallerError::Indeterminate)??;
    let isolation = if read_only {
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY"
    } else {
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE"
    };
    sqlx::query(isolation).execute(&mut *tx).await?;
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *tx)
        .await?;
    let version: String = sqlx::query_scalar("SHOW server_version_num")
        .fetch_one(&mut *tx)
        .await?;
    if !version
        .parse::<u32>()
        .is_ok_and(|version| (170_000..180_000).contains(&version))
    {
        return Err(DarkInstallerError::InvalidConnection);
    }
    for (show, set, cap) in [
        ("SHOW lock_timeout", "SET LOCAL lock_timeout = '5s'", 5_000),
        (
            "SHOW statement_timeout",
            "SET LOCAL statement_timeout = '10s'",
            10_000,
        ),
        (
            "SHOW idle_in_transaction_session_timeout",
            "SET LOCAL idle_in_transaction_session_timeout = '10s'",
            10_000,
        ),
        (
            "SHOW transaction_timeout",
            "SET LOCAL transaction_timeout = '20s'",
            20_000,
        ),
    ] {
        let current: String = sqlx::query_scalar(show).fetch_one(&mut *tx).await?;
        let millis = timeout_millis(&current).ok_or(DarkInstallerError::Indeterminate)?;
        if millis == 0 || millis > cap {
            sqlx::query(set).execute(&mut *tx).await?;
        }
    }
    // No lineage relation or attestor is resolved before these ordered locks.
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(LINEAGE_ADVISORY_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(lock_error)?;
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1, pg_catalog.hashtext($2))")
        .bind(INSTALL_LOCK_NAMESPACE)
        .bind(&config.trusted_schema.0)
        .execute(&mut *tx)
        .await
        .map_err(lock_error)?;
    verify_catalog(&mut tx, config, pins, false).await?;
    let row = sqlx::query(
        "SELECT pg_catalog.pg_backend_pid(), a.backend_start, pg_catalog.pg_postmaster_start_time() \
         FROM pg_catalog.pg_stat_activity a WHERE a.pid = pg_catalog.pg_backend_pid()",
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(DarkInstallerError::Indeterminate)?;
    let fence = DarkInstallerFence {
        operation_id: pins.operation_id,
        backend_pid: row.try_get(0)?,
        backend_started_at: row.try_get(1)?,
        postmaster_started_at: row.try_get(2)?,
    };
    sqlx::query(&format!(
        "SET LOCAL ROLE {}",
        quote_identifier(config.definer_role.as_str())
    ))
    .execute(&mut *tx)
    .await?;
    verify_catalog(&mut tx, config, pins, true).await?;
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}._lineage IN ACCESS SHARE MODE",
        quote_identifier(&config.postgres().schema.name.0)
    ))
    .execute(&mut *tx)
    .await
    .map_err(lock_error)?;
    verify_locked_lineage(&mut tx, config, pins).await?;
    read_identity(&mut tx, config).await?;
    Ok((pool, tx, fence))
}

fn lock_error(error: sqlx::Error) -> DarkInstallerError {
    let code = error
        .as_database_error()
        .and_then(|database| database.code());
    if matches!(code.as_deref(), Some("55P03" | "57014")) {
        DarkInstallerError::Indeterminate
    } else {
        DarkInstallerError::Database(error)
    }
}

pub(super) async fn verify_locked_lineage(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
    pins: DarkInstallerPins,
) -> Result<(), DarkInstallerError> {
    verify_catalog(connection, config, pins, true).await?;
    let held: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
         WHERE l.pid=pg_catalog.pg_backend_pid() AND l.locktype='relation' \
         AND l.relation=$1 AND l.mode='AccessShareLock' AND l.granted)",
    )
    .bind(Oid(pins.lineage_relation_oid))
    .fetch_one(connection)
    .await?;
    if !held {
        return Err(DarkInstallerError::CatalogDrift);
    }
    Ok(())
}

async fn read_identity(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
) -> Result<(), DarkInstallerError> {
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
        return Err(DarkInstallerError::CatalogDrift);
    }
    let row = sqlx::query(&identity_select(config))
        .persistent(false)
        .fetch_optional(connection)
        .await?
        .ok_or(DarkInstallerError::CatalogDrift)?;
    verify_identity(&parse_snapshot(&row)?, config)?;
    Ok(())
}

async fn verify_catalog(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
    pins: DarkInstallerPins,
    assumed_definer: bool,
) -> Result<(), DarkInstallerError> {
    let bounded: bool = sqlx::query_scalar(EARLY_QUERY)
        .bind(&config.postgres().database.name.0)
        .bind(Oid(config.postgres().database.oid))
        .bind(&config.postgres().schema.name.0)
        .bind(Oid(config.postgres().schema.oid))
        .bind(&config.trusted_schema.0)
        .bind(Oid(pins.trusted_schema_oid))
        .bind(Oid(pins.lineage_relation_oid))
        .bind(config.runtime_login.as_str())
        .bind(Oid(pins.runtime_role_oid))
        .bind(config.installer_login.as_str())
        .bind(Oid(pins.installer_role_oid))
        .bind(config.definer_role.as_str())
        .bind(Oid(pins.definer_role_oid))
        .bind(assumed_definer)
        .fetch_optional(&mut *connection)
        .await?
        .unwrap_or(false);
    if !bounded {
        return Err(DarkInstallerError::CatalogDrift);
    }
    match verify_prerequisites(&mut *connection, config).await {
        Ok(()) => {}
        Err(PhysicalAttestationError::Database(error)) => {
            return Err(DarkInstallerError::Database(error));
        }
        Err(_) => return Err(DarkInstallerError::CatalogDrift),
    }
    let exact: bool = sqlx::query_scalar(LINEAGE_QUERY)
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
        .bind(assumed_definer)
        .fetch_optional(&mut *connection)
        .await?
        .unwrap_or(false);
    if !exact {
        return Err(DarkInstallerError::CatalogDrift);
    }
    let bounded_expressions: bool = sqlx::query_scalar(EXPRESSION_SIZE_QUERY)
        .bind(Oid(pins.lineage_relation_oid))
        .fetch_one(&mut *connection)
        .await?;
    if !bounded_expressions {
        return Err(DarkInstallerError::CatalogDrift);
    }
    let exact_expressions: bool = sqlx::query_scalar(EXPRESSION_QUERY)
        .bind(Oid(pins.lineage_relation_oid))
        .fetch_one(&mut *connection)
        .await?;
    if !exact_expressions {
        return Err(DarkInstallerError::CatalogDrift);
    }
    Ok(())
}

pub(super) async fn inspect_function(
    connection: &mut PgConnection,
    config: &PhysicalAttestationConfig,
    pins: DarkInstallerPins,
) -> Result<Option<u32>, DarkInstallerError> {
    let rows = sqlx::query(FUNCTION_METADATA_QUERY)
        .bind(Oid(pins.trusted_schema_oid))
        .bind(FUNCTION_NAME)
        .fetch_all(&mut *connection)
        .await?;
    if rows.is_empty() {
        return Ok(None);
    }
    if rows.len() != 1 || !rows[0].try_get::<bool, _>(1)? {
        return Err(DarkInstallerError::Conflict);
    }
    let oid = rows[0].try_get::<Oid, _>(0)?.0;
    if pins
        .existing_function_oid
        .is_some_and(|expected| expected != oid)
    {
        return Err(DarkInstallerError::Conflict);
    }
    match verify_function(connection, config, &marker(config, pins.operation_id)).await {
        Ok(()) => {}
        Err(PhysicalAttestationError::Database(error)) => {
            return Err(DarkInstallerError::Database(error));
        }
        Err(_) => return Err(DarkInstallerError::Conflict),
    }
    Ok(Some(oid))
}

// Guard raw ACL sizes and the simple role graph before any ACL-expanding or
// effective-privilege helper can inspect attacker-provisioned metadata.
const EARLY_QUERY: &str = r#"
SELECT db.oid = $2 AND n.oid = $4 AND trusted.oid = $6 AND c.oid = $7
  AND r.oid = $9 AND i.oid = $11 AND d.oid = $13
  AND i.rolname = session_user AND current_user = CASE WHEN $14 THEN d.rolname ELSE i.rolname END
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
              WHERE m.roleid=d.oid AND m.member=i.oid
                AND m.set_option AND NOT m.inherit_option AND NOT m.admin_option)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
    WHERE m.member IN (d.oid,r.oid)
       OR (m.member=i.oid AND m.roleid<>d.oid)
       OR m.roleid IN (i.oid,r.oid)
       OR (m.roleid=d.oid AND m.member<>i.oid))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_event_trigger t WHERE t.evtenabled <> 'D')
  AND (db.datacl IS NULL OR (pg_catalog.pg_column_compression(db.datacl) IS NULL
                             AND pg_catalog.pg_column_size(db.datacl) <= 4096))
  AND (n.nspacl IS NULL OR (pg_catalog.pg_column_compression(n.nspacl) IS NULL
                            AND pg_catalog.pg_column_size(n.nspacl) <= 4096))
  AND (trusted.nspacl IS NULL OR (pg_catalog.pg_column_compression(trusted.nspacl) IS NULL
                                  AND pg_catalog.pg_column_size(trusted.nspacl) <= 4096))
  AND (c.relacl IS NULL OR (pg_catalog.pg_column_compression(c.relacl) IS NULL
                            AND pg_catalog.pg_column_size(c.relacl) <= 4096))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
    WHERE a.attrelid=c.oid AND a.attacl IS NOT NULL
      AND (pg_catalog.pg_column_compression(a.attacl) IS NOT NULL
           OR pg_catalog.pg_column_size(a.attacl) > 4096))
  AND (control.proacl IS NULL OR (pg_catalog.pg_column_compression(control.proacl) IS NULL
                                  AND pg_catalog.pg_column_size(control.proacl) <= 4096))
FROM pg_catalog.pg_database db
JOIN pg_catalog.pg_namespace n ON n.nspname=$3
JOIN pg_catalog.pg_class c ON c.relnamespace=n.oid AND c.relname='_lineage'
JOIN pg_catalog.pg_namespace trusted ON trusted.nspname=$5
JOIN pg_catalog.pg_proc control ON control.oid=3441::pg_catalog.oid
JOIN pg_catalog.pg_roles r ON r.rolname=$8
JOIN pg_catalog.pg_roles i ON i.rolname=$10
JOIN pg_catalog.pg_roles d ON d.rolname=$12
WHERE db.datname=$1 AND db.datname=pg_catalog.current_database()
"#;

// Function absence is allowed during the initial installation. If a name is
// present, bound all fields consumed by verify_function before it deparses or
// compares them. LIMIT 2 distinguishes one exact function from overloads.
const FUNCTION_METADATA_QUERY: &str = r#"
SELECT p.oid, COALESCE(
  (p.proacl IS NULL OR (pg_catalog.pg_column_compression(p.proacl) IS NULL
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
  AND mark.description IS NOT NULL
  AND pg_catalog.pg_column_compression(mark.description) IS NULL
  AND pg_catalog.pg_column_size(mark.description) <= 4096, false)
FROM pg_catalog.pg_proc p
LEFT JOIN pg_catalog.pg_description mark ON mark.objoid=p.oid
  AND mark.classoid='pg_catalog.pg_proc'::pg_catalog.regclass AND mark.objsubid=0
WHERE p.pronamespace=$1 AND p.proname=$2
LIMIT 2
"#;

const LINEAGE_QUERY: &str = r#"
SELECT db.oid = $2 AND db.datdba = $3 AND NOT pg_catalog.pg_is_in_recovery()
  AND n.oid = $5 AND n.nspowner = $6 AND c.oid = $15 AND c.relowner = $6
  AND r.oid = $8 AND i.oid = $10 AND d.oid = $12
  AND i.rolname = session_user AND current_user = CASE WHEN $17 THEN d.rolname ELSE i.rolname END
  AND trusted.oid = $14 AND trusted.nspowner = d.oid
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
  AND NOT pg_catalog.has_table_privilege(i.oid, c.oid, 'SELECT')
  AND NOT pg_catalog.has_any_column_privilege(i.oid, c.oid, 'SELECT')
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
