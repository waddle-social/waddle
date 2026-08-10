mod decode;
mod impls;
mod query;
mod schema;
mod write;

#[cfg(any(test, feature = "test-utils"))]
mod test_utils;

use std::str::FromStr;

use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tracing::info;

use super::MamStorageError;
use schema::{
    ensure_postgres_schema, ensure_sqlite_parent_dir, infer_driver, is_in_memory_sqlite,
    MamDatabaseDriver,
};

#[derive(Clone)]
pub(super) enum MamDatabaseBackend {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

/// sqlx-backed MAM storage implementation.
#[derive(Clone)]
pub struct SqlxMamStorage {
    pub(super) backend: MamDatabaseBackend,
    /// ADR-0017 Phase 3 Slice 7 FIX 1 (council-adjudicated): true only when
    /// the caller has verified (via [`Self::with_cluster_fencing`]) that
    /// clustering is enabled AND this storage's own database is co-located
    /// with the clustering global database. [`super::MamStorage::store_message_fenced`]
    /// uses this to decide whether to run the fenced groupchat-archive
    /// insert or fall back to the portable, unfenced [`super::MamStorage::store_message`]
    /// path. `false` for every non-clustered deployment (and, defensively,
    /// for a SQLite backend even if requested — see that method's doc
    /// comment).
    pub(super) fencing_enabled: bool,
}

impl SqlxMamStorage {
    pub async fn open(database_url: &str) -> Result<Self, MamStorageError> {
        let driver = infer_driver(database_url)?;
        let backend = match driver {
            MamDatabaseDriver::Sqlite => {
                ensure_sqlite_parent_dir(database_url)?;
                let options = SqliteConnectOptions::from_str(database_url)
                    .map_err(|error| MamStorageError::Database(error.to_string()))?
                    .create_if_missing(true)
                    .foreign_keys(true)
                    .journal_mode(SqliteJournalMode::Wal);
                let max_connections = if is_in_memory_sqlite(database_url) {
                    1
                } else {
                    10
                };
                let pool = SqlitePoolOptions::new()
                    .max_connections(max_connections)
                    .connect_with(options)
                    .await?;
                MamDatabaseBackend::Sqlite(pool)
            }
            MamDatabaseDriver::Postgres => {
                let pool = PgPoolOptions::new()
                    .max_connections(10)
                    .connect(database_url)
                    .await?;
                MamDatabaseBackend::Postgres(pool)
            }
        };

        let storage = Self {
            backend,
            fencing_enabled: false,
        };
        storage.initialize().await?;
        info!(driver = ?driver, "MAM storage initialized");
        Ok(storage)
    }

    pub async fn open_in_memory() -> Result<Self, MamStorageError> {
        Self::open("sqlite::memory:").await
    }

    /// Expose the underlying SQLite pool when this storage uses the
    /// SQLite backend. This keeps backend attestation in higher layers
    /// typed without coupling this crate to `waddle-server`.
    pub fn sqlite_pool(&self) -> Option<&SqlitePool> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => Some(pool),
            MamDatabaseBackend::Postgres(_) => None,
        }
    }

    /// Expose the underlying Postgres pool when this storage uses the
    /// Postgres backend. This keeps backend attestation in higher layers
    /// typed without coupling this crate to `waddle-server`.
    pub fn postgres_pool(&self) -> Option<&PgPool> {
        match &self.backend {
            MamDatabaseBackend::Postgres(pool) => Some(pool),
            MamDatabaseBackend::Sqlite(_) => None,
        }
    }

    /// Enable cluster fencing (ADR-0017 Phase 3 Slice 7 FIX 1,
    /// council-adjudicated) — a small builder mirroring
    /// `DatabasePendingDeliveryStorage::with_cluster_fencing`'s identical
    /// shape one table over.
    ///
    /// The caller MUST already have verified the co-location invariant
    /// before calling this: this storage's own live PostgreSQL boundary
    /// identity must match the clustering global database identity (the
    /// fencing `SELECT ... FOR SHARE` this storage issues targets
    /// `clustering_claims`, which only exists there). That check lives in
    /// `waddle-server` — see `create_websocket_mam_storage`'s call site.
    ///
    /// A no-op when this storage's backend is not Postgres: SQLite has no
    /// `clustering_claims` table to fence against, and clustering is
    /// Postgres-only per ADR-0017 element 1 — `fencing_enabled` stays
    /// `false` regardless of `enabled` in that case.
    pub fn with_cluster_fencing(mut self, enabled: bool) -> Self {
        self.fencing_enabled = enabled && matches!(self.backend, MamDatabaseBackend::Postgres(_));
        self
    }

    async fn initialize(&self) -> Result<(), MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => schema::ensure_sqlite_schema(pool).await,
            MamDatabaseBackend::Postgres(pool) => ensure_postgres_schema(pool).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlx::postgres::PgPoolOptions;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::{MamDatabaseBackend, SqlxMamStorage};

    #[tokio::test]
    async fn sqlite_backend_exposes_only_sqlite_pool() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("sqlite pool");
        let storage = SqlxMamStorage {
            backend: MamDatabaseBackend::Sqlite(pool.clone()),
            fencing_enabled: false,
        };

        assert!(storage.sqlite_pool().is_some());
        assert!(storage.postgres_pool().is_none());
        assert!(!storage.sqlite_pool().expect("sqlite pool").is_closed());
    }

    #[tokio::test]
    async fn postgres_backend_exposes_only_postgres_pool() {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgresql://postgres@localhost/waddle")
            .expect("postgres pool");
        let storage = SqlxMamStorage {
            backend: MamDatabaseBackend::Postgres(pool.clone()),
            fencing_enabled: false,
        };

        assert!(storage.postgres_pool().is_some());
        assert!(storage.sqlite_pool().is_none());
        assert!(!storage.postgres_pool().expect("postgres pool").is_closed());
    }
}
