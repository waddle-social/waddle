//! Read-only, deliberately unwired diagnostics for two exact epoch-guard stages.

use sqlx::postgres::{PgConnectOptions, PgConnection, PgPoolOptions};
use sqlx::Connection;

use super::{
    check_migration_login_and_assume_owner, check_migrator_options, DarkMigrationOwnerConfig,
    DarkMigrationOwnerError, CONNECT_DEADLINE, OPERATION_DEADLINE,
};
use crate::config::LineageConfig;
use crate::db::lineage;
use crate::db::migrations::waddle::{
    V1009_INERT_INGRESS_EPOCH_GUARDS_POSTGRES, V1028_INGRESS_SOURCE_BINDING_POSTGRES,
};
use crate::db::physical_attestation::PgRoleName;
use crate::db::{Database, DatabaseDriver, IntoParams, Transaction};
use crate::ingress_substrate::{supported_protocol_epoch, EPOCH_GUARDED_TABLES};

// The candidate is a laboratory asset, not a registered migration. Its single
// source is shared with the tests and included in the release source fileset.
pub(super) const RELATION_GUARD_CANDIDATE: &str =
    include_str!("ingress_epoch_relation_guard_candidate.sql");

/// An observation of installed guards and scoped epoch ACLs over a scan.
///
/// This is not a writer capability, migration admission, authorization, or a
/// fence. A positive finding requires independently quiescent DDL, role, ACL,
/// epoch, lineage, ledger and manifest mutations throughout the scan; this
/// function cannot establish quiescence or an atomic snapshot of PostgreSQL's
/// privilege lookups. Catalogs, privileges, credentials and connections can
/// change immediately afterward. The checks do not audit arbitrary
/// security-definer functions or prevent a runtime from forging epoch GUCs.
/// The pinned runtime session must use the origin replication role and the
/// runtime role must lack effective SET privilege for that parameter.
/// They do not audit ACLs on the thirteen guarded data tables or establish
/// application attachment or write readiness. Internal triggers on guarded
/// data tables, such as foreign-key triggers, are outside this inspection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DarkEpochAclStage {
    /// The original V1009 guard at epoch zero and the scoped direct/effective
    /// ACLs match SELECT plus UPDATE(id), needed for its `FOR SHARE`.
    V1009EpochZero,
    /// The exact experimental relation guard and scoped ACLs match SELECT
    /// without UPDATE.
    RelationGuardEpochZero,
}

/// Inspect the epoch and ledger objects using separate, live migrator and runtime
/// logins. The migrator assumes the NOLOGIN owner and verifies physical lineage.
/// The pinned runtime backend is correlated through `pg_stat_activity` using
/// its PID and an internally generated application-name nonce; insufficient
/// visibility or any mismatch fails closed. This correlation assumes honest
/// endpoints and no adversarial replay; it is not physical attestation of the
/// runtime connection. The owner needs the same identity-read permissions as
/// the dark migration owner. The configured migrator is independent of the DSN.
///
/// Only the complete current migration catalog and the two exact guard bodies
/// are recognized. Unknown or changed inspected epoch/catalog objects and
/// nonzero epochs are refused. The thirteen guarded `ingress_*` tables plus the
/// epoch and manifest tables must match the live non-index relation set. This
/// diagnostic performs no grants, DDL, migration, or epoch transition.
pub async fn inspect_dark_postgres_epoch_acl(
    migrator_options: &PgConnectOptions,
    runtime_options: &PgConnectOptions,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) -> Result<DarkEpochAclStage, DarkMigrationOwnerError> {
    tokio::time::timeout(
        OPERATION_DEADLINE,
        inspect_with_connections(migrator_options, runtime_options, config, migrator),
    )
    .await
    .map_err(|_| DarkMigrationOwnerError::PreflightTimeout)?
}

