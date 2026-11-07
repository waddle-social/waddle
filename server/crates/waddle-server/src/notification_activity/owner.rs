//! PostgreSQL-only owner preparation for the notification activity projection.

use std::time::Duration;

use crate::db::lineage::{
    PgDatabaseIdentity, PgDatabaseName, PgIdentity, PgSchemaIdentity, PgSchemaName,
    PgSystemIdentifier,
};
use crate::db::{CatalogFenceLock, Database, DatabaseError, Transaction};
use sqlx::postgres::PgConnection;
use sqlx::Row;

use super::schema::{self, ACTIVITY_OWNER_STATE_TABLE, ACTIVITY_SCHEMA_GATE};
use super::NotificationActivityError;

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(35);
const COMMIT_TIMEOUT: Duration = Duration::from_secs(5);

/// All PostgreSQL probes, marker changes and DDL use this single transaction.
/// The Activity gate is tried only after catalog-X, and a failed try releases
/// catalog-X before a fresh transaction is attempted by the fence helper.
pub(super) async fn initialize_postgres(
    db: &Database,
    strict_owner: bool,
) -> Result<(), NotificationActivityError> {
    let mut tx = db
        .begin_catalog_fenced_with_idle_bound(&[CatalogFenceLock::ExclusivePair(
            ACTIVITY_SCHEMA_GATE.0,
            ACTIVITY_SCHEMA_GATE.1,
        )])
        .await?;
    tokio::time::timeout(
        INITIALIZE_TIMEOUT,
        prepare_in_transaction(&mut tx, strict_owner),
    )
    .await
    .map_err(|_| NotificationActivityError::PostgresInitializationTimedOut)??;

    // A lost COMMIT response cannot be reconciled by a marker left by a
    // prior attempt. The lease is discarded if COMMIT is not acknowledged.
    if !matches!(
        tokio::time::timeout(COMMIT_TIMEOUT, tx.commit()).await,
        Ok(Ok(()))
    ) {
        return Err(NotificationActivityError::CommitIndeterminate);
    }
    Ok(())
}

