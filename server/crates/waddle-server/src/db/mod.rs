//! Database module for Waddle Server.
//!
//! Core infrastructure now uses SQLx adapters and a single logical database.

pub mod actor;
mod backend;
pub mod blocking;
mod migrations;
mod pool;
pub mod roster;
mod schema;
mod value;

#[cfg(test)]
use std::path::Path;
use thiserror::Error;
use tracing::{debug, info, instrument};

use backend::{connect_backend, DatabaseBackend};
pub use backend::{ConnectionGuard, DatabaseDriver, Transaction};
pub use migrations::MigrationRunner;
pub use pool::{DatabasePool, PoolConfig, PoolHealth};
pub use schema::{i64_sql_type, widen_postgres_i64_column_to_bigint};
pub use value::{row_value, DbDecode, IntoParams, Row, Rows, Value, ValueExt};

#[macro_export]
macro_rules! db_params {
    () => {
        Vec::<$crate::db::Value>::new()
    };
    ($($value:expr),+ $(,)?) => {
        vec![$($crate::db::Value::from($value)),+]
    };
}

/// Database-specific errors.
#[derive(Error, Debug)]
pub enum DatabaseError {
    #[error("Failed to connect to database: {0}")]
    ConnectionFailed(String),

    #[error("Database query failed: {0}")]
    QueryFailed(String),

    #[error("Migration failed: {0}")]
    MigrationFailed(String),

    #[error("Internal database error: {0}")]
    Internal(#[from] sqlx::Error),
}

/// Configuration for database connections.
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub driver: DatabaseDriver,
    pub database_url: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            driver: DatabaseDriver::Sqlite,
            database_url: "sqlite::memory:".to_string(),
        }
    }
}

impl DatabaseConfig {
    pub fn new(driver: DatabaseDriver, database_url: impl Into<String>) -> Self {
        Self {
            driver,
            database_url: database_url.into(),
        }
    }
}

/// Logical database handle.
#[derive(Clone, kameo::Reply)]
pub struct Database {
    backend: DatabaseBackend,
    name: String,
    driver: DatabaseDriver,
}

impl Database {
    pub async fn in_memory(name: &str) -> Result<Self, DatabaseError> {
        Self::from_config(name, &DatabaseConfig::default()).await
    }

    #[cfg(test)]
    #[instrument(skip_all, fields(path = %path.as_ref().display()))]
    pub async fn open_local(name: &str, path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DatabaseError::ConnectionFailed(format!(
                    "Failed to create database directory: {}",
                    e
                ))
            })?;
        }
        let database_url = format!("sqlite://{}", path.to_string_lossy());
        Self::from_config(
            name,
            &DatabaseConfig::new(DatabaseDriver::Sqlite, database_url),
        )
        .await
    }

    #[instrument(skip_all, fields(name = %name))]
    pub async fn from_config(name: &str, config: &DatabaseConfig) -> Result<Self, DatabaseError> {
        debug!(driver = ?config.driver, "Opening database");
        let backend = connect_backend(config.driver, &config.database_url).await?;

        info!(
            name = %name,
            driver = ?config.driver,
            "Opened database"
        );

        Ok(Self {
            backend,
            name: name.to_string(),
            driver: config.driver,
        })
    }

    pub async fn guard(&self) -> Result<ConnectionGuard, DatabaseError> {
        Ok(ConnectionGuard::new(self.backend.clone()))
    }

    /// Begin a database transaction.
    ///
    /// Multiple statements executed against the returned [`Transaction`]
    /// share a single connection and are committed atomically by
    /// [`Transaction::commit`] (or rolled back on drop). Use this when you
    /// need an invariant — like XEP-0237's "ver identifies the roster
    /// state" — to hold across multiple writes that must commit together
    /// or not at all.
    pub async fn begin(&self) -> Result<Transaction<'_>, DatabaseError> {
        Transaction::begin(&self.backend).await
    }

    /// Begin a transaction that acquires the database write lock
    /// immediately. Use this for transactions that perform writes after
    /// a read, to avoid SQLite deadlocks when two connections both
    /// start as readers and then try to upgrade to writers.
    pub async fn begin_immediate(&self) -> Result<Transaction<'_>, DatabaseError> {
        Transaction::begin_immediate(&self.backend).await
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn driver(&self) -> DatabaseDriver {
        self.driver
    }

    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn health_check(&self) -> Result<bool, DatabaseError> {
        let conn = self.guard().await?;
        match conn.query("SELECT 1", ()).await {
            Ok(_) => Ok(true),
            Err(e) => {
                tracing::warn!(error = %e, "Database health check failed");
                Ok(false)
            }
        }
    }

    #[cfg(test)]
    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn execute(&self, sql: &str) -> Result<u64, DatabaseError> {
        let conn = self.guard().await?;
        conn.execute(sql, ()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_database() {
        let db = Database::in_memory("test").await.unwrap();
        assert_eq!(db.name(), "test");
    }

    #[tokio::test]
    async fn test_health_check() {
        let db = Database::in_memory("test").await.unwrap();
        let healthy = db.health_check().await.unwrap();
        assert!(healthy);
    }

    #[tokio::test]
    async fn test_execute_query() {
        let db = Database::in_memory("test").await.unwrap();

        db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)")
            .await
            .unwrap();
        db.execute("INSERT INTO test (name) VALUES ('hello')")
            .await
            .unwrap();

        let conn = db.guard().await.unwrap();
        let mut rows = conn.query("SELECT * FROM test", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let name: String = row.get(1).unwrap();
        assert_eq!(name, "hello");
    }
}