async fn inspect_with_connections(
    migrator_options: &PgConnectOptions,
    runtime_options: &PgConnectOptions,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) -> Result<DarkEpochAclStage, DarkMigrationOwnerError> {
    check_migrator_options(migrator_options, config, migrator)?;
    if runtime_options.get_username() != config.runtime.as_str() {
        return Err(DarkMigrationOwnerError::Roles);
    }

    let nonce = format!("waddle-epoch-acl-{}", uuid::Uuid::new_v4().simple());
    let mut runtime = tokio::time::timeout(
        CONNECT_DEADLINE,
        PgConnection::connect_with(&runtime_options.clone().application_name(&nonce)),
    )
    .await
    .map_err(|_| DarkMigrationOwnerError::ConnectTimeout)??;
    // Hold one transaction across the entire observation, including both
    // correlations, so a transaction pooler cannot silently swap backends.
    let mut runtime_tx = tokio::time::timeout(CONNECT_DEADLINE, runtime.begin())
        .await
        .map_err(|_| DarkMigrationOwnerError::ConnectTimeout)??;
    match tokio::time::timeout(
        CONNECT_DEADLINE,
        sqlx::query("SET TRANSACTION READ ONLY").execute(&mut *runtime_tx),
    )
    .await
    {
        Ok(result) => {
            result?;
        }
        Err(_) => {
            // This connection is never pooled. A cancelled statement leaves
            // its backend outcome uncertain, so discard the whole session.
            drop(runtime_tx);
            let _ = tokio::time::timeout(CONNECT_DEADLINE, runtime.close_hard()).await;
            return Err(DarkMigrationOwnerError::PreflightTimeout);
        }
    }
    let pid = tokio::time::timeout(
        CONNECT_DEADLINE,
        runtime_pid(&mut runtime_tx, config, &nonce),
    )
    .await
    .map_err(|_| DarkMigrationOwnerError::ConnectTimeout)??;

    let pool = tokio::time::timeout(
        CONNECT_DEADLINE,
        PgPoolOptions::new()
            .max_connections(1)
            .connect_with(migrator_options.clone()),
    )
    .await
    .map_err(|_| DarkMigrationOwnerError::ConnectTimeout)??;
    let db = Database::from_postgres_pool("dark epoch ACL preflight", pool.clone());
    // Keep the runtime connection alive while its backend is correlated.
    let result = tokio::time::timeout(
        OPERATION_DEADLINE,
        inspect_in_transaction(&db, &mut runtime_tx, config, migrator, pid, &nonce),
    )
    .await
    .map_err(|_| DarkMigrationOwnerError::PreflightTimeout)
    .and_then(|result| result);
    drop(db);
    let _ = tokio::time::timeout(CONNECT_DEADLINE, pool.close()).await;
    let _ = tokio::time::timeout(CONNECT_DEADLINE, runtime_tx.rollback()).await;
    drop(runtime);
    result
}

async fn inspect_in_transaction(
    db: &Database,
    runtime_tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
    runtime_pid: i32,
    nonce: &str,
) -> Result<DarkEpochAclStage, DarkMigrationOwnerError> {
    let expected = config
        .expected
        .postgres_identity
        .as_ref()
        .ok_or(DarkMigrationOwnerError::MissingPostgresIdentity)?;
    let schema = &expected.schema.name.0;
    let quoted = format!("\"{}\"", schema.replace('"', "\"\""));
    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED, READ ONLY",
        (),
    )
    .await?;
    let mut rows = tx
        .query(
            "SELECT (EXTRACT(EPOCH FROM pg_catalog.current_setting('lock_timeout')::interval) * 1000)::bigint, \
             (EXTRACT(EPOCH FROM pg_catalog.current_setting('statement_timeout')::interval) * 1000)::bigint",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(DarkMigrationOwnerError::EpochCatalogDrift)?;
    let current_lock: i64 = row.get(0)?;
    let current_statement: i64 = row.get(1)?;
    let bounded = |current: i64, limit: i64| {
        if current <= 0 {
            limit
        } else {
            current.min(limit)
        }
    };
    tx.execute(
        "SELECT pg_catalog.set_config('lock_timeout', ?, true), \
         pg_catalog.set_config('statement_timeout', ?, true), \
         pg_catalog.set_config('search_path', ?, true)",
        crate::db_params![
            format!("{}ms", bounded(current_lock, 3_000)),
            format!("{}ms", bounded(current_statement, 30_000)),
            format!("{quoted}, pg_temp")
        ],
    )
    .await?;
    check_migration_login_and_assume_owner(
        &mut tx,
        config,
        migrator,
        schema,
        i64::from(expected.schema.oid),
    )
    .await?;
    let lineage_config = LineageConfig {
        deployment_uuid: Some(config.expected.deployment_uuid),
        action: None,
    };
    let observed = lineage::verify_qualified_in_transaction(
        &mut tx,
        DatabaseDriver::Postgres,
        &lineage_config,
        &format!("{quoted}._lineage"),
    )
    .await?;
    if observed != config.expected {
        return Err(DarkMigrationOwnerError::Identity);
    }

    correlate_runtime(&mut tx, config, runtime_pid, nonce).await?;
    let stage = inspect_catalog_in_transaction(&mut tx, config, schema, &quoted).await?;
    if self::runtime_pid(runtime_tx, config, nonce).await? != runtime_pid {
        return Err(DarkMigrationOwnerError::RuntimeSessionUnverified);
    }
    correlate_runtime(&mut tx, config, runtime_pid, nonce).await?;
    // A fresh READ COMMITTED statement catches a transition committed while
    // the first epoch query waited. It does not fence a subsequent transition.
    check_epoch_zero(&mut tx, &quoted).await?;
    tx.rollback().await?;
    Ok(stage)
}

