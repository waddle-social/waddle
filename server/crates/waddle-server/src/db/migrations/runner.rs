use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tracing::{debug, info, instrument};

use super::baseline;
use super::postgres_ledger::{self, PreparedLedger};
use super::sql::migrations_table_sql;
use super::{migration_checksum, Migration, MigrationLedgerError};
use crate::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
#[cfg(test)]
use crate::db::ConnectionGuard;
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

/// Dedicated, cluster-wide Postgres advisory-lock key for serializing the
/// append-only migration ledger. This differs from the claims lock key in
/// `clustering::claims`; SQLite is single-node, where `BEGIN IMMEDIATE`
/// provides the corresponding write serialization.
pub(crate) const MIGRATION_LEDGER_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_992;

const MIGRATION_LOCK_WAIT: Duration = Duration::from_secs(15);
const MIGRATION_LOCK_RETRY: Duration = Duration::from_millis(20);
const MIGRATION_LOCK_ROLLBACK_WAIT: Duration = Duration::from_secs(5);

/// Table the schema baseline creates first among those it shares with no
/// bootstrap. Finding it without a ledger means a schema the runner did not
/// record, which it refuses to build over.
pub(super) const SENTINEL_TABLE: &str = "users";

/// Migration runner for applying migrations to a database.
pub struct MigrationRunner {
    pub(super) migrations: Vec<Migration>,
    /// True on the deployment catalog, whose v1 PostgreSQL SQL is a schema
    /// template rendered with the active schema name on application.
    renders_postgres_template: bool,
}

impl MigrationRunner {
    /// Create a runner for exactly the given migrations. It owns every
    /// version in the ledger.
    pub fn new(migrations: Vec<Migration>) -> Self {
        let mut sorted = migrations;
        sorted.sort_by_key(|migration| migration.version);
        Self {
            migrations: sorted,
            renders_postgres_template: false,
        }
    }

    /// The deployment catalog, under the name tests have long used for it.
    #[cfg(test)]
    pub fn global() -> Self {
        Self::single()
    }

    /// The deployment catalog: the single initial schema (`version: 1`). Every
    /// ledger reader in a process builds its runner with this, so the runner
    /// that applies, the owner preparations that verify and the runtime
    /// preflight that pins agree.
    pub fn single() -> Self {
        let mut runner = Self::new(vec![baseline::migration()]);
        runner.renders_postgres_template = true;
        runner
    }

    /// The highest version in this runner's catalog: the version a database
    /// that has applied every migration records last.
    pub fn latest_version(&self) -> Option<i64> {
        self.migrations
            .iter()
            .map(|migration| migration.version)
            .max()
    }

    /// Run all pending migrations on one transaction. PostgreSQL acquires the
    /// cooperative catalog fence before the migration-ledger lock, and holds
    /// both through commit or rollback. Each contended attempt ends its
    /// transaction before retrying, so a ledger holder does not keep readers
    /// behind the catalog fence indefinitely. SQLite uses its immediate lock.
    /// Other privileged catalog mutators and any pool hooks that run before
    /// checkout must independently participate in the same fence or be
    /// quiesced; this does not establish global closure.
    /// Cancellation or a lost database response cannot prove that the original
    /// backend, its lock, or a possibly dispatched commit has finished. The
    /// operator must fence and reconcile that backend and any uncertain commit.
    /// This API does not return a durable backend identity, so that identity
    /// must be tracked independently, including before acquisition completes.
    #[instrument(skip_all, fields(db_name = %db.name()))]
    pub async fn run(&self, db: &Database) -> Result<Vec<i64>, DatabaseError> {
        let driver = db.driver();
        let mut tx = match driver {
            DatabaseDriver::Postgres => begin_fenced_postgres(db).await?,
            DatabaseDriver::Sqlite => db.begin_immediate().await?,
        };

        let result = match driver {
            DatabaseDriver::Postgres => {
                async {
                    let ledger = postgres_ledger::prepare(&mut tx).await?;
                    self.apply_pending_to_ledger(
                        &mut tx,
                        driver,
                        &ledger.table,
                        ledger.checksum_column_added,
                        Some(&ledger),
                    )
                    .await
                }
                .await
            }
            DatabaseDriver::Sqlite => self.apply_pending_in_transaction(&mut tx, driver).await,
        };
        let newly_applied = match result {
            Ok(applied) => applied,
            Err(error) if driver == DatabaseDriver::Postgres => {
                // Do not report a preflight or application failure until the
                // transaction's rollback is acknowledged. On an uncertain
                // end, discard the checkout and surface that uncertainty.
                rollback_migration_attempt(tx).await?;
                return Err(error);
            }
            Err(error) => {
                // A dropped SQLite transaction rolls back only when its
                // connection is returned, so a caller could otherwise read
                // the schema while the refused attempt still holds it.
                tx.rollback().await?;
                return Err(error);
            }
        };

        tx.commit().await?;

        if newly_applied.is_empty() {
            debug!("No new migrations to apply");
        } else {
            info!(count = newly_applied.len(), "Applied new migrations");
        }

        Ok(newly_applied)
    }