async fn prepare_in_transaction(
    tx: &mut Transaction<'_>,
    strict_owner: bool,
) -> Result<(), NotificationActivityError> {
    let connection = tx.postgres_connection().ok_or_else(|| {
        DatabaseError::QueryFailed(
            "notification activity initialization requires PostgreSQL".to_owned(),
        )
    })?;
    // The first read on the fenced backend calls only explicitly qualified
    // builtins. Preserve the caller's effective target before making the
    // remainder of the transaction use a builtins-first path; otherwise a
    // target schema could supply an operator used by a later catalog query.
    let row = sqlx::query(
        "SELECT pg_catalog.current_schema()::pg_catalog.text, \
                pg_catalog.current_schemas(false)::pg_catalog.text[]",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(DatabaseError::from)?;
    let active_schema: String = row.try_get(0).map_err(DatabaseError::from)?;
    let active_schemas: Vec<String> = row.try_get(1).map_err(DatabaseError::from)?;
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    // Preserve tighter role/session limits while bounding server-side waits.
    for (setting, cap) in [("lock_timeout", "5s"), ("statement_timeout", "30s")] {
        sqlx::query(
            "SELECT pg_catalog.set_config($1, \
             CASE WHEN pg_catalog.current_setting($1)::pg_catalog.interval = '0'::pg_catalog.interval \
               OR pg_catalog.current_setting($1)::pg_catalog.interval > $2::pg_catalog.interval \
             THEN $2 ELSE pg_catalog.current_setting($1) END, true)",
        )
        .bind(setting)
        .bind(cap)
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    }
    let identity = if strict_owner {
        Some(read_live_identity(connection, &active_schema).await?)
    } else {
        None
    };
    let schema = if let Some(identity) = identity.as_ref() {
        verify_owner_boundary(connection, identity, &active_schema, &active_schemas).await?;
        prepare_incomplete_marker(connection, identity).await?;
        identity.schema.name.0.clone()
    } else {
        let schema = pin_legacy_schema(connection, &active_schema).await?;
        invalidate_existing_marker(connection, &schema).await?;
        schema
    };
    schema::initialize_postgres(connection, &schema).await?;
    if let Some(identity) = identity.as_ref() {
        verify_owner_boundary(connection, identity, &active_schema, &active_schemas).await?;
        complete_preparation(connection, identity).await?;
    }
    Ok(())
}

/// Read strict owner identity only after catalog-X and the Activity gate are
/// held on this backend. Every builtin and cast is explicitly pg_catalog
/// qualified because the search path has not yet been trusted.
async fn read_live_identity(
    connection: &mut PgConnection,
    active_schema: &str,
) -> Result<PgIdentity, DatabaseError> {
    let row = sqlx::query(
        "SELECT (SELECT system_identifier::pg_catalog.text \
                 FROM pg_catalog.pg_control_system()), \
                database.oid::pg_catalog.int8, database.datname::pg_catalog.text, \
                namespace.oid::pg_catalog.int8, namespace.nspname::pg_catalog.text \
         FROM pg_catalog.pg_database database \
         JOIN pg_catalog.pg_namespace namespace ON namespace.nspname = $1 \
         WHERE database.datname = pg_catalog.current_database()",
    )
    .bind(active_schema)
    .fetch_optional(&mut *connection)
    .await
    .map_err(DatabaseError::from)?
    .ok_or_else(|| owner_boundary_error("notification activity active schema is missing"))?;
    let system_identifier: String = row.try_get(0).map_err(DatabaseError::from)?;
    let database_oid: i64 = row.try_get(1).map_err(DatabaseError::from)?;
    let database_name: String = row.try_get(2).map_err(DatabaseError::from)?;
    let schema_oid: i64 = row.try_get(3).map_err(DatabaseError::from)?;
    let schema_name: String = row.try_get(4).map_err(DatabaseError::from)?;
    Ok(PgIdentity {
        system_identifier: system_identifier.parse::<PgSystemIdentifier>()?,
        database: PgDatabaseIdentity {
            oid: u32::try_from(database_oid).map_err(|_| {
                owner_boundary_error("notification activity database OID is invalid")
            })?,
            name: PgDatabaseName(database_name),
        },
        schema: PgSchemaIdentity {
            oid: u32::try_from(schema_oid)
                .map_err(|_| owner_boundary_error("notification activity schema OID is invalid"))?,
            name: PgSchemaName(schema_name),
        },
    })
}

/// Pin legacy DDL to the captured persistent schema and reject temporary
/// relations that would shadow later unqualified runtime queries. SQL here
/// runs with pg_catalog first; the caller's path is not restored for DDL.
async fn pin_legacy_schema(
    connection: &mut PgConnection,
    active_schema: &str,
) -> Result<String, DatabaseError> {
    if active_schema.starts_with("pg_") || active_schema == "information_schema" {
        return Err(owner_boundary_error(
            "notification activity raw initializer requires a persistent application schema",
        ));
    }
    let row = sqlx::query(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n \
           WHERE n.oid = pg_catalog.pg_my_temp_schema() AND n.nspname = $1) \
         OR EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
           WHERE c.relnamespace = pg_catalog.pg_my_temp_schema() \
             AND c.relname IN ('notification_activity', 'notification_activity_owner_state'))",
    )
    .bind(active_schema)
    .fetch_one(&mut *connection)
    .await
    .map_err(DatabaseError::from)?;
    if row.try_get::<bool, _>(0).map_err(DatabaseError::from)? {
        return Err(owner_boundary_error(
            "notification activity raw initializer refused a temporary schema shadow",
        ));
    }
    Ok(active_schema.to_owned())
}

async fn complete_preparation(
    connection: &mut PgConnection,
    identity: &PgIdentity,
) -> Result<(), DatabaseError> {
    verify_marker_relation(connection, &identity.schema.name.0).await?;
    let marker = qualified_marker(&identity.schema.name.0);
    let row = sqlx::query(&format!(
        "UPDATE {marker} SET prepared_revision = '1' WHERE singleton = '1' \
         AND prepared_revision = '0' AND system_identifier = $1 \
         AND database_oid = $2 AND database_name = $3 \
         AND schema_oid = $4 AND schema_name = $5"
    ))
    .bind(identity.system_identifier.to_string())
    .bind(i64::from(identity.database.oid))
    .bind(&identity.database.name.0)
    .bind(i64::from(identity.schema.oid))
    .bind(&identity.schema.name.0)
    .execute(&mut *connection)
    .await
    .map_err(DatabaseError::from)?;
    if row.rows_affected() != 1 {
        return Err(owner_boundary_error(
            "notification activity owner marker changed during preparation",
        ));
    }
    Ok(())
}