// This deliberately checks only the V1009 epoch relation and runtime ACL. The
// caller separately validates the applied ledger and holds the epoch AX lock;
// this does not establish old-session fencing or audit SECURITY DEFINER code.
pub(super) async fn verify_v1009_epoch_owner_stage(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
    quoted: &str,
) -> Result<(), DarkMigrationOwnerError> {
    require(
        tx,
        "SELECT pg_catalog.current_setting('transaction_isolation') = 'read committed' \
         AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
                     JOIN pg_catalog.pg_class c ON c.oid = l.relation \
                     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = ? AND c.relname = 'ingress_protocol_epoch' \
                       AND l.locktype = 'relation' AND l.pid = pg_catalog.pg_backend_pid() \
                       AND l.mode = 'AccessExclusiveLock' AND l.granted)",
        crate::db_params![schema],
        DarkMigrationOwnerError::EpochCatalogDrift,
    )
    .await?;
    check_runtime_role(tx, config, schema).await?;
    check_epoch_shape(tx, config, schema).await?;
    let epoch = function_source(tx, config, schema, "waddle_ingress_protocol_epoch_guard").await?;
    if Some(epoch.source.as_str())
        != source_body(
            V1009_INERT_INGRESS_EPOCH_GUARDS_POSTGRES,
            "waddle_ingress_protocol_epoch_guard",
        )
    {
        return Err(DarkMigrationOwnerError::EpochCatalogDrift);
    }
    check_trigger_pair(
        tx,
        config,
        schema,
        "ingress_protocol_epoch",
        TriggerPair {
            row_function: epoch.oid,
            truncate_function: epoch.oid,
            row_type: 31,
            reject_internal: true,
        },
    )
    .await?;
    check_acl(
        tx,
        config,
        schema,
        "ingress_protocol_epoch",
        DarkEpochAclStage::V1009EpochZero,
    )
    .await?;
    check_epoch_zero(tx, quoted).await
}

// Shared by the read-only diagnostic and the test-only owner transition lab.
// The caller owns the transaction and must independently establish identity,
// role separation and any needed operational quiescence.
pub(super) async fn inspect_catalog_in_transaction(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
    quoted: &str,
) -> Result<DarkEpochAclStage, DarkMigrationOwnerError> {
    inspect_catalog_with_epoch_policy(tx, config, schema, quoted, true).await
}

/// Verify the exact candidate guard on a supported live epoch. The public
/// dark diagnostic remains restricted to epoch zero; a capability verifier
/// must also be able to re-attest after the disposable owner transition.
pub(super) async fn verify_relation_guard_candidate_in_transaction(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
    quoted: &str,
) -> Result<(), DarkMigrationOwnerError> {
    let stage = inspect_catalog_with_epoch_policy(tx, config, schema, quoted, false).await?;
    if stage == DarkEpochAclStage::RelationGuardEpochZero {
        check_guarded_relation_acls(tx, config, schema).await
    } else {
        Err(DarkMigrationOwnerError::EpochCatalogDrift)
    }
}