    /// Apply the catalog in the caller's transaction without acquiring locks or
    /// committing. The caller must establish the required driver-specific
    /// serialization and search path before entering this method.
    pub(super) async fn apply_pending_in_transaction(
        &self,
        tx: &mut Transaction<'_>,
        driver: DatabaseDriver,
    ) -> Result<Vec<i64>, DatabaseError> {
        let checksum_column_added = self.bootstrap(tx, driver).await?;
        self.apply_pending_to_ledger(tx, driver, "_migrations", checksum_column_added, None)
            .await
    }

    async fn apply_pending_to_ledger(
        &self,
        tx: &mut Transaction<'_>,
        driver: DatabaseDriver,
        ledger_table: &str,
        checksum_column_added: bool,
        prepared: Option<&PreparedLedger>,
    ) -> Result<Vec<i64>, DatabaseError> {
        let applied_rows = self.read_ledger_from(tx, ledger_table).await?;
        self.validate_schema_has_ledger(tx, driver, &applied_rows)
            .await?;
        let adoption_backfill =
            self.validate_ledger(&applied_rows, driver, checksum_column_added)?;

        for (version, checksum) in adoption_backfill {
            tx.execute(
                &format!(
                    "UPDATE {ledger_table} SET checksum = ? WHERE version = ? AND checksum IS NULL"
                ),
                (checksum, version),
            )
            .await?;
        }

        let applied: HashSet<i64> = applied_rows.iter().map(|row| row.version).collect();
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
            if let Some(ledger) = prepared {
                // A previous migration body may have changed its local path.
                ledger.set_search_path(tx).await?;
            }
            let apply_result = if self.renders_postgres_template
                && migration.version == baseline::INITIAL_SCHEMA_VERSION
            {
                baseline::apply(tx, driver, migration).await
            } else {
                tx.execute_batch(migration.sql_for(driver)).await
            };
            apply_result.map_err(|source| DatabaseError::MigrationApply {
                version: migration.version,
                description: migration.description.clone(),
                source: Box::new(source),
            })?;
            tx.execute(
                &format!(
                    "INSERT INTO {ledger_table} (version, description, checksum) VALUES (?, ?, ?)"
                ),
                crate::db_params![
                    migration.version,
                    migration.description.as_str(),
                    migration_checksum(migration, driver)
                ],
            )
            .await
            .map_err(|source| DatabaseError::MigrationApply {
                version: migration.version,
                description: migration.description.clone(),
                source: Box::new(source),
            })?;

            newly_applied.push(migration.version);
            info!(version = migration.version, "Applied migration");
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

    async fn read_ledger_from(
        &self,
        tx: &mut Transaction<'_>,
        ledger_table: &str,
    ) -> Result<Vec<LedgerRow>, DatabaseError> {
        let sql =
            format!("SELECT version, description, checksum FROM {ledger_table} ORDER BY version");
        let mut rows = tx.query(&sql, ()).await?;
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

    /// Verify an already committed ledger without bootstrap DDL or checksum
    /// backfill. The caller owns a read-only PostgreSQL transaction so this
    /// cannot silently become an owner operation as the catalog evolves.
    pub(crate) async fn verify_complete_in_transaction(
        &self,
        tx: &mut Transaction<'_>,
        driver: DatabaseDriver,
        ledger_table: &str,
    ) -> Result<(), DatabaseError> {
        let applied_rows = self.read_ledger_from(tx, ledger_table).await?;
        self.validate_schema_has_ledger(tx, driver, &applied_rows)
            .await?;
        self.verify_complete_rows(&applied_rows, driver)
    }

    /// Verify the same catalog on a dedicated PostgreSQL owner session. A
    /// complete ledger is not empty, so the bootstrap-only sentinel check
    /// cannot authorize additional work here.
    pub(super) async fn verify_complete_on_postgres_connection(
        &self,
        connection: &mut sqlx::PgConnection,
        ledger_table: &str,
    ) -> Result<(), DatabaseError> {
        let sql =
            format!("SELECT version, description, checksum FROM {ledger_table} ORDER BY version");
        let rows: Vec<(i64, String, Option<String>)> =
            sqlx::query_as(&sql).fetch_all(connection).await?;
        let applied_rows = rows
            .into_iter()
            .map(|(version, description, checksum)| LedgerRow {
                version,
                description,
                checksum,
            })
            .collect::<Vec<_>>();
        self.verify_complete_rows(&applied_rows, DatabaseDriver::Postgres)
    }

    fn verify_complete_rows(
        &self,
        applied_rows: &[LedgerRow],
        driver: DatabaseDriver,
    ) -> Result<(), DatabaseError> {
        self.validate_ledger(applied_rows, driver, false)?;

        let mut seen = HashSet::new();
        for row in applied_rows {
            if !seen.insert(row.version) {
                return Err(MigrationLedgerError::DuplicateVersion {
                    version: row.version,
                }
                .into());
            }
        }
        for migration in &self.migrations {
            if !seen.contains(&migration.version) {
                return Err(MigrationLedgerError::PendingMigration {
                    version: migration.version,
                }
                .into());
            }
        }
        Ok(())
    }

    /// Verify the committed ledger prefix without applying pending migrations.
    /// No checksum repair or schema DDL is permitted by this check.
    pub(super) async fn verify_applied_through_in_transaction(
        &self,
        tx: &mut Transaction<'_>,
        driver: DatabaseDriver,
        ledger_table: &str,
        required_version: i64,
    ) -> Result<(), DatabaseError> {
        let applied_rows = self.read_ledger_from(tx, ledger_table).await?;
        self.validate_schema_has_ledger(tx, driver, &applied_rows)
            .await?;
        self.verify_applied_through_rows(&applied_rows, driver, required_version)
    }

    /// Read only the already-pinned, locked PostgreSQL heap. The caller must
    /// establish its catalog shape and safe search path before calling this.
    pub(super) async fn verify_applied_through_locked_postgres(
        &self,
        connection: &mut sqlx::PgConnection,
        ledger_table: &str,
        required_version: i64,
    ) -> Result<(), DatabaseError> {
        // Bound row count and raw TEXT bytes in separate statements before
        // substring projection. PostgreSQL can fetch an entire compressed
        // TOAST value even for a short substring; octet_length(text) reads
        // its raw size without detoasting. The existing locked/quiescent
        // ledger boundary makes these checks apply to the later read.
        let max_description = self
            .migrations
            .iter()
            .map(|migration| migration.description.chars().count())
            .max()
            .unwrap_or(0);
        let description_limit = i32::try_from(max_description.saturating_add(1))
            .map_err(|_| MigrationLedgerError::OversizedLedger)?;
        let row_limit = i64::try_from(self.migrations.len().saturating_add(1))
            .map_err(|_| MigrationLedgerError::OversizedLedger)?;
        let max_description_bytes = self
            .migrations
            .iter()
            .map(|migration| migration.description.len())
            .max()
            .unwrap_or(0);
        let byte_limit = i32::try_from(max_description_bytes)
            .map_err(|_| MigrationLedgerError::OversizedLedger)?;
        let counted: i64 = sqlx::query_scalar(&format!(
            "SELECT pg_catalog.count(*) FROM (SELECT 1 FROM ONLY {ledger_table} LIMIT $1) bounded"
        ))
        .bind(row_limit)
        .fetch_one(&mut *connection)
        .await?;
        if counted
            > i64::try_from(self.migrations.len())
                .map_err(|_| MigrationLedgerError::OversizedLedger)?
        {
            return Err(MigrationLedgerError::OversizedLedger.into());
        }
        let bounded: bool = sqlx::query_scalar(&format!(
            "SELECT NOT EXISTS (SELECT 1 FROM ONLY {ledger_table} \
             WHERE pg_catalog.octet_length(description) > $1 \
                OR (checksum IS NOT NULL AND pg_catalog.octet_length(checksum) > 64))"
        ))
        .bind(byte_limit)
        .fetch_one(&mut *connection)
        .await?;
        if !bounded {
            return Err(MigrationLedgerError::OversizedLedger.into());
        }
        // One extra row/character still catches any malformed prefix before
        // SQLx decodes it. No ORDER BY needs a text ordering operator.
        let sql = format!(
            "SELECT version, pg_catalog.substr(description, 1, $1), \
             pg_catalog.substr(checksum, 1, 65) FROM ONLY {ledger_table} LIMIT $2"
        );
        let rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(&sql)
            .bind(description_limit)
            .bind(row_limit)
            .fetch_all(connection)
            .await?;
        if rows.len() > self.migrations.len()
            || rows.iter().any(|(_, description, checksum)| {
                description.chars().count() > max_description
                    || checksum.as_ref().is_some_and(|value| value.len() > 64)
            })
        {
            return Err(MigrationLedgerError::OversizedLedger.into());
        }
        let applied_rows = rows
            .into_iter()
            .map(|(version, description, checksum)| LedgerRow {
                version,
                description,
                checksum,
            })
            .collect::<Vec<_>>();
        self.verify_applied_through_rows(
            &applied_rows,
            DatabaseDriver::Postgres,
            required_version,
        )?;

        // The bootstrap sentinel query is deliberately unavailable here. A
        // committed prefix must instead contain every catalog version
        // through the requested one.
        for migration in self
            .migrations
            .iter()
            .filter(|m| m.version <= required_version)
        {
            if !applied_rows
                .iter()
                .any(|row| row.version == migration.version)
            {
                return Err(MigrationLedgerError::PendingMigration {
                    version: migration.version,
                }
                .into());
            }
        }
        Ok(())
    }

    fn verify_applied_through_rows(
        &self,
        applied_rows: &[LedgerRow],
        driver: DatabaseDriver,
        required_version: i64,
    ) -> Result<(), DatabaseError> {
        self.validate_ledger(applied_rows, driver, false)?;
        let mut seen = HashSet::new();
        for row in applied_rows {
            if !seen.insert(row.version) {
                return Err(MigrationLedgerError::DuplicateVersion {
                    version: row.version,
                }
                .into());
            }
        }
        if !seen.contains(&required_version) {
            return Err(MigrationLedgerError::PendingMigration {
                version: required_version,
            }
            .into());
        }
        Ok(())
    }

    /// Refuse to build the baseline over a schema that exists without any
    /// ledger row: the runner did not record it, and plain `CREATE` would
    /// either fail part-way or build next to tables it does not know.
    async fn validate_schema_has_ledger(
        &self,
        tx: &mut Transaction<'_>,
        driver: DatabaseDriver,
        applied_rows: &[LedgerRow],
    ) -> Result<(), DatabaseError> {
        if !applied_rows.is_empty() {
            return Ok(());
        }
        let mut rows = match driver {
            DatabaseDriver::Sqlite => {
                tx.query(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
                    crate::db_params![SENTINEL_TABLE],
                )
                .await?
            }
            DatabaseDriver::Postgres => {
                tx.query(
                    "SELECT table_name FROM information_schema.tables \
                     WHERE table_schema = current_schema() AND table_name = ?",
                    crate::db_params![SENTINEL_TABLE],
                )
                .await?
            }
        };
        if rows.next().await?.is_some() {
            return Err(MigrationLedgerError::SchemaWithoutLedger {
                table: SENTINEL_TABLE.to_string(),
            }
            .into());
        }
        Ok(())
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
        let mut applied_versions = Vec::new();
        let mut adoption_backfill = Vec::new();

        for row in applied_rows {
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
            applied_versions.push(row.version);
        }

        for migration in &self.migrations {
            if applied_versions.contains(&migration.version) {
                continue;
            }
            if let Some(applied_after) = applied_versions
                .iter()
                .copied()
                .find(|version| *version > migration.version)
            {
                return Err(MigrationLedgerError::VersionGap {
                    missing: migration.version,
                    applied_after,
                }
                .into());
            }
        }

        Ok(adoption_backfill)
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

/// Retry both locks in fresh transactions, with the catalog fence first. A
/// failed ledger try must release the catalog fence before retrying. Holding a
/// snapshot while waiting can prevent concurrent index maintenance finishing.
async fn begin_fenced_postgres(db: &Database) -> Result<Transaction<'_>, DatabaseError> {
    let deadline = tokio::time::Instant::now() + MIGRATION_LOCK_WAIT;
    // The last conflict an attempt observed. Once one is seen, running out of
    // time reports it: the deadline can land inside a later attempt's
    // statement, and that is still contention, not an unexplained timeout.
    let mut observed: Option<LockConflict> = None;
    loop {
        let mut tx = tokio::time::timeout_at(deadline, db.begin_discard_postgres_on_drop())
            .await
            .map_err(|_| timed_out(observed))??;
        // This utility statement takes no data snapshot. READ COMMITTED makes
        // the post-ledger-lock read observe a competing runner's commit.
        let isolation = tokio::time::timeout_at(
            deadline,
            tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ()),
        )
        .await;
        match isolation {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                rollback_migration_attempt(tx).await?;
                return Err(error);
            }
            Err(_) => {
                rollback_migration_attempt(tx).await?;
                return Err(timed_out(observed));
            }
        }
        let mut contended = None;
        for (key, conflict) in [
            (
                CATALOG_MUTATION_ADVISORY_LOCK_KEY,
                LockConflict::CatalogFence,
            ),
            (MIGRATION_LEDGER_ADVISORY_LOCK_KEY, LockConflict::Ledger),
        ] {
            let connection = tx.postgres_connection().ok_or_else(|| {
                DatabaseError::QueryFailed("migration locks require PostgreSQL".to_owned())
            })?;
            let acquired = tokio::time::timeout_at(
                deadline,
                sqlx::query_scalar::<_, bool>("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                    .bind(key)
                    .fetch_one(connection),
            )
            .await;
            match acquired {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => {
                    contended = Some(conflict);
                    break;
                }
                Ok(Err(error)) => {
                    rollback_migration_attempt(tx).await?;
                    return Err(error.into());
                }
                Err(_) => {
                    rollback_migration_attempt(tx).await?;
                    return Err(timed_out(observed));
                }
            }
        }
        match contended {
            None if tokio::time::Instant::now() < deadline => return Ok(tx),
            None => {
                rollback_migration_attempt(tx).await?;
                return Err(DatabaseError::MigrationLockTimeout);
            }
            Some(conflict) => {
                observed = Some(conflict);
                rollback_migration_attempt(tx).await?;
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(conflict.into());
                }
                tokio::time::sleep(MIGRATION_LOCK_RETRY.min(remaining)).await;
                if tokio::time::Instant::now() >= deadline {
                    return Err(conflict.into());
                }
            }
        }
    }
}

/// Which migration lock an acquisition attempt found held.
#[derive(Clone, Copy)]
enum LockConflict {
    CatalogFence,
    Ledger,
}

impl From<LockConflict> for DatabaseError {
    fn from(conflict: LockConflict) -> Self {
        match conflict {
            LockConflict::CatalogFence => Self::MigrationCatalogFenceContended,
            LockConflict::Ledger => Self::MigrationLedgerLockContended,
        }
    }
}

/// The deadline's error: the observed conflict, or a plain timeout when no
/// attempt got far enough to see one.
fn timed_out(observed: Option<LockConflict>) -> DatabaseError {
    observed.map_or(DatabaseError::MigrationLockTimeout, Into::into)
}

async fn rollback_migration_attempt(tx: Transaction<'_>) -> Result<(), DatabaseError> {
    if !tx
        .rollback_bounded_discard(MIGRATION_LOCK_ROLLBACK_WAIT)
        .await
    {
        return Err(DatabaseError::MigrationLockRollbackUnconfirmed);
    }
    Ok(())
}

struct LedgerRow {
    version: i64,
    description: String,
    checksum: Option<String>,
}

#[cfg(test)]
mod lock_conflict_tests {
    use super::{timed_out, DatabaseError, LockConflict};

    #[test]
    fn a_deadline_after_an_observed_conflict_reports_that_conflict() {
        assert!(matches!(
            timed_out(Some(LockConflict::Ledger)),
            DatabaseError::MigrationLedgerLockContended
        ));
        assert!(matches!(
            timed_out(Some(LockConflict::CatalogFence)),
            DatabaseError::MigrationCatalogFenceContended
        ));
        assert!(matches!(
            timed_out(None),
            DatabaseError::MigrationLockTimeout
        ));
    }
}