async fn verify_owner_boundary(
    connection: &mut PgConnection,
    identity: &PgIdentity,
    active_schema: &str,
    active_schemas: &[String],
) -> Result<(), DatabaseError> {
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "notification activity schema")
        .map_err(DatabaseError::from)?;
    if active_schema != schema
        || active_schemas.first().is_none_or(|first| first != schema)
        || active_schemas.len() > 2
        || (active_schemas.len() == 2 && !active_schemas[1].starts_with("pg_temp_"))
    {
        return Err(owner_boundary_error(
            "notification activity owner search path does not identify its schema",
        ));
    }
    let row = sqlx::query(
        "SELECT (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
         database.oid::bigint, database.datname::text, namespace.oid::bigint, \
         namespace.nspname::text, \
         NOT pg_catalog.pg_is_in_recovery() \
           AND owner.rolname = current_user \
           AND NOT owner.rolcanlogin AND NOT owner.rolsuper \
           AND NOT owner.rolcreatedb AND NOT owner.rolcreaterole \
           AND NOT owner.rolreplication AND NOT owner.rolbypassrls \
           AND owner.oid <> database.datdba \
           AND NOT pg_catalog.has_database_privilege(owner.oid, database.oid, 'CREATE,TEMP') \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                           WHERE membership.member = owner.oid) \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
             COALESCE(namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
             WHERE acl.grantee = 0 OR (acl.grantee <> namespace.nspowner \
               AND (acl.privilege_type <> 'USAGE' OR acl.is_grantable))) \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class temp_relation \
                           WHERE temp_relation.relnamespace = pg_catalog.pg_my_temp_schema()) \
         FROM pg_catalog.pg_namespace namespace \
         JOIN pg_catalog.pg_roles owner ON owner.oid = namespace.nspowner \
         JOIN pg_catalog.pg_database database \
           ON database.datname = pg_catalog.current_database() \
         WHERE namespace.nspname = $1",
    )
    .bind(schema)
    .fetch_optional(&mut *connection)
    .await
    .map_err(DatabaseError::from)?;

    let valid = if let Some(row) = row {
        let system_identifier: String = row.try_get(0).map_err(DatabaseError::from)?;
        system_identifier == identity.system_identifier.to_string()
            && row.try_get::<i64, _>(1).map_err(DatabaseError::from)?
                == i64::from(identity.database.oid)
            && row.try_get::<String, _>(2).map_err(DatabaseError::from)? == identity.database.name.0
            && row.try_get::<i64, _>(3).map_err(DatabaseError::from)?
                == i64::from(identity.schema.oid)
            && row.try_get::<String, _>(4).map_err(DatabaseError::from)? == *schema
            && row.try_get::<bool, _>(5).map_err(DatabaseError::from)?
    } else {
        false
    };
    if !valid {
        return Err(owner_boundary_error(
            "notification activity owner handle does not match its physical schema identity",
        ));
    }
    Ok(())
}

async fn prepare_incomplete_marker(
    connection: &mut PgConnection,
    identity: &PgIdentity,
) -> Result<(), DatabaseError> {
    let schema = &identity.schema.name.0;
    let exists = marker_exists(connection, schema).await?;

    let marker = qualified_marker(schema);
    if exists {
        lock_marker(connection, schema).await?;
        verify_marker_relation(connection, schema).await?;
        verify_marker_identity(connection, schema).await?;
        let result = sqlx::query(&format!(
            "UPDATE {marker} SET prepared_revision = '0' WHERE singleton = '1'"
        ))
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
        if result.rows_affected() != 1 {
            return Err(owner_boundary_error(
                "notification activity owner marker could not be invalidated",
            ));
        }
    } else {
        sqlx::query(&format!(
            "CREATE TABLE {marker} (\
               singleton TEXT NOT NULL CONSTRAINT notification_activity_owner_state_singleton_check \
                 CHECK (singleton IN ('1')), \
               prepared_revision TEXT NOT NULL CONSTRAINT notification_activity_owner_state_revision_check \
                 CHECK (prepared_revision IN ('0', '1')), \
               system_identifier TEXT NOT NULL, \
               database_oid BIGINT NOT NULL, \
               database_name TEXT NOT NULL, \
               schema_oid BIGINT NOT NULL, \
               schema_name TEXT NOT NULL, \
               PRIMARY KEY (singleton))"
        ))
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
        sqlx::query(&format!(
            "INSERT INTO {marker} (singleton, prepared_revision, system_identifier, \
             database_oid, database_name, schema_oid, schema_name) \
             VALUES ('1', '0', $1, $2, $3, $4, $5)"
        ))
        .bind(identity.system_identifier.to_string())
        .bind(i64::from(identity.database.oid))
        .bind(&identity.database.name.0)
        .bind(i64::from(identity.schema.oid))
        .bind(schema)
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    }
    verify_marker_relation(connection, schema).await?;
    verify_marker_identity(connection, schema).await
}