async fn inspect_catalog_with_epoch_policy(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
    quoted: &str,
    require_epoch_zero: bool,
) -> Result<DarkEpochAclStage, DarkMigrationOwnerError> {
    check_runtime_role(tx, config, schema).await?;
    check_epoch_shape(tx, config, schema).await?;
    super::MigrationRunner::single()
        .verify_complete_in_transaction(
            tx,
            DatabaseDriver::Postgres,
            &format!("{quoted}._migrations"),
        )
        .await?;

    let dml = function_source(tx, config, schema, "waddle_ingress_epoch_guard").await?;
    let epoch = function_source(tx, config, schema, "waddle_ingress_protocol_epoch_guard").await?;
    let truncate = function_source(tx, config, schema, "waddle_ingress_truncate_guard").await?;
    let manifest = function_source(
        tx,
        config,
        schema,
        "waddle_ingress_epoch_guard_manifest_append_only",
    )
    .await?;
    let v1009 = V1009_INERT_INGRESS_EPOCH_GUARDS_POSTGRES;
    if Some(truncate.source.as_str()) != source_body(v1009, "waddle_ingress_truncate_guard")
        || Some(manifest.source.as_str())
            != source_body(v1009, "waddle_ingress_epoch_guard_manifest_append_only")
    {
        return Err(DarkMigrationOwnerError::EpochCatalogDrift);
    }
    let matches = |source: &str| {
        Some(dml.source.as_str()) == source_body(source, "waddle_ingress_epoch_guard")
            && Some(epoch.source.as_str())
                == source_body(source, "waddle_ingress_protocol_epoch_guard")
    };
    let stage = if matches(v1009) {
        DarkEpochAclStage::V1009EpochZero
    } else if matches(RELATION_GUARD_CANDIDATE) {
        DarkEpochAclStage::RelationGuardEpochZero
    } else {
        return Err(DarkMigrationOwnerError::EpochCatalogDrift);
    };

    check_trigger_pair(
        tx,
        config,
        schema,
        "ingress_protocol_epoch",
        TriggerPair {
            row_function: epoch.oid,
            truncate_function: epoch.oid,
            row_type: 31,
            reject_internal: true,
        },
    )
    .await?;
    check_trigger_pair(
        tx,
        config,
        schema,
        "ingress_epoch_guard_manifest",
        TriggerPair {
            row_function: manifest.oid,
            truncate_function: manifest.oid,
            row_type: 31,
            reject_internal: true,
        },
    )
    .await?;
    let mut rows = tx
        .query(
            &format!(
                "SELECT table_name FROM {quoted}.ingress_epoch_guard_manifest ORDER BY table_name"
            ),
            (),
        )
        .await?;
    let mut live = Vec::new();
    while let Some(row) = rows.next().await? {
        live.push(row.get::<String>(0)?);
    }
    let mut expected_tables = EPOCH_GUARDED_TABLES.to_vec();
    expected_tables.sort_unstable();
    if live.iter().map(String::as_str).ne(expected_tables) {
        return Err(DarkMigrationOwnerError::EpochCatalogDrift);
    }
    let mut rows = tx
        .query(
            "SELECT c.relname::text, c.relkind::text FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND pg_catalog.left(c.relname, 8) = 'ingress_' \
               AND c.relkind NOT IN ('i', 'I') ORDER BY c.relname",
            crate::db_params![schema],
        )
        .await?;
    let mut live_relations = Vec::new();
    while let Some(row) = rows.next().await? {
        let kind: String = row.get(1)?;
        if kind != "r" {
            return Err(DarkMigrationOwnerError::EpochCatalogDrift);
        }
        live_relations.push(row.get::<String>(0)?);
    }
    let mut expected_relations = EPOCH_GUARDED_TABLES.to_vec();
    expected_relations.extend(["ingress_epoch_guard_manifest", "ingress_protocol_epoch"]);
    expected_relations.sort_unstable();
    if live_relations
        .iter()
        .map(String::as_str)
        .ne(expected_relations)
    {
        return Err(DarkMigrationOwnerError::EpochCatalogDrift);
    }
    for table in EPOCH_GUARDED_TABLES {
        check_trigger_pair(
            tx,
            config,
            schema,
            table,
            TriggerPair {
                row_function: dml.oid,
                truncate_function: truncate.oid,
                row_type: 30,
                reject_internal: false,
            },
        )
        .await?;
    }

    for table in [
        "ingress_protocol_epoch",
        "_migrations",
        "_lineage",
        "ingress_epoch_guard_manifest",
    ] {
        check_acl(tx, config, schema, table, stage).await?;
    }
    if require_epoch_zero {
        check_epoch_zero(tx, quoted).await?;
    } else {
        check_supported_epoch(tx, config, quoted).await?;
    }
    Ok(stage)
}

async fn check_epoch_zero(
    tx: &mut Transaction<'_>,
    quoted_schema: &str,
) -> Result<(), DarkMigrationOwnerError> {
    require(
        tx,
        &format!(
            "SELECT count(*) = 1 AND bool_and(id = 1 AND epoch = 0 \
             AND activated_at IS NULL AND lineage_uuid IS NULL) \
             FROM {quoted_schema}.ingress_protocol_epoch"
        ),
        (),
        DarkMigrationOwnerError::UnsupportedEpoch,
    )
    .await
}

async fn check_supported_epoch(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    quoted_schema: &str,
) -> Result<(), DarkMigrationOwnerError> {
    let supported = i64::from(supported_protocol_epoch().to_storage());
    require(
        tx,
        &format!(
            "SELECT count(*) = 1 AND bool_and(id = 1 AND epoch >= 0 AND epoch <= ? \
             AND ((epoch = 0 AND activated_at IS NULL AND lineage_uuid IS NULL) \
               OR (epoch > 0 AND activated_at IS NOT NULL AND lineage_uuid::text = ?))) \
             FROM {quoted_schema}.ingress_protocol_epoch"
        ),
        crate::db_params![supported, config.expected.lineage_uuid.0.to_string()],
        DarkMigrationOwnerError::UnsupportedEpoch,
    )
    .await
}

pub(super) async fn runtime_pid(
    runtime: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    config: &DarkMigrationOwnerConfig,
    nonce: &str,
) -> Result<i32, DarkMigrationOwnerError> {
    let (session, current, pid, application, origin): (String, String, i32, String, bool) =
        sqlx::query_as(
            "SELECT session_user::text, current_user::text, pg_catalog.pg_backend_pid(), \
             pg_catalog.current_setting('application_name'), \
             pg_catalog.current_setting('session_replication_role') = 'origin'",
        )
        .fetch_one(&mut **runtime)
        .await?;
    if session != config.runtime.as_str()
        || current != config.runtime.as_str()
        || application != nonce
        || !origin
    {
        return Err(DarkMigrationOwnerError::RuntimeSessionUnverified);
    }
    Ok(pid)
}

