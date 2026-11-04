//! Dark owner-only grant for the V1028 owner-rejection relation.

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection};
use thiserror::Error;

use super::epoch_acl::{
    correlate_runtime, function_source, runtime_pid, source_body, RELATION_GUARD_CANDIDATE,
};
use super::{
    check_migration_login_and_assume_owner, check_migrator_options, DarkMigrationOwnerConfig,
    DarkMigrationOwnerError, COMMIT_DEADLINE, CONNECT_DEADLINE, OPERATION_DEADLINE,
};
use crate::config::LineageConfig;
use crate::db::lineage::{self, LINEAGE_ADVISORY_LOCK_KEY};
use crate::db::migrations::runner::MIGRATION_LEDGER_ADVISORY_LOCK_KEY;
use crate::db::migrations::{migration_checksum, waddle};
use crate::db::physical_attestation::{quote_identifier, validate_identifier, PgRoleName};
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

/// Result of a fresh owner-side observation, not an admission capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerRejectionGrantState {
    Missing,
    Granted,
}

#[derive(Debug, Error)]
pub enum OwnerRejectionGrantError {
    #[error(transparent)]
    Owner(#[from] DarkMigrationOwnerError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("owner connection failed")]
    Postgres(#[from] sqlx::Error),
    #[error("owner-rejection grant requires a simple attested schema identifier")]
    SchemaIdentifier,
    #[error("owner-rejection relation, owner, or V1028 ledger entry does not match")]
    RelationMismatch,
    #[error("runtime role has unsafe attributes, membership, or effective privileges")]
    UnsafeRuntime,
    #[error("could not install bounded PostgreSQL timeouts")]
    Bounds,
    #[error("owner-rejection grant or commit may have succeeded; fence and inspect before retry")]
    CommitUnknown,
    #[error("owner-rejection grant committed, but fresh owner-side readback failed")]
    PostCommitUnverified,
    #[error("owner-rejection grant observation timed out")]
    ProbeTimeout,
}

/// Idempotently grant exactly table-level SELECT and INSERT to the separately
/// configured runtime login. No other table is changed and unexpected grants
/// are refused, not repaired. This is not called by normal startup.
///
/// The operator supplies the independent expected physical lineage and roles
/// in `config`, provides a direct authoritative primary, fences old attempts,
/// and keeps privileged DDL, role membership and ACL changes quiescent. The
/// NOLOGIN owner must own the schema and relation and read pg_control_system();
/// the independently configured migrator login must be allowed to assume it.
/// The checks do not audit arbitrary SECURITY DEFINER functions or all schema
/// objects. Failure/cancellation around COMMIT is indeterminate; a later read
/// cannot prove an older unfenced backend will not commit afterward.
/// A separately pinned runtime backend is checked in origin mode, but this does
/// not prove that old sessions are drained or that other sessions use origin.
pub async fn grant_dark_owner_rejection_runtime_access(
    migrator_options: &PgConnectOptions,
    runtime_options: &PgConnectOptions,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) -> Result<(), OwnerRejectionGrantError> {
    let result = tokio::time::timeout(
        OPERATION_DEADLINE,
        with_fresh_connection(migrator_options, runtime_options, config, migrator, true),
    )
    .await
    .map_err(|_| OwnerRejectionGrantError::CommitUnknown)??;
    if result != OwnerRejectionGrantState::Granted {
        return Err(OwnerRejectionGrantError::PostCommitUnverified);
    }
    match inspect_dark_owner_rejection_runtime_access(
        migrator_options,
        runtime_options,
        config,
        migrator,
    )
    .await
    {
        Ok(OwnerRejectionGrantState::Granted) => Ok(()),
        Ok(OwnerRejectionGrantState::Missing) | Err(_) => {
            Err(OwnerRejectionGrantError::PostCommitUnverified)
        }
    }
}

/// Read the scoped grant using a fresh, read-only owner transaction. A missing
/// grant is not proof that an earlier unfenced attempt cannot later commit.
pub async fn inspect_dark_owner_rejection_runtime_access(
    migrator_options: &PgConnectOptions,
    runtime_options: &PgConnectOptions,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
) -> Result<OwnerRejectionGrantState, OwnerRejectionGrantError> {
    tokio::time::timeout(
        OPERATION_DEADLINE,
        with_fresh_connection(migrator_options, runtime_options, config, migrator, false),
    )
    .await
    .map_err(|_| OwnerRejectionGrantError::ProbeTimeout)?
}

async fn with_fresh_connection(
    options: &PgConnectOptions,
    runtime_options: &PgConnectOptions,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
    grant: bool,
) -> Result<OwnerRejectionGrantState, OwnerRejectionGrantError> {
    check_migrator_options(options, config, migrator)?;
    if runtime_options.get_username() != config.runtime.as_str() {
        return Err(DarkMigrationOwnerError::Roles.into());
    }
    let nonce = format!("waddle-owner-rejection-{}", uuid::Uuid::new_v4().simple());
    let mut runtime = tokio::time::timeout(
        CONNECT_DEADLINE,
        PgConnection::connect_with(&runtime_options.clone().application_name(&nonce)),
    )
    .await
    .map_err(|_| DarkMigrationOwnerError::ConnectTimeout)??;
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
            drop(runtime_tx);
            let _ = tokio::time::timeout(CONNECT_DEADLINE, runtime.close_hard()).await;
            return Err(DarkMigrationOwnerError::PreflightTimeout.into());
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
            .connect_with(options.clone()),
    )
    .await
    .map_err(|_| DarkMigrationOwnerError::ConnectTimeout)??;
    let db = Database::from_postgres_pool("owner rejection grant", pool.clone());
    let result =
        run_on_connection(&db, &mut runtime_tx, config, migrator, grant, pid, &nonce).await;
    drop(db);
    let _ = tokio::time::timeout(CONNECT_DEADLINE, pool.close()).await;
    let _ = tokio::time::timeout(CONNECT_DEADLINE, runtime_tx.rollback()).await;
    drop(runtime);
    result
}

async fn run_on_connection(
    db: &Database,
    runtime_tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    config: &DarkMigrationOwnerConfig,
    migrator: &PgRoleName,
    grant: bool,
    pid: i32,
    nonce: &str,
) -> Result<OwnerRejectionGrantState, OwnerRejectionGrantError> {
    let identity = config
        .expected
        .postgres_identity
        .as_ref()
        .ok_or(DarkMigrationOwnerError::MissingPostgresIdentity)?;
    let schema = &identity.schema.name.0;
    validate_identifier(schema, "schema")
        .map_err(|_| OwnerRejectionGrantError::SchemaIdentifier)?;
    let quoted = quote_identifier(schema);
    let runtime = quote_identifier(config.runtime.as_str());
    let mut tx = db.begin().await?;
    let transaction_mode = if grant {
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE"
    } else {
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY"
    };
    tx.execute(transaction_mode, ()).await?;
    crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
        .await
        .map_err(|_| OwnerRejectionGrantError::Bounds)?;
    tx.execute(
        "SELECT pg_catalog.set_config('search_path', ?, true)",
        crate::db_params![format!("{quoted}, pg_temp")],
    )
    .await?;
    check_migration_login_and_assume_owner(
        &mut tx,
        config,
        migrator,
        schema,
        i64::from(identity.schema.oid),
    )
    .await?;
    for key in [
        LINEAGE_ADVISORY_LOCK_KEY,
        MIGRATION_LEDGER_ADVISORY_LOCK_KEY,
    ] {
        tx.execute(
            "SELECT pg_catalog.pg_advisory_xact_lock(?)",
            crate::db_params![key],
        )
        .await?;
    }
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
        return Err(DarkMigrationOwnerError::Identity.into());
    }
    correlate_runtime(&mut tx, config, pid, nonce).await?;
    verify_v1028_ledger(&mut tx, &quoted).await?;
    // Take the epoch relation before the protected mapping relation. This
    // avoids a target-first lock against an epoch-first migration.
    tx.execute(
        &format!("LOCK TABLE ONLY {quoted}.ingress_protocol_epoch IN ACCESS SHARE MODE"),
        (),
    )
    .await?;
    // Lock the exact qualified relation before inspecting or modifying its
    // ACL. Errors after the GRANT drop this uncommitted transaction.
    tx.execute(
        &format!("LOCK TABLE ONLY {quoted}.ingress_owner_rejections IN ACCESS SHARE MODE"),
        (),
    )
    .await?;
    verify_relation_manifest(&mut tx, config, schema).await?;
    let state = inspect_locked(&mut tx, config, schema).await?;
    let state = if grant && state == OwnerRejectionGrantState::Missing {
        tx.execute(
            &format!(
                "GRANT SELECT, INSERT ON TABLE {quoted}.ingress_owner_rejections TO {runtime}"
            ),
            (),
        )
        .await?;
        let after = inspect_locked(&mut tx, config, schema).await?;
        if after != OwnerRejectionGrantState::Granted {
            return Err(OwnerRejectionGrantError::UnsafeRuntime);
        }
        after
    } else {
        state
    };
    if runtime_pid(runtime_tx, config, nonce).await? != pid {
        return Err(DarkMigrationOwnerError::RuntimeSessionUnverified.into());
    }
    correlate_runtime(&mut tx, config, pid, nonce).await?;
    if grant {
        match tokio::time::timeout(COMMIT_DEADLINE, tx.commit()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => return Err(OwnerRejectionGrantError::CommitUnknown),
        }
    } else {
        tx.rollback().await?;
    }
    Ok(state)
}