async fn marker_exists(connection: &mut PgConnection, schema: &str) -> Result<bool, DatabaseError> {
    sqlx::query(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class relation \
         JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
         WHERE namespace.nspname = $1 AND relation.relname = $2)",
    )
    .bind(schema)
    .bind(ACTIVITY_OWNER_STATE_TABLE)
    .fetch_one(&mut *connection)
    .await
    .map_err(DatabaseError::from)?
    .try_get::<bool, _>(0)
    .map_err(DatabaseError::from)
}

async fn lock_marker(connection: &mut PgConnection, schema: &str) -> Result<(), DatabaseError> {
    sqlx::query(&format!(
        "LOCK TABLE {} IN SHARE ROW EXCLUSIVE MODE",
        qualified_marker(schema)
    ))
    .execute(connection)
    .await
    .map_err(DatabaseError::from)?;
    Ok(())
}

async fn invalidate_existing_marker(
    connection: &mut PgConnection,
    schema: &str,
) -> Result<(), DatabaseError> {
    if !marker_exists(connection, schema).await? {
        return Ok(());
    }
    lock_marker(connection, schema).await?;
    verify_marker_relation(connection, schema).await?;
    verify_marker_identity(connection, schema).await?;
    let rows = sqlx::query(&format!(
        "UPDATE {} SET prepared_revision = '0' WHERE singleton = '1'",
        qualified_marker(schema)
    ))
    .execute(&mut *connection)
    .await
    .map_err(DatabaseError::from)?;
    if rows.rows_affected() != 1 {
        return Err(owner_boundary_error(
            "notification activity owner marker could not be invalidated",
        ));
    }
    Ok(())
}

async fn verify_marker_identity(
    connection: &mut PgConnection,
    schema: &str,
) -> Result<(), DatabaseError> {
    let valid: bool = sqlx::query_scalar(&format!(
        "SELECT pg_catalog.count(*) = 1 AND COALESCE(pg_catalog.bool_and(\
           singleton = '1' AND prepared_revision IN ('0', '1') \
           AND system_identifier = (SELECT system_identifier::text FROM pg_catalog.pg_control_system()) \
           AND database_oid = (SELECT oid::bigint FROM pg_catalog.pg_database \
                               WHERE datname = pg_catalog.current_database()) \
           AND database_name = pg_catalog.current_database()::text \
           AND schema_oid = (SELECT oid::bigint FROM pg_catalog.pg_namespace WHERE nspname = $1) \
           AND schema_name = $1), false) FROM {}",
        qualified_marker(schema)
    ))
    .bind(schema)
    .fetch_one(&mut *connection)
    .await
    .map_err(DatabaseError::from)?;
    if !valid {
        return Err(owner_boundary_error(
            "notification activity owner marker has an unexpected physical identity or revision",
        ));
    }
    Ok(())
}