pub(super) async fn correlate_runtime(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    pid: i32,
    nonce: &str,
) -> Result<(), DarkMigrationOwnerError> {
    // These four fields are visible to an ordinary non-superuser on PG17.
    // No inference is made from restricted pg_stat_activity fields.
    // pg_stat_activity otherwise can reuse a transaction-local stats snapshot.
    tx.execute("SELECT pg_catalog.pg_stat_clear_snapshot()", ())
        .await?;
    require(
        tx,
        "SELECT count(*) = 1 FROM pg_catalog.pg_stat_activity \
         WHERE pid = ? AND usename = ? AND application_name = ? \
           AND datname = pg_catalog.current_database()",
        crate::db_params![pid, config.runtime.as_str(), nonce],
        DarkMigrationOwnerError::RuntimeSessionUnverified,
    )
    .await
}

pub(super) fn source_body<'a>(sql: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("FUNCTION {name}() RETURNS trigger");
    let (_, declaration) = sql.split_once(&marker)?;
    let (_, body) = declaration.split_once("AS $$")?;
    let (body, _) = body.split_once("$$;")?;
    Some(body)
}

async fn require(
    tx: &mut Transaction<'_>,
    sql: &str,
    params: impl IntoParams,
    error: DarkMigrationOwnerError,
) -> Result<(), DarkMigrationOwnerError> {
    let mut rows = tx.query(sql, params).await?;
    match rows.next().await? {
        Some(row) if row.get::<bool>(0)? => Ok(()),
        _ => Err(error),
    }
}

async fn check_runtime_role(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
) -> Result<(), DarkMigrationOwnerError> {
    // Any membership is refused, including NOINHERIT memberships that
    // permit SET ROLE. This intentionally conservative rule avoids trying
    // to model a changing graph of potentially privileged roles.
    require(
        tx,
        r#"SELECT r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreaterole
              AND NOT r.rolcreatedb AND NOT r.rolreplication AND NOT r.rolbypassrls
              AND r.oid <> d.datdba
              AND NOT pg_catalog.has_database_privilege(r.oid, d.oid, 'CREATE,TEMP')
              AND NOT pg_catalog.has_parameter_privilege(r.oid, 'session_replication_role', 'SET')
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member = r.oid)
              AND pg_catalog.has_schema_privilege(r.oid, n.oid, 'USAGE')
              AND NOT pg_catalog.has_schema_privilege(r.oid, n.oid, 'CREATE')
              AND NOT EXISTS (
                  SELECT 1 FROM pg_catalog.aclexplode(
                      coalesce(n.nspacl, pg_catalog.acldefault('n', n.nspowner))
                  ) a WHERE a.grantee = 0 OR (a.grantee = r.oid AND a.is_grantable)
              )
           FROM pg_catalog.pg_roles r
           JOIN pg_catalog.pg_namespace n ON n.nspname = ?
           JOIN pg_catalog.pg_database d ON d.datname = pg_catalog.current_database()
           WHERE r.rolname = ?"#,
        (schema, config.runtime.as_str()),
        DarkMigrationOwnerError::Roles,
    )
    .await
}

async fn check_epoch_shape(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
) -> Result<(), DarkMigrationOwnerError> {
    require(
        tx,
        r#"SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND c.relowner = o.oid
              AND NOT c.relispartition AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity
              AND NOT c.relhasrules
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i
                              WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
              AND (SELECT count(*) FROM pg_catalog.pg_attribute a
                   WHERE a.attrelid = c.oid AND a.attnum > 0) = 4
              AND (SELECT count(*) FROM pg_catalog.pg_attribute a
                   LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = c.oid AND d.adnum = a.attnum
                   WHERE a.attrelid = c.oid AND NOT a.attisdropped
                     AND a.attgenerated = '' AND a.attidentity = ''
                     AND ((a.attnum = 1 AND a.attname = 'id'
                           AND a.atttypid = 'pg_catalog.int4'::regtype AND a.attnotnull
                           AND d.oid IS NULL)
                       OR (a.attnum = 2 AND a.attname = 'epoch'
                           AND a.atttypid = 'pg_catalog.int8'::regtype AND a.attnotnull
                           AND pg_catalog.pg_get_expr(d.adbin, d.adrelid) = '0')
                       OR (a.attnum = 3 AND a.attname = 'activated_at'
                           AND a.atttypid = 'pg_catalog.timestamptz'::regtype AND NOT a.attnotnull
                           AND d.oid IS NULL)
                       OR (a.attnum = 4 AND a.attname = 'lineage_uuid'
                           AND a.atttypid = 'pg_catalog.uuid'::regtype AND NOT a.attnotnull
                           AND d.oid IS NULL))) = 4
              AND (SELECT count(*) FROM pg_catalog.pg_constraint x WHERE x.conrelid = c.oid) = 4
              AND (SELECT count(DISTINCT pg_catalog.pg_get_expr(x.conbin, x.conrelid)) FROM pg_catalog.pg_constraint x
                   WHERE x.conrelid = c.oid AND x.contype = 'c' AND x.convalidated
                     AND NOT x.connoinherit AND x.conislocal
                     AND pg_catalog.pg_get_expr(x.conbin, x.conrelid) IN (
                         '(id = 1)',
                         '((epoch >= 0) AND (epoch <= ''4294967295''::bigint))',
                         '((epoch = 0) = ((activated_at IS NULL) AND (lineage_uuid IS NULL)))'
                     )) = 3
              AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x
                          JOIN pg_catalog.pg_index i ON i.indexrelid = x.conindid
                          WHERE x.conrelid = c.oid AND x.contype = 'p' AND x.convalidated
                            AND NOT x.condeferrable AND x.conkey = ARRAY[1]::smallint[]
                            AND i.indisvalid AND i.indisready AND i.indisunique)
           FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_catalog.pg_roles o ON o.rolname = ?
           WHERE n.nspname = ? AND c.relname = 'ingress_protocol_epoch'"#,
        (config.owner.as_str(), schema),
        DarkMigrationOwnerError::EpochCatalogDrift,
    )
    .await
}

