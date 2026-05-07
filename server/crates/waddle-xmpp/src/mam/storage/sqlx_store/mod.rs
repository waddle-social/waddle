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

        let storage = Self { backend };
        storage.initialize().await?;
        info!(driver = ?driver, "MAM storage initialized");
        Ok(storage)
    }

    pub async fn open_in_memory() -> Result<Self, MamStorageError> {
        Self::open("sqlite::memory:").await
    }

    async fn initialize(&self) -> Result<(), MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => schema::ensure_sqlite_schema(pool).await,
            MamDatabaseBackend::Postgres(pool) => ensure_postgres_schema(pool).await,
        }
    }
}