async fn verify_v1028_ledger(
    tx: &mut Transaction<'_>,
    quoted: &str,
) -> Result<(), OwnerRejectionGrantError> {
    let migration = waddle::all()
        .into_iter()
        .find(|migration| migration.version == 1028)
        .ok_or(OwnerRejectionGrantError::RelationMismatch)?;
    let checksum = migration_checksum(&migration, DatabaseDriver::Postgres);
    let mut rows = tx
        .query(
            &format!("SELECT COUNT(*) FROM {quoted}._migrations WHERE version = ? AND description = ? AND checksum = ?"),
            crate::db_params![migration.version, migration.description.as_str(), checksum.as_str()],
        )
        .await?;
    let count: i64 = rows
        .next()
        .await?
        .ok_or(OwnerRejectionGrantError::RelationMismatch)?
        .get(0)?;
    if count != 1 {
        return Err(OwnerRejectionGrantError::RelationMismatch);
    }
    Ok(())
}

async fn verify_relation_manifest(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
) -> Result<(), OwnerRejectionGrantError> {
    let mut rows = tx.query(
        r#"SELECT NOT c.relhasrules AND NOT c.relforcerowsecurity
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
          AND (SELECT COUNT(*) = 5 FROM pg_catalog.pg_constraint x WHERE x.conrelid = c.oid)
          AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x
            JOIN pg_catalog.pg_index i ON i.indexrelid = x.conindid
            WHERE x.conrelid = c.oid AND x.contype = 'p' AND x.conkey = ARRAY[1]::smallint[]
              AND x.convalidated AND NOT x.condeferrable AND i.indisvalid AND i.indisready AND i.indisunique)
          AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x
            JOIN pg_catalog.pg_index i ON i.indexrelid = x.conindid
            WHERE x.conrelid = c.oid AND x.contype = 'u' AND x.conkey = ARRAY[2]::smallint[]
              AND x.convalidated AND NOT x.condeferrable AND i.indisvalid AND i.indisready AND i.indisunique)
          AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x
            WHERE x.conrelid = c.oid AND x.contype = 'c' AND x.convalidated AND NOT x.connoinherit
              AND pg_catalog.pg_get_expr(x.conbin, x.conrelid) = '(source_message_key <> child_message_key)')
          AND (SELECT COUNT(*) = 2 AND COUNT(DISTINCT x.conkey::text) = 2 FROM pg_catalog.pg_constraint x
            WHERE x.conrelid = c.oid AND x.contype = 'f' AND x.confrelid = message.oid
              AND x.confkey = ARRAY[1]::smallint[] AND x.confupdtype = 'a' AND x.confmatchtype = 's'
              AND x.convalidated AND NOT x.condeferrable AND NOT x.condeferred
              AND ((x.conkey = ARRAY[1]::smallint[] AND x.confdeltype = 'r')
                OR (x.conkey = ARRAY[2]::smallint[] AND x.confdeltype = 'c'))
              AND (SELECT COUNT(*) = 4 FROM pg_catalog.pg_trigger t
                   WHERE t.tgconstraint = x.oid AND t.tgisinternal AND t.tgenabled = 'O'))
          AND (SELECT COUNT(*) = 5 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid AND NOT t.tgisinternal)
        FROM pg_catalog.pg_namespace n
        JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = 'ingress_owner_rejections'
        JOIN pg_catalog.pg_class message ON message.relnamespace = n.oid AND message.relname = 'ingress_messages'
        WHERE n.nspname = ? AND message.relkind = 'r'"#,
        crate::db_params![schema],
    ).await?;
    let valid = rows
        .next()
        .await?
        .ok_or(OwnerRejectionGrantError::RelationMismatch)?
        .get::<bool>(0)?;
    drop(rows);
    if !valid {
        return Err(OwnerRejectionGrantError::RelationMismatch);
    }
    verify_relation_triggers(tx, config, schema).await
}

