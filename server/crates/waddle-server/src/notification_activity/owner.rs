//! PostgreSQL-only owner preparation for the notification activity projection.

use crate::db::lineage::{self, PgIdentity};
use crate::db::{Database, DatabaseError};
use sqlx::postgres::PgConnection;
use sqlx::{Connection, Row};

use super::schema::{ACTIVITY_OWNER_STATE_TABLE, ACTIVITY_SCHEMA_GATE};

/// A pinned owner session holds the exclusive activity-schema gate while the
/// existing initializer performs its migrations through the normal pool.
/// The incomplete marker is committed before that work begins, so cancellation
/// or a partial migration cannot leave a completed marker behind.
pub(super) struct OwnerPreparationSession {
    connection: Option<PgConnection>,
    identity: PgIdentity,
}

impl OwnerPreparationSession {
    pub(super) async fn begin(db: &Database) -> Result<Self, DatabaseError> {
        let identity = lineage::live_postgres_identity(db).await?;
        let mut connection = db.detached_postgres_startup_connection().await?;
        let result = begin_preparation(&mut connection, &identity).await;
        if let Err(error) = result {
            let _ = connection.close().await;
            return Err(error);
        }
        Ok(Self {
            connection: Some(connection),
            identity,
        })
    }

    pub(super) async fn complete(mut self) -> Result<(), DatabaseError> {
        let connection = self.connection.as_mut().ok_or_else(|| {
            DatabaseError::QueryFailed(
                "notification activity owner session is already closed".to_owned(),
            )
        })?;
        let result = complete_preparation(connection, &self.identity).await;
        let close_result = self.close().await;
        result?;
        close_result
    }

    pub(super) async fn abort(mut self) {
        if let Some(connection) = self.connection.take() {
            if let Err(error) = connection.close().await {
                tracing::warn!(%error, "failed to close notification activity owner session");
            }
        }
    }

    async fn close(&mut self) -> Result<(), DatabaseError> {
        match self.connection.take() {
            Some(connection) => connection.close().await.map_err(DatabaseError::from),
            None => Ok(()),
        }
    }
}

async fn begin_preparation(
    connection: &mut PgConnection,
    identity: &PgIdentity,
) -> Result<(), DatabaseError> {
    sqlx::query("SET lock_timeout = '30s'")
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    sqlx::query("SELECT pg_catalog.pg_advisory_lock($1::integer, $2::integer)")
        .bind(ACTIVITY_SCHEMA_GATE.0)
        .bind(ACTIVITY_SCHEMA_GATE.1)
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;

    sqlx::query("BEGIN")
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE")
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    sqlx::query("SET LOCAL statement_timeout = '300000ms'")
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    verify_owner_boundary(connection, identity).await?;
    prepare_incomplete_marker(connection, identity).await?;
    sqlx::query("COMMIT")
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    Ok(())
}

async fn complete_preparation(
    connection: &mut PgConnection,
    identity: &PgIdentity,
) -> Result<(), DatabaseError> {
    sqlx::query("BEGIN")
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE")
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    sqlx::query("SET LOCAL statement_timeout = '300000ms'")
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    verify_owner_boundary(connection, identity).await?;
    verify_marker_relation(connection, identity).await?;
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
    sqlx::query("COMMIT")
        .execute(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
    Ok(())
}

async fn verify_owner_boundary(
    connection: &mut PgConnection,
    identity: &PgIdentity,
) -> Result<(), DatabaseError> {
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "notification activity schema")
        .map_err(DatabaseError::from)?;
    let row = sqlx::query(
        "SELECT (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
         database.oid::bigint, database.datname::text, namespace.oid::bigint, \
         namespace.nspname::text, \
         namespace.nspname = pg_catalog.current_schema() \
           AND COALESCE((pg_catalog.current_schemas(false))[1]::text, '') = namespace.nspname \
           AND pg_catalog.cardinality(pg_catalog.current_schemas(false)) <= 2 \
           AND (pg_catalog.cardinality(pg_catalog.current_schemas(false)) = 1 \
             OR pg_catalog.left((pg_catalog.current_schemas(false))[2]::text, 8) = 'pg_temp_') \
           AND NOT pg_catalog.pg_is_in_recovery() \
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
    let exists = sqlx::query(
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
    .map_err(DatabaseError::from)?;

    let marker = qualified_marker(schema);
    if exists {
        verify_marker_relation(connection, identity).await?;
        let row = sqlx::query(&format!(
            "SELECT prepared_revision, system_identifier, database_oid, database_name, \
             schema_oid, schema_name FROM {marker} WHERE singleton = '1'"
        ))
        .fetch_optional(&mut *connection)
        .await
        .map_err(DatabaseError::from)?;
        let Some(row) = row else {
            return Err(owner_boundary_error(
                "notification activity owner marker has no singleton row",
            ));
        };
        let revision: String = row.try_get(0).map_err(DatabaseError::from)?;
        let marker_identity_matches = row.try_get::<String, _>(1).map_err(DatabaseError::from)?
            == identity.system_identifier.to_string()
            && row.try_get::<i64, _>(2).map_err(DatabaseError::from)?
                == i64::from(identity.database.oid)
            && row.try_get::<String, _>(3).map_err(DatabaseError::from)?
                == identity.database.name.0
            && row.try_get::<i64, _>(4).map_err(DatabaseError::from)?
                == i64::from(identity.schema.oid)
            && row.try_get::<String, _>(5).map_err(DatabaseError::from)? == *schema;
        if !marker_identity_matches || !matches!(revision.as_str(), "0" | "1") {
            return Err(owner_boundary_error(
                "notification activity owner marker has an unexpected physical identity or revision",
            ));
        }
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
    verify_marker_relation(connection, identity).await
}

async fn verify_marker_relation(
    connection: &mut PgConnection,
    identity: &PgIdentity,
) -> Result<(), DatabaseError> {
    let row = sqlx::query(
        "SELECT relation.relkind = 'r' AND relation.relpersistence = 'p' \
           AND relation.relowner = namespace.nspowner \
           AND NOT relation.relrowsecurity AND NOT relation.relforcerowsecurity \
           AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attribute attribute \
                WHERE attribute.attrelid = relation.oid AND attribute.attnum > 0 \
                  AND NOT attribute.attisdropped) = 7 \
           AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint constraint_ \
                WHERE constraint_.conrelid = relation.oid) = 3 \
           AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_index index_ \
                WHERE index_.indrelid = relation.oid) = 1 \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger trigger_ \
                           WHERE trigger_.tgrelid = relation.oid) \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint inbound \
                           WHERE inbound.contype = 'f' AND inbound.confrelid = relation.oid) \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy policy_ \
                           WHERE policy_.polrelid = relation.oid) \
         FROM pg_catalog.pg_class relation \
         JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
         WHERE namespace.nspname = $1 AND relation.relname = $2",
    )
    .bind(&identity.schema.name.0)
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
