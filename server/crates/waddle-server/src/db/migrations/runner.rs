use std::collections::{HashMap, HashSet};

use tracing::{debug, info, instrument};

use super::sql::migrations_table_sql;
use super::{
    global, migration_checksum, waddle, Migration, MigrationLedgerError, MigrationNamespace,
};
#[cfg(test)]
use crate::db::ConnectionGuard;
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

/// Dedicated, cluster-wide Postgres advisory-lock key for serializing the
/// append-only migration ledger. This differs from the claims lock key in
/// `clustering::claims`; SQLite is single-node, where `BEGIN IMMEDIATE`
/// provides the corresponding write serialization.
pub(super) const MIGRATION_LEDGER_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_992;

/// Migration runner for applying migrations to a database.
pub struct MigrationRunner {
    pub(super) migrations: Vec<Migration>,
    owned_namespaces: HashSet<MigrationNamespace>,
}

impl MigrationRunner {
    /// Create a new migration runner with the given migrations. A runner owns
    /// every namespace represented by its catalog; ledger rows outside those
    /// namespaces intentionally belong to another runner and are ignored.
    pub fn new(migrations: Vec<Migration>) -> Self {
        let mut sorted = migrations;
        sorted.sort_by_key(|migration| migration.version);
        let owned_namespaces = sorted
            .iter()
            .map(|migration| MigrationNamespace::of(migration.version))
            .collect();
        Self {
            migrations: sorted,
            owned_namespaces,
        }
    }

    /// Create a runner for global database migrations.
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
    /// This composes global + channel/message schema migrations into one
    /// ordered stream so they share one migration history without collisions.
    pub fn single() -> Self {
        let mut migrations = global::all();
        migrations.extend(waddle::all());
        Self::new(migrations)
    }

    /// Run all pending migrations on one transaction. Postgres serializes
    /// starters with a transaction-scoped advisory lock; SQLite is a
    /// single-node deployment and uses its immediate write lock instead.
    #[instrument(skip_all, fields(db_name = %db.name()))]
    pub async fn run(&self, db: &Database) -> Result<Vec<i64>, DatabaseError> {
        let driver = db.driver();
        let mut tx = match driver {
            DatabaseDriver::Postgres => {
                let mut tx = db.begin().await?;
                tx.query(
                    "SELECT pg_advisory_xact_lock(?)",
                    crate::db_params![MIGRATION_LEDGER_ADVISORY_LOCK_KEY],
                )
                .await?;
                tx
            }
            DatabaseDriver::Sqlite => db.begin_immediate().await?,
        };

        let checksum_column_added = self.bootstrap(&mut tx, driver).await?;
        let applied_rows = self.read_ledger(&mut tx).await?;
        let adoption_backfill =
            self.validate_ledger(&applied_rows, driver, checksum_column_added)?;

        for (version, checksum) in adoption_backfill {
            tx.execute(
                "UPDATE _migrations SET checksum = ? WHERE version = ? AND checksum IS NULL",
                (checksum, version),
            )
            .await?;
        }

        let applied: HashSet<i64> = applied_rows
            .iter()
            .filter(|row| self.owns_version(row.version))
            .map(|row| row.version)
            .collect();
        debug!(?applied, "Already applied migrations");

        let mut newly_applied = Vec::new();
        for migration in &self.migrations {
            if applied.contains(&migration.version) {
                debug!(
                    version = migration.version,
                    "Skipping already applied migration"
                );
                continue;
            }

            info!(
                version = migration.version,
                description = %migration.description,
                "Applying migration"
            );
            tx.execute_batch(migration.sql_for(driver)).await?;
            tx.execute(
                "INSERT INTO _migrations (version, description, checksum) VALUES (?, ?, ?)",
                crate::db_params![
                    migration.version,
                    migration.description.as_str(),
                    migration_checksum(migration, driver)
                ],
            )
            .await?;

            newly_applied.push(migration.version);
            info!(version = migration.version, "Applied migration");
        }

        tx.commit().await?;

        if newly_applied.is_empty() {
            debug!("No new migrations to apply");
        } else {
            info!(count = newly_applied.len(), "Applied new migrations");
        }

        Ok(newly_applied)
    }

    async fn bootstrap(
        &self,
        tx: &mut Transaction<'_>,
        driver: DatabaseDriver,
    ) -> Result<bool, DatabaseError> {
        tx.execute(migrations_table_sql(driver), ()).await?;

        let has_checksum_column = Self::has_checksum_column(tx, driver).await?;
        match driver {
            DatabaseDriver::Postgres => {
                tx.execute(
                    "ALTER TABLE _migrations ADD COLUMN IF NOT EXISTS checksum TEXT",
                    (),
                )
                .await?;
            }
            DatabaseDriver::Sqlite if !has_checksum_column => {
                tx.execute("ALTER TABLE _migrations ADD COLUMN checksum TEXT", ())
                    .await?;
            }
            DatabaseDriver::Sqlite => {}
        }

        Ok(!has_checksum_column)
    }

