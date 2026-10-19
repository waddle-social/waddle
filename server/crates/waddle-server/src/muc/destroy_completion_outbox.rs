//! Clustering-owned crash-recovery outbox for completed MUC destroys.
//!
//! The table is intentionally outside the application migration ledger: it is
//! support state for clustered MUC recovery, not application schema history.

use crate::db::{Database, DatabaseDriver, DatabaseError};

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
    pub async fn new(db: Database) -> Result<Self, DatabaseError> {
        Self::ensure_schema(&db).await?;
        Ok(Self { _db: db })
    }

    async fn ensure_schema(db: &Database) -> Result<(), DatabaseError> {
        match db.driver() {
            DatabaseDriver::Postgres => Self::ensure_postgres_schema(db).await,
            DatabaseDriver::Sqlite => Self::ensure_sqlite_schema(db).await,
        }
    }

    async fn ensure_postgres_schema(db: &Database) -> Result<(), DatabaseError> {
        let mut tx = db.begin().await?;
        // The transaction isolation command must remain first: a bootstrap
        // loser needs to observe the winner's committed DDL.
        tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await?;
        tx.query(
            "SELECT pg_advisory_xact_lock(?)",
            crate::db_params![MUC_DESTROY_COMPLETION_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY],
        )
        .await?;
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
    use crate::db::{DatabaseConfig, DEFAULT_CONTROL_PLANE_POOL_SIZE};

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
        let Some(url) = std::env::var("WADDLE_TEST_POSTGRES_URL").ok() else {
            return;
        };
        let db = Database::from_config(
            "muc-destroy-completion-outbox-schema-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        MucDestroyCompletionOutboxStore::new(db.clone())
            .await
            .expect("initialize MUC destroy completion outbox");
        // A normal restart observes the catalog and skips the ALTER rather
        // than taking an unnecessary table lock.
        MucDestroyCompletionOutboxStore::new(db.clone())
            .await
            .expect("reinitialize MUC destroy completion outbox");

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
