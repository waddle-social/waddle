//! Clustering-owned crash-recovery outbox for completed MUC destroys.
//!
//! The table is intentionally outside the application migration ledger: it is
//! support state for clustered MUC recovery, not application schema history.

use crate::db::{CatalogFenceLock, Database, DatabaseDriver, DatabaseError};
use std::time::Duration;

const READ_ONLY_ROLLBACK_WAIT: Duration = Duration::from_secs(5);

/// A restricted runtime may only adopt an owner-prepared outbox on its
/// physically attested main lane. The legacy constructor still owns bootstrap.
#[derive(Debug, thiserror::Error)]
pub enum MucDestroyRuntimeAttachError {
    #[error("MUC destroy outbox attachment requires PostgreSQL")]
    PostgresRequired,
    #[error("MUC destroy outbox attachment requires the main admission lane")]
    MainLaneRequired,
    #[error("MUC destroy outbox attachment requires a physically attested pool")]
    AttestedPoolRequired,
    #[error("MUC destroy outbox attachment found a different active schema")]
    ActiveSchemaMismatch,
    #[error("could not verify the PostgreSQL {setting} bound for MUC destroy outbox attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[error("MUC destroy outbox relation or index has an unexpected shape")]
    InvalidSchema,
    #[error("the MUC destroy outbox runtime role or grants are unsafe")]
    UnsafeRuntimeRole,
    #[error("failed to inspect the MUC destroy outbox relation: {0}")]
    Relation(#[source] DatabaseError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// Dedicated transaction-scoped Postgres advisory lock for MUC destroy
/// completion outbox bootstrap. It is distinct from the clustering claims
/// lock (`6_841_445_497_037_937_991`), migration-ledger lock
/// (`6_841_445_497_037_937_992`), lineage lock
/// (`6_841_445_497_037_937_993`), and MUC room-schema lock
/// (`6_841_445_497_037_937_994`) because it protects only this outbox.
const MUC_DESTROY_COMPLETION_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_995;

/// Owns startup schema bootstrap for the MUC destroy completion outbox.
#[derive(Clone)]
pub struct MucDestroyCompletionOutboxStore {
    _db: Database,
}

impl MucDestroyCompletionOutboxStore {
    pub async fn try_new(db: Database) -> Result<Self, DatabaseError> {
        Self::ensure_schema(&db).await?;
        Ok(Self { _db: db })
    }

    /// Attach the exact restricted main-pool handle without owner DDL or a
    /// reconnectable URL. This structural snapshot does not grant the runtime
    /// writer privileges or fence later privileged schema changes. Owner and
    /// migrator membership is provisioned by the separate owner handoff.
    pub async fn attach_existing(db: Database) -> Result<Self, MucDestroyRuntimeAttachError> {
        if db.driver() != DatabaseDriver::Postgres {
            return Err(MucDestroyRuntimeAttachError::PostgresRequired);
        }
        if !db.is_main_lane() {
            return Err(MucDestroyRuntimeAttachError::MainLaneRequired);
        }
        let capability = db
            .attested_pg_writer_capability()
            .ok_or(MucDestroyRuntimeAttachError::AttestedPoolRequired)?;
        db.verify_attested_pg_writer_capability(&capability)
            .map_err(DatabaseError::from)?;

        let mut tx = db.begin_catalog_fenced_read_only(&[]).await?;
        crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
            .await
            .map_err(|error| match error {
                crate::db::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { setting } => {
                    MucDestroyRuntimeAttachError::InvalidTimeoutSetting { setting }
                }
                crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                    MucDestroyRuntimeAttachError::Database(error)
                }
            })?;
        let expected = &capability.identity().schema;
        let schema = &expected.name.0;
        let qualified = format!(
            "\"{}\".\"clustering_muc_destroy_outbox\"",
            schema.replace('"', "\"\"")
        );
        // Catalog-S excludes owner schema work while this read-only check is
        // in progress. Lock the exact relation before the first snapshot-forming
        // SELECT so the metadata snapshot follows any lock wait.
        tx.execute(&format!("LOCK TABLE {qualified} IN ACCESS SHARE MODE"), ())
            .await
            .map_err(MucDestroyRuntimeAttachError::Relation)?;
        capability.attest_transaction(&mut tx).await?;
        let mut rows = tx
            .query(
                "SELECT n.nspname::text, n.oid::bigint FROM pg_catalog.pg_namespace n \
                 WHERE n.nspname = pg_catalog.current_schema()",
                (),
            )
            .await?;
        let active = rows.next().await?;
        let matches = if let Some(row) = active {
            row.get::<String>(0)? == schema.as_str()
                && row.get::<i64>(1)? == i64::from(expected.oid)
        } else {
            false
        };
        drop(rows);
        if !matches {
            return Err(MucDestroyRuntimeAttachError::ActiveSchemaMismatch);
        }
        let mut rows = tx
            .query(
                "SELECT c.relpersistence = 'p' AND NOT c.relispartition \
                   AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
                   AND c.reloftype = 0 AND c.relchecks = 0 \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr \
                                   WHERE tr.tgrelid = c.oid AND NOT tr.tgisinternal) \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw \
                                   WHERE rw.ev_class = c.oid) \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inh \
                                   WHERE inh.inhrelid = c.oid OR inh.inhparent = c.oid) \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint co \
                                   WHERE co.conrelid = c.oid AND co.contype <> 'p') \
                 FROM pg_catalog.pg_class c \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = ? AND c.relname = 'clustering_muc_destroy_outbox' \
                   AND c.relkind = 'r'",
                crate::db_params![schema.clone()],
            )
            .await?;
        let plain_table = match rows.next().await? {
            Some(row) => row.get::<bool>(0)?,
            None => false,
        };
        drop(rows);
        if !plain_table {
            return Err(MucDestroyRuntimeAttachError::InvalidSchema);
        }
        tx.query(
            &format!(
                "SELECT attempt_id, payload_json, lifecycle_id, origin_instance_id, \
                 available_at_ms, lease_token, leased_at_ms FROM {qualified} WHERE false"
            ),
            (),
        )
        .await
        .map_err(MucDestroyRuntimeAttachError::Relation)?;

        let mut rows = tx
            .query(
                "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), \
                 a.attnotnull, a.atthasdef, a.attgenerated::text, a.attidentity::text \
                 FROM pg_catalog.pg_attribute a \
                 JOIN pg_catalog.pg_class t ON t.oid = a.attrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                 WHERE n.nspname = ? AND t.relname = 'clustering_muc_destroy_outbox' \
                   AND t.relkind = 'r' AND a.attnum > 0 AND NOT a.attisdropped",
                crate::db_params![schema.clone()],
            )
            .await?;
        let mut columns = std::collections::HashMap::new();
        while let Some(row) = rows.next().await? {
            columns.insert(
                row.get::<String>(0)?,
                (
                    row.get::<String>(1)?,
                    row.get::<bool>(2)?,
                    row.get::<bool>(3)?,
                    row.get::<String>(4)?,
                    row.get::<String>(5)?,
                ),
            );
        }
        drop(rows);
        if columns.len() != 7 {
            return Err(MucDestroyRuntimeAttachError::InvalidSchema);
        }
        for (name, kind, not_null) in [
            ("attempt_id", "text", true),
            ("payload_json", "text", true),
            ("lifecycle_id", "text", false),
            ("origin_instance_id", "text", false),
            ("available_at_ms", "bigint", true),
            ("lease_token", "text", false),
            ("leased_at_ms", "bigint", false),
        ] {
            if columns.get(name).is_none_or(|actual| {
                actual
                    != &(
                        kind.to_owned(),
                        not_null,
                        false,
                        String::new(),
                        String::new(),
                    )
            }) {
                return Err(MucDestroyRuntimeAttachError::InvalidSchema);
            }
        }
        let mut rows = tx
            .query(
                "SELECT i.indisprimary, i.indisunique, i.indisvalid, i.indisready, i.indislive, \
                 i.indnkeyatts::bigint, i.indnatts::bigint, am.amname::text, \
                 a0.attname::text, a1.attname::text, i.indimmediate, \
                 co.condeferrable, co.condeferred, co.convalidated \
                 FROM pg_catalog.pg_index i \
                 JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                 JOIN pg_catalog.pg_class x ON x.oid = i.indexrelid \
                 JOIN pg_catalog.pg_am am ON am.oid = x.relam \
                 LEFT JOIN pg_catalog.pg_constraint co ON co.conindid = i.indexrelid \
                   AND co.conrelid = t.oid AND co.contype = 'p' \
                 LEFT JOIN pg_catalog.pg_attribute a0 ON a0.attrelid = t.oid AND a0.attnum = i.indkey[0] \
                 LEFT JOIN pg_catalog.pg_attribute a1 ON a1.attrelid = t.oid AND a1.attnum = i.indkey[1] \
                 WHERE n.nspname = ? AND t.relname = 'clustering_muc_destroy_outbox' \
                   AND (i.indisprimary OR x.relname = 'clustering_muc_destroy_outbox_due_idx') \
                   AND i.indpred IS NULL AND i.indexprs IS NULL",
                crate::db_params![schema.clone()],
            )
            .await?;
        let mut primary = false;
        let mut due = false;
        let mut index_count = 0;
        while let Some(row) = rows.next().await? {
            index_count += 1;
            let is_primary = row.get::<bool>(0)?;
            let unique = row.get::<bool>(1)?;
            let healthy = row.get::<bool>(2)? && row.get::<bool>(3)? && row.get::<bool>(4)?;
            let keys = row.get::<i64>(5)?;
            let attributes = row.get::<i64>(6)?;
            let btree = row.get::<String>(7)? == "btree";
            let first = row.get::<Option<String>>(8)?;
            let second = row.get::<Option<String>>(9)?;
            let immediate = row.get::<bool>(10)?;
            if is_primary {
                primary |= healthy
                    && unique
                    && immediate
                    && row.get::<Option<bool>>(11)? == Some(false)
                    && row.get::<Option<bool>>(12)? == Some(false)
                    && row.get::<Option<bool>>(13)? == Some(true)
                    && keys == 1
                    && attributes == 1
                    && first.as_deref() == Some("attempt_id");
            } else {
                due |= healthy
                    && !unique
                    && btree
                    && keys == 2
                    && attributes == 2
                    && first.as_deref() == Some("available_at_ms")
                    && second.as_deref() == Some("leased_at_ms");
            }
        }
        drop(rows);
        if !primary || !due || index_count != 2 {
            return Err(MucDestroyRuntimeAttachError::InvalidSchema);
        }
        let mut rows = tx
            .query(
                "SELECT count(*)::bigint FROM pg_catalog.pg_index i \
                 JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                 WHERE n.nspname = ? AND t.relname = 'clustering_muc_destroy_outbox'",
                crate::db_params![schema.clone()],
            )
            .await?;
        let exact_indexes = match rows.next().await? {
            Some(row) => row.get::<i64>(0)? == 2,
            None => false,
        };
        drop(rows);
        if !exact_indexes {
            return Err(MucDestroyRuntimeAttachError::InvalidSchema);
        }
        let mut rows = tx
            .query(
                "SELECT session_user::text = current_user::text \
                   AND current_user::text = runtime.rolname \
                   AND runtime.rolcanlogin AND NOT runtime.rolsuper \
                   AND NOT runtime.rolcreatedb AND NOT runtime.rolcreaterole \
                   AND NOT runtime.rolreplication AND NOT runtime.rolbypassrls \
                   AND runtime.oid <> database.datdba \
                   AND NOT pg_catalog.has_database_privilege(runtime.oid, database.oid, 'CREATE,TEMP') \
                   AND NOT pg_catalog.has_parameter_privilege(runtime.oid, 'session_replication_role', 'SET') \
                   AND pg_catalog.current_setting('session_replication_role') = 'origin' \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m \
                                   WHERE m.member = runtime.oid OR m.roleid = runtime.oid) \
                   AND pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'USAGE') \
                   AND NOT pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'CREATE') \
                   AND ns.nspowner = owner.oid AND relation.relowner = owner.oid \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                       CASE WHEN pg_catalog.cardinality(ns.nspacl) > 0 THEN ns.nspacl \
                            ELSE pg_catalog.acldefault('n', ns.nspowner) END \
                   ) acl WHERE acl.grantee = 0 \
                       OR (acl.grantee <> owner.oid \
                           AND (acl.privilege_type <> 'USAGE' OR acl.is_grantable))) \
                   AND owner.oid <> database.datdba AND NOT owner.rolcanlogin \
                   AND NOT owner.rolsuper AND NOT owner.rolcreaterole \
                   AND NOT owner.rolcreatedb AND NOT owner.rolreplication \
                   AND NOT owner.rolbypassrls \
                   AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'SELECT') \
                   AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'INSERT') \
                   AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'UPDATE') \
                   AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'DELETE') \
                   AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'TRUNCATE, REFERENCES, TRIGGER') \
                   AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, \
                       'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION, UPDATE WITH GRANT OPTION, \
                        DELETE WITH GRANT OPTION, TRUNCATE WITH GRANT OPTION, \
                        REFERENCES WITH GRANT OPTION, TRIGGER WITH GRANT OPTION') \
                   AND NOT pg_catalog.has_any_column_privilege(runtime.oid, relation.oid, \
                       'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION, \
                        UPDATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION') \
                   AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000 \
                            THEN NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'MAINTAIN') \
                            ELSE true END \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                       CASE WHEN pg_catalog.cardinality(relation.relacl) > 0 THEN relation.relacl \
                            ELSE pg_catalog.acldefault('r', relation.relowner) END \
                   ) acl WHERE acl.grantee NOT IN (owner.oid, runtime.oid) \
                       OR (acl.grantee = runtime.oid AND (acl.is_grantable \
                           OR acl.privilege_type NOT IN ('SELECT', 'INSERT', 'UPDATE', 'DELETE')))) \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a \
                       CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) acl \
                       WHERE a.attrelid = relation.oid AND a.attnum > 0 \
                         AND NOT a.attisdropped) \
                 FROM pg_catalog.pg_roles runtime \
                 JOIN pg_catalog.pg_namespace ns ON ns.nspname = ? \
                 JOIN pg_catalog.pg_roles owner ON owner.oid = ns.nspowner \
                 JOIN pg_catalog.pg_database database ON database.datname = pg_catalog.current_database() \
                 JOIN pg_catalog.pg_class relation ON relation.relnamespace = ns.oid \
                   AND relation.relname = 'clustering_muc_destroy_outbox' AND relation.relkind = 'r' \
                 WHERE runtime.rolname = current_user",
                crate::db_params![schema.clone()],
            )
            .await?;
        let safe = match rows.next().await? {
            Some(row) => row.get::<bool>(0)?,
            None => false,
        };
        drop(rows);
        if !safe {
            return Err(MucDestroyRuntimeAttachError::UnsafeRuntimeRole);
        }
        if !tx.rollback_bounded_discard(READ_ONLY_ROLLBACK_WAIT).await {
            return Err(DatabaseError::CatalogMutationRollbackUnconfirmed.into());
        }
        Ok(Self { _db: db })
    }

    async fn ensure_schema(db: &Database) -> Result<(), DatabaseError> {
        match db.driver() {
            DatabaseDriver::Postgres => Self::ensure_postgres_schema(db).await,
            DatabaseDriver::Sqlite => Self::ensure_sqlite_schema(db).await,
        }
    }

    async fn ensure_postgres_schema(db: &Database) -> Result<(), DatabaseError> {
        let mut tx = db
            .begin_catalog_fenced(&[CatalogFenceLock::Exclusive(
                MUC_DESTROY_COMPLETION_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY,
            )])
            .await?;
        bound_owner_bootstrap_waits(&mut tx).await?;
        tx.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_muc_destroy_outbox (
                attempt_id      TEXT PRIMARY KEY,
                payload_json    TEXT NOT NULL,
                lifecycle_id    TEXT,
                origin_instance_id TEXT,
                available_at_ms BIGINT NOT NULL,
                lease_token     TEXT,
                leased_at_ms    BIGINT
            )
            "#,
            (),
        )
        .await?;
        tx.execute(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1
                    FROM pg_attribute
                    WHERE attrelid = 'clustering_muc_destroy_outbox'::regclass
                      AND attname = 'origin_instance_id'
                      AND NOT attisdropped
                ) THEN
                    ALTER TABLE clustering_muc_destroy_outbox ADD COLUMN origin_instance_id TEXT;
                END IF;
            END $$
            "#,
            (),
        )
        .await?;
        // `ADD COLUMN IF NOT EXISTS` still takes ACCESS EXCLUSIVE before
        // PostgreSQL checks the catalog. Probe pg_attribute first so normal
        // rolling starts do not queue every destroy behind an unnecessary
        // table lock; the bootstrap that actually adds the column remains
        // serialized by the advisory lock above.
        tx.execute(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1
                    FROM pg_attribute
                    WHERE attrelid = 'clustering_muc_destroy_outbox'::regclass
                      AND attname = 'lifecycle_id'
                      AND NOT attisdropped
                ) THEN
                    ALTER TABLE clustering_muc_destroy_outbox ADD COLUMN lifecycle_id TEXT;
                END IF;
            END $$
            "#,
            (),
        )
        .await?;
        // Unlike `CREATE INDEX IF NOT EXISTS`, this catalog guard does not
        // acquire a relation lock when the index already exists.
        tx.execute(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1
                    FROM pg_index i
                    JOIN pg_class c ON c.oid = i.indexrelid
                    WHERE i.indrelid = 'clustering_muc_destroy_outbox'::regclass
                      AND c.relname = 'clustering_muc_destroy_outbox_due_idx'
                ) THEN
                    CREATE INDEX clustering_muc_destroy_outbox_due_idx
                        ON clustering_muc_destroy_outbox (available_at_ms, leased_at_ms);
                END IF;
            END $$
            "#,
            (),
        )
        .await?;
        tx.commit().await
    }

    async fn bound_owner_bootstrap_waits(
        tx: &mut crate::db::Transaction<'_>,
    ) -> Result<(), DatabaseError> {
        tx.execute("SET LOCAL lock_timeout = '3s'", ()).await?;
        tx.execute("SET LOCAL statement_timeout = '30s'", ())
            .await?;
        Ok(())
    }

    async fn ensure_sqlite_schema(db: &Database) -> Result<(), DatabaseError> {
        let connection = db.guard().await?;
        connection
            .execute(
                r#"
                CREATE TABLE IF NOT EXISTS clustering_muc_destroy_outbox (
                attempt_id      TEXT PRIMARY KEY,
                payload_json    TEXT NOT NULL,
                lifecycle_id    TEXT,
                origin_instance_id TEXT,
                available_at_ms INTEGER NOT NULL,
                    lease_token     TEXT,
                    leased_at_ms    INTEGER
                )
                "#,
                (),
            )
            .await?;
        if let Err(error) = connection
            .execute(
                "ALTER TABLE clustering_muc_destroy_outbox ADD COLUMN lifecycle_id TEXT",
                (),
            )
            .await
        {
            let message = error.to_string().to_lowercase();
            if !message.contains("duplicate column") && !message.contains("already exists") {
                return Err(error);
            }
        }
        if let Err(error) = connection
            .execute(
                "ALTER TABLE clustering_muc_destroy_outbox ADD COLUMN origin_instance_id TEXT",
                (),
            )
            .await
        {
            let message = error.to_string().to_lowercase();
            if !message.contains("duplicate column") && !message.contains("already exists") {
                return Err(error);
            }
        }
        connection
            .execute(
                "CREATE INDEX IF NOT EXISTS clustering_muc_destroy_outbox_due_idx \
                 ON clustering_muc_destroy_outbox (available_at_ms, leased_at_ms)",
                (),
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DEFAULT_CONTROL_PLANE_POOL_SIZE, DatabaseConfig};

    #[tokio::test]
    async fn runtime_attach_refuses_sqlite_before_schema_work() {
        let db = Database::in_memory("muc-destroy-runtime-refusal")
            .await
            .expect("SQLite fixture");
        let error = MucDestroyCompletionOutboxStore::attach_existing(db)
            .await
            .err()
            .expect("runtime attachment requires PostgreSQL");
        assert!(matches!(
            error,
            MucDestroyRuntimeAttachError::PostgresRequired
        ));
    }

    async fn column_nullability(db: &Database, column: &str) -> Option<String> {
        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT is_nullable FROM information_schema.columns \
                 WHERE table_schema = current_schema() \
                   AND table_name = 'clustering_muc_destroy_outbox' \
                   AND column_name = ?",
                crate::db_params![column],
            )
            .await
            .expect("query outbox column nullability");
        rows.next()
            .await
            .expect("read outbox column nullability")
            .map(|row| row.get(0).expect("decode outbox column nullability"))
    }

    #[tokio::test]
    async fn schema_has_expected_catalog_shape() {
        let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
            return;
        };
        let db = Database::from_config(
            "muc-destroy-completion-outbox-schema-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        MucDestroyCompletionOutboxStore::try_new(db.clone())
            .await
            .expect("initialize MUC destroy completion outbox");
        // A normal restart observes the catalog and skips the ALTER rather
        // than taking an unnecessary table lock.
        MucDestroyCompletionOutboxStore::try_new(db.clone())
            .await
            .expect("reinitialize MUC destroy completion outbox");

        let error = MucDestroyCompletionOutboxStore::attach_existing(db.clone())
            .await
            .err()
            .expect("an ordinary PostgreSQL pool is not an attested runtime handle");
        assert!(matches!(
            error,
            MucDestroyRuntimeAttachError::AttestedPoolRequired
        ));

        for (column, nullable) in [
            ("attempt_id", "NO"),
            ("payload_json", "NO"),
            ("lifecycle_id", "YES"),
            ("origin_instance_id", "YES"),
            ("available_at_ms", "NO"),
            ("lease_token", "YES"),
            ("leased_at_ms", "YES"),
        ] {
            assert_eq!(
                column_nullability(&db, column).await,
                Some(nullable.to_string()),
                "destroy completion outbox column {column} must have expected nullability"
            );
        }

        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM pg_index i \
                 JOIN pg_class index_relation ON index_relation.oid = i.indexrelid \
                 WHERE index_relation.relname = ? \
                   AND i.indrelid = to_regclass('clustering_muc_destroy_outbox')",
                crate::db_params!["clustering_muc_destroy_outbox_due_idx"],
            )
            .await
            .expect("query destroy completion outbox index catalog");
        let count: i64 = rows
            .next()
            .await
            .expect("read destroy completion outbox index catalog")
            .expect("destroy completion outbox index count row")
            .get(0)
            .expect("decode destroy completion outbox index count");
        assert_eq!(count, 1, "destroy completion outbox due index must exist");
    }
}
