//! Database migration system for Waddle Server
//!
//! Uses [refinery] for battle-tested, checksum-verified SQL migrations.
//!
//! SQL migration files live alongside this crate under:
//! - `migrations/global/`  – global database (auth, users, roster, etc.)
//! - `migrations/waddle/`  – per-Waddle databases (channels, messages, etc.)
//!
//! File naming follows refinery's convention: `V{version}__{description}.sql`
//! (e.g. `V001__auth_broker_schema.sql`).
//!
//! The multi-tenant architecture is preserved: `MigrationRunner::global()` and
//! `MigrationRunner::waddle()` each embed their own migration set at compile
//! time and run against the corresponding database independently.

use super::Database;
use super::DatabaseError;
use async_trait::async_trait;
use refinery_core::traits::r#async::{AsyncMigrate, AsyncQuery, AsyncTransaction};
use refinery_core::Migration;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tracing::{debug, info, instrument};

// Compile-time embedding of SQL migration files.
mod global_migrations {
    refinery::embed_migrations!("migrations/global");
}

mod waddle_migrations {
    refinery::embed_migrations!("migrations/waddle");
}

/// Thin adapter that implements refinery's async traits on top of a libsql connection.
///
/// `libsql::Connection` uses interior mutability (`Arc` internally), so the
/// underlying async methods only require `&self`; the `&mut self` bounds here
/// satisfy the refinery trait contracts.
struct LibsqlConnection<'a>(&'a libsql::Connection);

#[async_trait]
impl AsyncTransaction for LibsqlConnection<'_> {
    type Error = libsql::Error;

    async fn execute<'a, T: Iterator<Item = &'a str> + Send>(
        &mut self,
        queries: T,
    ) -> Result<usize, Self::Error> {
        let mut count = 0;
        for query in queries {
            self.0.execute_batch(query).await?;
            count += 1;
        }
        Ok(count)
    }
}

#[async_trait]
impl AsyncQuery<Vec<Migration>> for LibsqlConnection<'_> {
    async fn query(
        &mut self,
        query: &str,
    ) -> Result<Vec<Migration>, <Self as AsyncTransaction>::Error> {
        let mut rows = self.0.query(query, ()).await?;
        let mut applied = Vec::new();
        while let Some(row) = rows.next().await? {
            let version: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let applied_on: String = row.get(2)?;
            let checksum: String = row.get(3)?;
            let applied_on = OffsetDateTime::parse(&applied_on, &Rfc3339)
                .expect("applied_on stored in refinery_schema_history must be valid RFC 3339");
            applied.push(Migration::applied(
                version as refinery_core::SchemaVersion,
                name,
                applied_on,
                checksum
                    .parse::<u64>()
                    .expect("checksum stored in refinery_schema_history must be a valid u64"),
            ));
        }
        Ok(applied)
    }
}

impl AsyncMigrate for LibsqlConnection<'_> {}

/// Runs refinery-managed migrations against a [`Database`].
///
/// Use [`MigrationRunner::global`] for the global database and
/// [`MigrationRunner::waddle`] for per-Waddle databases.  Each instance
/// embeds its migration set at compile time, so no SQL is loaded from disk at
/// runtime.
pub struct MigrationRunner {
    runner: refinery::Runner,
    /// Total number of migrations in this set; used by [`has_pending`].
    total_migrations: usize,
}

impl MigrationRunner {
    /// Create a runner for global database migrations.
    pub fn global() -> Self {
        let runner = global_migrations::migrations::runner();
        let total_migrations = runner.get_migrations().len();
        Self {
            runner,
            total_migrations,
        }
    }

    /// Create a runner for per-waddle database migrations.
    pub fn waddle() -> Self {
        let runner = waddle_migrations::migrations::runner();
        let total_migrations = runner.get_migrations().len();
        Self {
            runner,
            total_migrations,
        }
    }