pub(super) struct GuardFunction {
    pub(super) oid: i64,
    pub(super) source: String,
}

pub(super) async fn function_source(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
    name: &str,
) -> Result<GuardFunction, DarkMigrationOwnerError> {
    let mut rows = tx
        .query(
            r#"SELECT p.oid::bigint, p.prosrc FROM pg_catalog.pg_proc p
               JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
               JOIN pg_catalog.pg_roles o ON o.rolname = ?
               JOIN pg_catalog.pg_language l ON l.oid = p.prolang
               WHERE n.nspname = ? AND p.proname = ? AND p.proowner = o.oid
                 AND p.prokind = 'f' AND p.pronargs = 0 AND p.provariadic = 0
                 AND p.prorettype = 'pg_catalog.trigger'::regtype AND NOT p.proretset
                 AND l.lanname = 'plpgsql' AND p.provolatile = 'v' AND p.proparallel = 'u'
                 AND NOT p.prosecdef AND NOT p.proleakproof AND NOT p.proisstrict
                 AND p.prosupport = 0 AND p.procost = 100 AND p.prorows = 0
                 AND p.proconfig = ARRAY['search_path=pg_catalog']::text[]"#,
            crate::db_params![config.owner.as_str(), schema, name],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(DarkMigrationOwnerError::EpochCatalogDrift)?;
    Ok(GuardFunction {
        oid: row.get(0)?,
        source: row.get(1)?,
    })
}

struct TriggerPair {
    row_function: i64,
    truncate_function: i64,
    row_type: i32,
    reject_internal: bool,
}

struct AdditionalTrigger {
    name: &'static str,
    function: &'static str,
    trigger_type: i32,
}

// V1028 adds ordinary row triggers to these two epoch-guarded tables. Their
// presence must not hide a missing guard or admit an unrelated trigger.
const MESSAGE_TRIGGERS: &[AdditionalTrigger] = &[
    AdditionalTrigger {
        name: "ingress_messages_source_binding_guard",
        function: "waddle_ingress_source_binding_guard",
        trigger_type: 19,
    },
    AdditionalTrigger {
        name: "ingress_messages_source_insert_guard",
        function: "waddle_ingress_source_insert_guard",
        trigger_type: 7,
    },
];
const OWNER_REJECTION_TRIGGERS: &[AdditionalTrigger] = &[
    AdditionalTrigger {
        name: "ingress_owner_rejections_update_guard",
        function: "waddle_ingress_owner_rejection_immutable",
        trigger_type: 19,
    },
    AdditionalTrigger {
        name: "ingress_owner_rejections_insert_guard",
        function: "waddle_ingress_owner_rejection_insert_guard",
        trigger_type: 7,
    },
    AdditionalTrigger {
        name: "ingress_owner_rejections_delete_guard",
        function: "waddle_ingress_owner_rejection_delete_guard",
        trigger_type: 11,
    },
];

async fn check_trigger_pair(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
    table: &str,
    pair: TriggerPair,
) -> Result<(), DarkMigrationOwnerError> {
    let additional = match table {
        "ingress_messages" => MESSAGE_TRIGGERS,
        "ingress_owner_rejections" => OWNER_REJECTION_TRIGGERS,
        _ => &[],
    };
    let (row_name, truncate_name) = match table {
        "ingress_protocol_epoch" => (
            "ingress_protocol_epoch_guard_row".to_string(),
            "ingress_protocol_epoch_guard_truncate".to_string(),
        ),
        "ingress_epoch_guard_manifest" => (
            "ingress_epoch_guard_manifest_append_only_row".to_string(),
            "ingress_epoch_guard_manifest_append_only_truncate".to_string(),
        ),
        _ => (
            format!("{table}_epoch_guard_dml"),
            format!("{table}_epoch_guard_truncate"),
        ),
    };
    require(
        tx,
        r#"SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND c.relowner = o.oid
              AND NOT c.relispartition AND NOT c.relhasrules
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i
                              WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
              AND (SELECT count(*) FROM pg_catalog.pg_trigger t
                   WHERE t.tgrelid = c.oid AND (NOT t.tgisinternal OR ? <> 0)) = ?
              AND EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t
                          WHERE t.tgrelid = c.oid AND t.tgname = ? AND t.tgtype = ?
                            AND t.tgfoid::bigint = ?
                            AND NOT t.tgisinternal AND t.tgenabled = 'A' AND t.tgparentid = 0
                            AND t.tgconstraint = 0 AND NOT t.tgdeferrable AND NOT t.tginitdeferred
                            AND t.tgnargs = 0 AND t.tgattr::text = '' AND t.tgqual IS NULL
                            AND t.tgoldtable IS NULL AND t.tgnewtable IS NULL)
              AND EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t
                          WHERE t.tgrelid = c.oid AND t.tgname = ? AND t.tgtype = 34
                            AND t.tgfoid::bigint = ?
                            AND NOT t.tgisinternal AND t.tgenabled = 'A' AND t.tgparentid = 0
                            AND t.tgconstraint = 0 AND NOT t.tgdeferrable AND NOT t.tginitdeferred
                            AND t.tgnargs = 0 AND t.tgattr::text = '' AND t.tgqual IS NULL
                            AND t.tgoldtable IS NULL AND t.tgnewtable IS NULL)
           FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_catalog.pg_roles o ON o.rolname = ?
           WHERE n.nspname = ? AND c.relname = ?"#,
        crate::db_params![
            pair.reject_internal,
            2_i64 + additional.len() as i64,
            row_name,
            pair.row_type,
            pair.row_function,
            truncate_name,
            pair.truncate_function,
            config.owner.as_str(),
            schema,
            table
        ],
        DarkMigrationOwnerError::EpochCatalogDrift,
    )
    .await?;

    for trigger in additional {
        let function = function_source(tx, config, schema, trigger.function).await?;
        if Some(function.source.as_str())
            != source_body(V1028_INGRESS_SOURCE_BINDING_POSTGRES, trigger.function)
        {
            return Err(DarkMigrationOwnerError::EpochCatalogDrift);
        }
        require(
            tx,
            r#"SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t
                 JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname = ? AND c.relname = ? AND t.tgname = ? AND t.tgtype = ?
                   AND t.tgfoid::bigint = ?
                   AND NOT t.tgisinternal AND t.tgenabled = 'O' AND t.tgparentid = 0
                   AND t.tgconstraint = 0 AND NOT t.tgdeferrable AND NOT t.tginitdeferred
                   AND t.tgnargs = 0 AND t.tgattr::text = '' AND t.tgqual IS NULL
                   AND t.tgoldtable IS NULL AND t.tgnewtable IS NULL)"#,
            crate::db_params![
                schema,
                table,
                trigger.name,
                trigger.trigger_type,
                function.oid
            ],
            DarkMigrationOwnerError::EpochCatalogDrift,
        )
        .await?;
    }
    Ok(())
}