async fn verify_relation_triggers(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
) -> Result<(), OwnerRejectionGrantError> {
    let migration = waddle::V1028_INGRESS_SOURCE_BINDING_POSTGRES;
    for (name, trigger, trigger_type) in [
        (
            "waddle_ingress_owner_rejection_immutable",
            "ingress_owner_rejections_update_guard",
            19_i64,
        ),
        (
            "waddle_ingress_owner_rejection_insert_guard",
            "ingress_owner_rejections_insert_guard",
            7_i64,
        ),
        (
            "waddle_ingress_owner_rejection_delete_guard",
            "ingress_owner_rejections_delete_guard",
            11_i64,
        ),
    ] {
        let function = function_source(tx, config, schema, name).await?;
        if Some(function.source.as_str()) != source_body(migration, name) {
            return Err(OwnerRejectionGrantError::RelationMismatch);
        }
        verify_trigger(tx, schema, trigger, function.oid, trigger_type, "O").await?;
    }
    let guard = function_source(tx, config, schema, "waddle_ingress_epoch_guard").await?;
    let v1009 = waddle::V1009_INERT_INGRESS_EPOCH_GUARDS_POSTGRES;
    if Some(guard.source.as_str()) != source_body(v1009, "waddle_ingress_epoch_guard")
        && Some(guard.source.as_str())
            != source_body(RELATION_GUARD_CANDIDATE, "waddle_ingress_epoch_guard")
    {
        return Err(OwnerRejectionGrantError::RelationMismatch);
    }
    verify_trigger(
        tx,
        schema,
        "ingress_owner_rejections_epoch_guard_dml",
        guard.oid,
        30,
        "A",
    )
    .await?;
    let truncate = function_source(tx, config, schema, "waddle_ingress_truncate_guard").await?;
    if Some(truncate.source.as_str()) != source_body(v1009, "waddle_ingress_truncate_guard") {
        return Err(OwnerRejectionGrantError::RelationMismatch);
    }
    verify_trigger(
        tx,
        schema,
        "ingress_owner_rejections_epoch_guard_truncate",
        truncate.oid,
        34,
        "A",
    )
    .await?;
    Ok(())
}

