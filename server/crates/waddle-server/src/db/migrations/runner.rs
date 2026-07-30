use std::collections::HashMap;

use tracing::{debug, info, instrument, warn};

use super::sql::migrations_table_sql;
use super::{global, waddle, Migration};
use crate::db::{ConnectionGuard, Database, DatabaseDriver, DatabaseError};

/// Migration runner for applying migrations to a database
pub struct MigrationRunner {
    pub(super) migrations: Vec<Migration>,
}

impl MigrationRunner {
    /// Create a new migration runner with the given migrations
    pub fn new(migrations: Vec<Migration>) -> Self {
        let mut sorted = migrations;
        sorted.sort_by_key(|m| m.version);
        Self { migrations: sorted }
    }

    /// Create a runner for global database migrations
    #[cfg(test)]
    pub fn global() -> Self {
        Self::single()
    }

    /// Create a runner for channel/message schema migrations.
    #[cfg(test)]
    pub fn waddle() -> Self {
        Self::new(waddle::all())
    }

    /// Create a runner for single-database mode.
    ///
    /// This composes global + channel/message schema migrations into one ordered
    /// stream so they share one migration history without version collisions.
    pub fn single() -> Self {
        let mut migrations = global::all();
        migrations.extend(waddle::all());
        Self::new(migrations)
    }

    /// Run all pending migrations on the database
    #[instrument(skip_all, fields(db_name = %db.name()))]
    pub async fn run(&self, db: &Database) -> Result<Vec<i64>, DatabaseError> {
        let conn = db.guard().await?;
        self.run_with_connection(&conn, db.driver()).await
    }

    /// Internal method to run migrations with a given connection
    async fn run_with_connection(
        &self,
        conn: &ConnectionGuard,
        driver: DatabaseDriver,
    ) -> Result<Vec<i64>, DatabaseError> {
        // Ensure migrations table exists
        conn.execute(migrations_table_sql(driver), ())
            .await
            .map_err(|e| {
                DatabaseError::MigrationFailed(format!("Failed to create migrations table: {}", e))
            })?;

        // Get applied migrations (version + description).
        let mut applied_rows: Vec<(i64, String)> = Vec::new();
        let mut rows = conn
            .query(
                "SELECT version, description FROM _migrations ORDER BY version",
                (),
            )
            .await
            .map_err(|e| {
                DatabaseError::MigrationFailed(format!("Failed to query migrations: {}", e))
            })?;

        while let Some(row) = rows.next().await.map_err(|e| {
            DatabaseError::MigrationFailed(format!("Failed to read migration row: {}", e))
        })? {
            let version: i64 = row.get(0).map_err(|e| {
                DatabaseError::MigrationFailed(format!("Failed to get version from row: {}", e))
            })?;
            let description: String = row.get(1).map_err(|e| {
                DatabaseError::MigrationFailed(format!("Failed to get description from row: {}", e))
            })?;
            applied_rows.push((version, description));
        }

        // Migration history is durable authority. An unknown version or a
        // changed description means this binary cannot prove that its DDL is
        // compatible with the schema already on disk. In particular, never
        // drop `_migrations` here: doing so makes every V1 migration look
        // pending and can replay destructive DDL against a forward database.
        let expected: HashMap<i64, &str> = self
            .migrations
            .iter()
            .map(|m| (m.version, m.description.as_str()))
            .collect();
        let has_incompatible_history = applied_rows.iter().any(|(version, description)| {
            expected
                .get(version)
                .map(|expected_desc| *expected_desc != description.as_str())
                .unwrap_or(true)
        });

        if has_incompatible_history {
            warn!("Incompatible migration history detected; refusing to mutate schema");
            return Err(DatabaseError::MigrationFailed(
                "incompatible migration history; refusing to mutate schema".to_string(),
            ));
        }

        let applied: Vec<i64> = applied_rows.iter().map(|(version, _)| *version).collect();

        debug!("Already applied migrations: {:?}", applied);

        // Apply pending migrations
        let mut newly_applied = Vec::new();
        for migration in &self.migrations {
            if applied.contains(&migration.version) {
                debug!("Skipping already applied migration v{}", migration.version);
                continue;
            }

            info!(
                "Applying migration v{}: {}",
                migration.version, migration.description
            );

            // Execute migration SQL using batch execution (driver-specific dialect)
            let sql = migration.sql_for(driver);
            conn.execute_batch(sql).await.map_err(|e| {
                DatabaseError::MigrationFailed(format!(
                    "Migration v{} failed: {}",
                    migration.version, e
                ))
            })?;

            // Record the migration. `ON CONFLICT DO NOTHING`: there is
            // no cross-node lock (#1338), so two replicas booting
            // concurrently can both pass the applied-versions check and
            // race this insert; the loser must not crash-loop on the
            // duplicate primary key after (idempotent) DDL succeeded.
            conn.execute(
                "INSERT INTO _migrations (version, description) VALUES (?, ?) \
                 ON CONFLICT (version) DO NOTHING",
                (migration.version, migration.description.as_str()),
            )
            .await
            .map_err(|e| {
                DatabaseError::MigrationFailed(format!(
                    "Failed to record migration v{}: {}",
                    migration.version, e
                ))
            })?;

            newly_applied.push(migration.version);
            info!("Applied migration v{}", migration.version);
        }

        if newly_applied.is_empty() {
            debug!("No new migrations to apply");
        } else {
            info!("Applied {} new migrations", newly_applied.len());
        }

        Ok(newly_applied)
    }

    /// Get the current schema version
    #[cfg(test)]
    #[instrument(skip_all, fields(db_name = %db.name()))]
    pub async fn current_version(&self, db: &Database) -> Result<Option<i64>, DatabaseError> {
        let conn = db.guard().await?;
        self.current_version_with_connection(&conn, db.driver())
            .await
    }

    /// Internal method to get current version with a given connection
    #[cfg(test)]
    async fn current_version_with_connection(
        &self,
        conn: &ConnectionGuard,
        driver: DatabaseDriver,
    ) -> Result<Option<i64>, DatabaseError> {
        conn.execute(migrations_table_sql(driver), ())
            .await
            .map_err(|e| {
                DatabaseError::MigrationFailed(format!("Failed to ensure migrations table: {}", e))
            })?;

        // Get the latest version
        let mut rows = conn
            .query("SELECT MAX(version) FROM _migrations", ())
            .await
            .map_err(|e| {
                DatabaseError::QueryFailed(format!("Failed to query max version: {}", e))
            })?;

        match rows
            .next()
            .await
            .map_err(|e| DatabaseError::QueryFailed(format!("Failed to read max version: {}", e)))?
        {
            Some(row) => {
                let version: Option<i64> = row.get(0).ok();
                Ok(version)
            }
            None => Ok(None),
        }
    }

    /// Check if there are pending migrations
    #[cfg(test)]
    pub async fn has_pending(&self, db: &Database) -> Result<bool, DatabaseError> {
        let current = self.current_version(db).await?.unwrap_or(0);
        let latest = self.migrations.last().map(|m| m.version).unwrap_or(0);
        // A forward/unknown history is not "up to date" merely because its
        // largest version exceeds this binary's final migration. `run()`
        // rejects that state without mutation; expose it as pending here so
        // tests and callers cannot mistake it for a converged schema.
        Ok(current != latest)
    }
}