async fn check_acl(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
    table: &str,
    stage: DarkEpochAclStage,
) -> Result<(), DarkMigrationOwnerError> {
    let epoch = table == "ingress_protocol_epoch";
    let id_update = epoch && stage == DarkEpochAclStage::V1009EpochZero;
    require(
        tx,
        r#"SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND c.relowner = o.oid
              AND NOT c.relispartition AND NOT c.relhasrules
              AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i
                              WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
              AND (? = 0 OR pg_catalog.has_table_privilege(r.oid, c.oid, 'SELECT'))
              AND NOT pg_catalog.has_table_privilege(r.oid, c.oid,
                  'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER')
              AND NOT pg_catalog.has_any_column_privilege(r.oid, c.oid, 'INSERT,REFERENCES')
              AND (CASE WHEN ? <> 0
                   THEN pg_catalog.has_column_privilege(r.oid, c.oid, 'id', 'UPDATE')
                   ELSE NOT pg_catalog.has_any_column_privilege(r.oid, c.oid, 'UPDATE') END)
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
                              WHERE a.attrelid = c.oid AND a.attnum <> 0 AND NOT a.attisdropped
                                AND (a.attname <> 'id' OR ? = 0)
                                AND pg_catalog.has_column_privilege(r.oid, c.oid, a.attnum, 'UPDATE'))
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
                                  coalesce(c.relacl, pg_catalog.acldefault('r', c.relowner))
                              ) a
                              WHERE a.grantee = 0 OR (a.grantee = r.oid
                                    AND (a.is_grantable OR a.privilege_type <> 'SELECT')))
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute col
                              CROSS JOIN LATERAL pg_catalog.aclexplode(col.attacl) a
                              WHERE col.attrelid = c.oid
                                AND (a.grantee = 0 OR (a.grantee = r.oid AND
                                    (a.is_grantable OR (a.privilege_type <> 'SELECT' AND NOT
                                        (? <> 0 AND col.attname = 'id' AND a.privilege_type = 'UPDATE'))))))
           FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_catalog.pg_roles o ON o.rolname = ?
           JOIN pg_catalog.pg_roles r ON r.rolname = ?
           WHERE n.nspname = ? AND c.relname = ?"#,
        crate::db_params![
            epoch,
            id_update,
            id_update,
            id_update,
            config.owner.as_str(),
            config.runtime.as_str(),
            schema,
            table
        ],
        DarkMigrationOwnerError::Roles,
    )
    .await
}