async fn verify_trigger(
    tx: &mut Transaction<'_>,
    schema: &str,
    name: &str,
    function_oid: i64,
    trigger_type: i64,
    enabled: &str,
) -> Result<(), OwnerRejectionGrantError> {
    let mut rows = tx.query(
        r#"SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t
          JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid
          JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = ? AND c.relname = 'ingress_owner_rejections' AND t.tgname = ?
            AND t.tgfoid::bigint = ? AND t.tgtype::bigint = ? AND t.tgenabled::text = ?
            AND NOT t.tgisinternal AND t.tgparentid = 0 AND t.tgconstraint = 0
            AND NOT t.tgdeferrable AND NOT t.tginitdeferred AND t.tgnargs = 0
            AND t.tgattr::text = '' AND t.tgqual IS NULL AND t.tgoldtable IS NULL AND t.tgnewtable IS NULL)"#,
        crate::db_params![schema, name, function_oid, trigger_type, enabled],
    ).await?;
    if !rows
        .next()
        .await?
        .ok_or(OwnerRejectionGrantError::RelationMismatch)?
        .get::<bool>(0)?
    {
        return Err(OwnerRejectionGrantError::RelationMismatch);
    }
    Ok(())
}

async fn inspect_locked(
    tx: &mut Transaction<'_>,
    config: &DarkMigrationOwnerConfig,
    schema: &str,
) -> Result<OwnerRejectionGrantState, OwnerRejectionGrantError> {
    let mut rows = tx.query(
        r#"SELECT
          c.relkind = 'r' AND c.relpersistence = 'p' AND NOT c.relispartition
            AND NOT c.relrowsecurity AND c.relowner = owner_role.oid
            AND (SELECT COUNT(*) = 2 AND BOOL_AND(
              ((a.attnum = 1 AND a.attname = 'source_message_key')
                OR (a.attnum = 2 AND a.attname = 'child_message_key'))
              AND a.atttypid = 'pg_catalog.uuid'::pg_catalog.regtype AND a.attnotnull
              AND a.attgenerated = '' AND a.attidentity = '')
              FROM pg_catalog.pg_attribute a
              WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped),
          r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreatedb AND NOT r.rolcreaterole
            AND NOT r.rolreplication AND NOT r.rolbypassrls
            AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member = r.oid)
            AND pg_catalog.has_schema_privilege(r.oid, n.oid, 'USAGE')
            AND NOT pg_catalog.has_schema_privilege(r.oid, n.oid, 'CREATE')
            AND NOT pg_catalog.has_database_privilege(r.oid, pg_catalog.current_database(), 'TEMPORARY')
            AND NOT pg_catalog.has_parameter_privilege(r.oid, 'session_replication_role', 'SET')
            AND NOT pg_catalog.has_table_privilege(r.oid, c.oid, 'UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER')
            AND NOT pg_catalog.has_any_column_privilege(r.oid, c.oid, 'UPDATE,REFERENCES')
            AND NOT pg_catalog.has_table_privilege(r.oid, c.oid, 'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION')
            AND NOT pg_catalog.has_any_column_privilege(r.oid, c.oid, 'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION')
            AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
              COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) a
              WHERE a.grantee <> c.relowner AND NOT (
                a.grantor = c.relowner AND NOT a.is_grantable AND (
                  (a.grantee = r.oid AND a.privilege_type IN ('SELECT', 'INSERT')) OR
                  (a.grantee = (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'pg_monitor')
                    AND a.privilege_type = 'SELECT'))))
            AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute col
              CROSS JOIN LATERAL pg_catalog.aclexplode(col.attacl) a
              WHERE col.attrelid = c.oid AND a.grantee <> c.relowner),
          pg_catalog.has_table_privilege(r.oid, c.oid, 'SELECT')
            AND pg_catalog.has_table_privilege(r.oid, c.oid, 'INSERT')
            AND (SELECT COUNT(DISTINCT a.privilege_type) = 2
                 FROM pg_catalog.aclexplode(COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) a
                 WHERE a.grantee = r.oid AND a.grantor = c.relowner
                   AND a.privilege_type IN ('SELECT', 'INSERT') AND NOT a.is_grantable)
        FROM pg_catalog.pg_namespace n
        JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = 'ingress_owner_rejections'
        JOIN pg_catalog.pg_roles owner_role ON owner_role.rolname = ?
        JOIN pg_catalog.pg_roles r ON r.rolname = ?
        WHERE n.nspname = ?"#,
        crate::db_params![config.owner.as_str(), config.runtime.as_str(), schema],
    ).await?;
    let row = rows
        .next()
        .await?
        .ok_or(OwnerRejectionGrantError::RelationMismatch)?;
    if !row.get::<bool>(0)? {
        return Err(OwnerRejectionGrantError::RelationMismatch);
    }
    if !row.get::<bool>(1)? {
        return Err(OwnerRejectionGrantError::UnsafeRuntime);
    }
    Ok(if row.get::<bool>(2)? {
        OwnerRejectionGrantState::Granted
    } else {
        OwnerRejectionGrantState::Missing
    })
}