    /// Apply all pending migrations and return the versions that were applied.
    ///
    /// Refinery verifies checksums of previously applied migrations; if a
    /// migration file has been modified after it was applied the runner will
    /// return an error instead of silently re-applying.
    #[instrument(skip_all, fields(db_name = %db.name()))]
    pub async fn run(&self, db: &Database) -> Result<Vec<i64>, DatabaseError> {
        let conn = db.guard().await?;
        let mut adapter = LibsqlConnection(&*conn);
        let report = self
            .runner
            .run_async(&mut adapter)
            .await
            .map_err(|e| DatabaseError::MigrationFailed(e.to_string()))?;

        let applied: Vec<i64> = report
            .applied_migrations()
            .iter()
            .map(|m| m.version() as i64)
            .collect();

        if applied.is_empty() {
            debug!("No new migrations to apply");
        } else {
            info!("Applied {} new migration(s): {:?}", applied.len(), applied);
        }

        Ok(applied)
    }

    /// Return `true` if there are migrations that have not yet been applied.
    #[allow(dead_code)]
    #[instrument(skip_all, fields(db_name = %db.name()))]
    pub async fn has_pending(&self, db: &Database) -> Result<bool, DatabaseError> {
        let conn = db.guard().await?;
        let mut adapter = LibsqlConnection(&*conn);
        match self.runner.get_applied_migrations_async(&mut adapter).await {
            Ok(applied) => Ok(applied.len() < self.total_migrations),
            // If the schema history table does not exist yet all migrations are pending.
            Err(_) => Ok(true),
        }
    }

    /// Return the version of the last applied migration, or `None` if no
    /// migrations have been applied.
    #[allow(dead_code)]
    #[instrument(skip_all, fields(db_name = %db.name()))]
    pub async fn current_version(&self, db: &Database) -> Result<Option<i64>, DatabaseError> {
        let conn = db.guard().await?;
        let mut adapter = LibsqlConnection(&*conn);
        match self
            .runner
            .get_last_applied_migration_async(&mut adapter)
            .await
        {
            Ok(Some(m)) => Ok(Some(m.version() as i64)),
            Ok(None) => Ok(None),
            // Schema history table does not exist yet.
            Err(_) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_migration_runner_global() {
        let db = Database::in_memory("test-global").await.unwrap();
        let runner = MigrationRunner::global();

        // Run migrations
        let applied = runner.run(&db).await.unwrap();
        assert!(!applied.is_empty());

        // Running again should apply nothing (idempotent)
        let applied_again = runner.run(&db).await.unwrap();
        assert!(applied_again.is_empty());

        // Check version (single hard-cut schema migration)
        let version = runner.current_version(&db).await.unwrap();
        assert_eq!(version, Some(1));
    }

    #[tokio::test]
    async fn test_migration_runner_waddle() {
        let db = Database::in_memory("test-waddle").await.unwrap();
        let runner = MigrationRunner::waddle();

        // Run migrations
        let applied = runner.run(&db).await.unwrap();
        assert!(!applied.is_empty());

        // Verify tables exist
        let conn = db.guard().await.unwrap();
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
                (),
            )
            .await
            .unwrap();

        let mut tables = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            let name: String = row.get(0).unwrap();
            tables.push(name);
        }

        assert!(tables.contains(&"channels".to_string()));
        assert!(tables.contains(&"messages".to_string()));
        assert!(tables.contains(&"reactions".to_string()));
        assert!(tables.contains(&"attachments".to_string()));
    }

    #[tokio::test]
    async fn test_has_pending_migrations() {
        let db = Database::in_memory("test-pending").await.unwrap();
        let runner = MigrationRunner::global();

        // Should have pending migrations on a fresh database
        assert!(runner.has_pending(&db).await.unwrap());

        // Run migrations
        runner.run(&db).await.unwrap();

        // Should not have pending migrations
        assert!(!runner.has_pending(&db).await.unwrap());
    }

    #[tokio::test]
    async fn test_divergent_migration_is_rejected() {
        let db = Database::in_memory("test-divergent").await.unwrap();

        // Apply the real global migrations first.
        let runner = MigrationRunner::global();
        runner.run(&db).await.unwrap();

        // Build a runner whose V1 migration has different SQL (different checksum).
        let divergent = refinery::Runner::new(&[refinery_core::Migration::unapplied(
            "V001__auth_broker_schema",
            "SELECT 1;",
        )
        .unwrap()]);
        let conn = db.guard().await.unwrap();
        let mut adapter = LibsqlConnection(&*conn);
        let result = divergent.run_async(&mut adapter).await;

        // refinery must reject the divergent migration with an error.
        assert!(
            result.is_err(),
            "expected an error for divergent migration, got Ok"
        );
    }
}