async fn check_guarded_relation_acls(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
) -> Result<(), DarkMigrationOwnerError> {
    for table in EPOCH_GUARDED_TABLES {
        let (select, insert, update) = match table {
            "ingress_messages" => (true, true, true),
            "ingress_effect_intents" | "ingress_effect_receipts" | "ingress_deliveries" => {
                (true, true, false)
            }
            "ingress_effect_descendants" => (true, true, true),
            "ingress_sm_refs" | "ingress_archive_dispatch" | "ingress_owner_rejections" => {
                (true, false, false)
            }
            _ => (false, false, false),
        };
        // V1011 grants pg_monitor SELECT on the first six guarded tables;
        // later ingress migrations extend that same read profile to the rest.
        let monitor_select = true;
        require(
            tx,
            r#"SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND c.relowner = owner.oid
                  AND (pg_catalog.has_table_privilege(runtime.oid, c.oid, 'SELECT'))::int = ?
                  AND (pg_catalog.has_table_privilege(runtime.oid, c.oid, 'INSERT'))::int = ?
                  AND (pg_catalog.has_table_privilege(runtime.oid, c.oid, 'UPDATE'))::int = ?
                  AND NOT pg_catalog.has_table_privilege(runtime.oid, c.oid, 'DELETE')
                  AND NOT pg_catalog.has_table_privilege(runtime.oid, c.oid, 'TRUNCATE')
                  AND NOT pg_catalog.has_table_privilege(runtime.oid, c.oid, 'REFERENCES')
                  AND NOT pg_catalog.has_table_privilege(runtime.oid, c.oid, 'TRIGGER')
                  AND (pg_catalog.has_any_column_privilege(runtime.oid, c.oid, 'SELECT'))::int = ?
                  AND (pg_catalog.has_any_column_privilege(runtime.oid, c.oid, 'INSERT'))::int = ?
                  AND (pg_catalog.has_any_column_privilege(runtime.oid, c.oid, 'UPDATE'))::int = ?
                  AND NOT pg_catalog.has_any_column_privilege(runtime.oid, c.oid, 'REFERENCES')
                  AND CASE WHEN pg_catalog.current_setting('server_version_num')::int >= 170000
                       THEN NOT pg_catalog.has_table_privilege(runtime.oid, c.oid, 'MAINTAIN')
                       ELSE true END
                  AND (pg_catalog.has_table_privilege(monitor.oid, c.oid, 'SELECT'))::int = ?
                  AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
                                      coalesce(c.relacl, pg_catalog.acldefault('r', c.relowner))
                                  ) acl
                                  WHERE acl.grantee = 0
                                     OR acl.grantee NOT IN (owner.oid, runtime.oid, monitor.oid)
                                     OR (acl.grantee <> owner.oid AND acl.is_grantable)
                                     OR (acl.grantee = runtime.oid AND (
                                          (acl.privilege_type = 'SELECT' AND ? = 0) OR
                                          (acl.privilege_type = 'INSERT' AND ? = 0) OR
                                          (acl.privilege_type = 'UPDATE' AND ? = 0) OR
                                          acl.privilege_type IN ('DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER', 'MAINTAIN')))
                                     OR (acl.grantee = monitor.oid AND
                                         (? = 0 OR acl.privilege_type <> 'SELECT')))
                  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute column_
                                  CROSS JOIN LATERAL pg_catalog.aclexplode(column_.attacl) acl
                                  WHERE column_.attrelid = c.oid AND acl.grantee <> owner.oid)
               FROM pg_catalog.pg_class c
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
               JOIN pg_catalog.pg_roles owner ON owner.rolname = ?
               JOIN pg_catalog.pg_roles runtime ON runtime.rolname = ?
               JOIN pg_catalog.pg_roles monitor ON monitor.rolname = 'pg_monitor'
               WHERE n.nspname = ? AND c.relname = ?"#,
            crate::db_params![
                select,
                insert,
                update,
                select,
                insert,
                update,
                monitor_select,
                select,
                insert,
                update,
                monitor_select,
                config.owner.as_str(),
                config.runtime.as_str(),
                schema,
                table
            ],
            DarkMigrationOwnerError::EpochCatalogDrift,
        )
        .await?;
    }
    Ok(())
}