async fn verify_marker_relation(
    connection: &mut PgConnection,
    schema: &str,
) -> Result<(), DatabaseError> {
    let row = sqlx::query(
        "SELECT relation.relkind = 'r' AND relation.relpersistence = 'p' \
           AND relation.relowner = namespace.nspowner \
           AND NOT relation.relrowsecurity AND NOT relation.relforcerowsecurity \
           AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attribute attribute \
                WHERE attribute.attrelid = relation.oid AND attribute.attnum > 0 \
                  AND NOT attribute.attisdropped) = 7 \
           AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attribute attribute \
                JOIN pg_catalog.pg_type typ ON typ.oid = attribute.atttypid \
                WHERE attribute.attrelid = relation.oid AND attribute.attnum > 0 \
                  AND NOT attribute.attisdropped AND attribute.attnotnull \
                  AND attribute.attidentity = '' AND attribute.attgenerated = '' \
                  AND attribute.attcollation = typ.typcollation \
                  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attrdef d \
                    WHERE d.adrelid = relation.oid AND d.adnum = attribute.attnum) \
                  AND ((attribute.attname IN ('singleton', 'prepared_revision', \
                         'system_identifier', 'database_name', 'schema_name') \
                        AND attribute.atttypid = 'pg_catalog.text'::pg_catalog.regtype) \
                    OR (attribute.attname IN ('database_oid', 'schema_oid') \
                        AND attribute.atttypid = 'pg_catalog.int8'::pg_catalog.regtype))) = 7 \
           AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint constraint_ \
                WHERE constraint_.conrelid = relation.oid) = 3 \
           AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_index i ON i.indexrelid = c.conindid \
             WHERE c.conrelid = relation.oid AND c.contype = 'p' \
               AND NOT c.condeferrable AND c.convalidated \
               AND c.conkey = ARRAY[(SELECT a.attnum FROM pg_catalog.pg_attribute a \
                 WHERE a.attrelid = relation.oid AND a.attname = 'singleton')]::int2[] \
               AND i.indisvalid AND i.indisready AND i.indislive AND i.indimmediate) \
           AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint c \
             WHERE c.conrelid = relation.oid AND c.contype = 'c' AND c.convalidated \
               AND NOT c.condeferrable AND NOT c.condeferred AND ( \
                (c.conname = 'notification_activity_owner_state_singleton_check' \
                 AND pg_catalog.regexp_replace(pg_catalog.replace(pg_catalog.replace( \
                   pg_catalog.lower(pg_catalog.pg_get_constraintdef(c.oid)), \
                   '::text[]', ''), '::text', ''), '[[:space:]()]', '', 'g') \
                   = 'checksingleton=''1''') \
                OR (c.conname = 'notification_activity_owner_state_revision_check' \
                 AND pg_catalog.regexp_replace(pg_catalog.replace(pg_catalog.replace( \
                   pg_catalog.lower(pg_catalog.pg_get_constraintdef(c.oid)), \
                   '::text[]', ''), '::text', ''), '[[:space:]()]', '', 'g') \
                   = 'checkprepared_revision=anyarray[''0'',''1'']'))) = 2 \
           AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_index index_ \
                WHERE index_.indrelid = relation.oid) = 1 \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger trigger_ \
                           WHERE trigger_.tgrelid = relation.oid) \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rewrite_ \
                           WHERE rewrite_.ev_class = relation.oid) \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inheritance \
                           WHERE inheritance.inhrelid = relation.oid \
                              OR inheritance.inhparent = relation.oid) \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint inbound \
                           WHERE inbound.contype = 'f' AND inbound.confrelid = relation.oid) \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy policy_ \
                           WHERE policy_.polrelid = relation.oid) \
         FROM pg_catalog.pg_class relation \
         JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
         WHERE namespace.nspname = $1 AND relation.relname = $2",
    )
    .bind(schema)
    .bind(ACTIVITY_OWNER_STATE_TABLE)
    .fetch_optional(&mut *connection)
    .await
    .map_err(DatabaseError::from)?;
    let safe = match row {
        Some(row) => row.try_get::<bool, _>(0).map_err(DatabaseError::from)?,
        None => false,
    };
    if !safe {
        return Err(owner_boundary_error(
            "notification activity owner marker has an unexpected PostgreSQL shape",
        ));
    }
    Ok(())
}

fn qualified_marker(schema: &str) -> String {
    format!(
        "{}.\"{}\"",
        crate::db::physical_attestation::quote_identifier(schema),
        ACTIVITY_OWNER_STATE_TABLE
    )
}

fn owner_boundary_error(message: &str) -> DatabaseError {
    DatabaseError::QueryFailed(message.to_owned())
}