    async fn has_checksum_column(
        tx: &mut Transaction<'_>,
        driver: DatabaseDriver,
    ) -> Result<bool, DatabaseError> {
        let mut rows = match driver {
            DatabaseDriver::Sqlite => tx.query("PRAGMA table_info('_migrations')", ()).await?,
            DatabaseDriver::Postgres => {
                tx.query(
                    "SELECT column_name FROM information_schema.columns \
                     WHERE table_schema = current_schema() \
                     AND table_name = ? AND column_name = ?",
                    ("_migrations", "checksum"),
                )
                .await?
            }
        };

        while let Some(row) = rows.next().await? {
            let column_name: String = match driver {
                DatabaseDriver::Sqlite => row.get(1)?,
                DatabaseDriver::Postgres => row.get(0)?,
            };
            if column_name == "checksum" {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn read_ledger(&self, tx: &mut Transaction<'_>) -> Result<Vec<LedgerRow>, DatabaseError> {
        let mut rows = tx
            .query(
                "SELECT version, description, checksum FROM _migrations ORDER BY version",
                (),
            )
            .await?;
        let mut ledger = Vec::new();
        while let Some(row) = rows.next().await? {
            ledger.push(LedgerRow {
                version: row.get(0)?,
                description: row.get(1)?,
                checksum: row.get(2)?,
            });
        }
        Ok(ledger)
    }

    fn validate_ledger(
        &self,
        applied_rows: &[LedgerRow],
        driver: DatabaseDriver,
        checksum_column_added: bool,
    ) -> Result<Vec<(i64, String)>, DatabaseError> {
        let expected: HashMap<i64, &Migration> = self
            .migrations
            .iter()
            .map(|migration| (migration.version, migration))
            .collect();
        let mut applied_by_namespace: HashMap<MigrationNamespace, Vec<i64>> = HashMap::new();
        let mut adoption_backfill = Vec::new();

        for row in applied_rows {
            let namespace = MigrationNamespace::of(row.version);
            if !self.owned_namespaces.contains(&namespace) {
                continue;
            }

            let Some(migration) = expected.get(&row.version) else {
                return Err(MigrationLedgerError::UnknownVersion {
                    version: row.version,
                    description: row.description.clone(),
                }
                .into());
            };
            if migration.description != row.description {
                return Err(MigrationLedgerError::DescriptionMismatch {
                    version: row.version,
                    expected: migration.description.clone(),
                    found: row.description.clone(),
                }
                .into());
            }

            let expected_checksum = migration_checksum(migration, driver);
            match &row.checksum {
                Some(found) if found != &expected_checksum => {
                    return Err(MigrationLedgerError::ChecksumMismatch {
                        version: row.version,
                        expected: expected_checksum,
                        found: found.clone(),
                    }
                    .into());
                }
                Some(_) => {}
                None if checksum_column_added => {
                    adoption_backfill.push((row.version, expected_checksum));
                }
                None => {
                    return Err(MigrationLedgerError::MissingChecksum {
                        version: row.version,
                    }
                    .into())
                }
            }
            applied_by_namespace
                .entry(namespace)
                .or_default()
                .push(row.version);
        }

        for migration in &self.migrations {
            let namespace = MigrationNamespace::of(migration.version);
            let Some(applied_versions) = applied_by_namespace.get(&namespace) else {
                continue;
            };
            if applied_versions.contains(&migration.version) {
                continue;
            }
            if let Some(applied_after) = applied_versions
                .iter()
                .copied()
                .find(|version| *version > migration.version)
            {
                return Err(MigrationLedgerError::VersionGap {
                    namespace,
                    missing: migration.version,
                    applied_after,
                }
                .into());
            }
        }

        Ok(adoption_backfill)
    }

    fn owns_version(&self, version: i64) -> bool {
        self.owned_namespaces
            .contains(&MigrationNamespace::of(version))
    }

    /// Get the current schema version.
    #[cfg(test)]
    #[instrument(skip_all, fields(db_name = %db.name()))]
    pub async fn current_version(&self, db: &Database) -> Result<Option<i64>, DatabaseError> {
        let conn = db.guard().await?;
        self.current_version_with_connection(&conn, db.driver())
            .await
    }

    /// Internal method to get current version with a given connection.
    #[cfg(test)]
    async fn current_version_with_connection(
        &self,
        conn: &ConnectionGuard,
        driver: DatabaseDriver,
    ) -> Result<Option<i64>, DatabaseError> {
        conn.execute(migrations_table_sql(driver), ()).await?;
        let mut rows = conn
            .query("SELECT MAX(version) FROM _migrations", ())
            .await?;

        match rows.next().await? {
            Some(row) => Ok(row.get(0)?),
            None => Ok(None),
        }
    }

    /// Check if there are pending migrations.
    #[cfg(test)]
    pub async fn has_pending(&self, db: &Database) -> Result<bool, DatabaseError> {
        let current = self.current_version(db).await?.unwrap_or(0);
        let latest = self
            .migrations
            .last()
            .map(|migration| migration.version)
            .unwrap_or(0);
        Ok(current < latest)
    }
}

struct LedgerRow {
    version: i64,
    description: String,
    checksum: Option<String>,
}
