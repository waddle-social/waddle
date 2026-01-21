//! Database module for Waddle Server
//!
//! This module provides a Turso/libSQL database layer with:
//! - Connection pooling for global and per-Waddle databases
//! - Automatic schema migrations
//! - Health check capabilities
//!
//! # Architecture
//!
//! Waddle uses a multi-tenant database architecture:
//! - **Global Database**: Stores users, waddle registry, sessions
//! - **Per-Waddle Databases**: Each Waddle (community) has its own database for channels, messages
//!
//! This separation allows for:
//! - Data isolation between communities
//! - Independent scaling per waddle
//! - Easy data export/backup per community

mod migrations;
mod pool;

use libsql::{Connection, Database as LibSqlDatabase};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, info, instrument};

pub use migrations::{Migration, MigrationRunner};
pub use pool::{DatabasePool, PoolConfig, PoolHealth};

/// Database-specific errors
#[derive(Error, Debug)]
pub enum DatabaseError {
    #[error("Failed to connect to database: {0}")]
    ConnectionFailed(String),

    #[error("Database query failed: {0}")]
    QueryFailed(String),

    #[error("Migration failed: {0}")]
    MigrationFailed(String),

    #[error("Database not found: {0}")]
    NotFound(String),

    #[error("Database already exists: {0}")]
    AlreadyExists(String),

    #[error("Internal database error: {0}")]
    Internal(#[from] libsql::Error),
}

/// Configuration for database connections
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// Path to the global database file (None for in-memory)
    pub global_db_path: Option<String>,
    /// Base path for per-waddle databases
    pub waddle_db_base_path: Option<String>,
    /// Optional Turso URL for remote database sync
    pub turso_url: Option<String>,
    /// Optional Turso auth token
    pub turso_auth_token: Option<String>,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            global_db_path: None, // In-memory by default
            waddle_db_base_path: None,
            turso_url: None,
            turso_auth_token: None,
        }
    }
}

impl DatabaseConfig {
    /// Create a development configuration with file-based storage
    pub fn development(base_path: &str) -> Self {
        Self {
            global_db_path: Some(format!("{}/global.db", base_path)),
            waddle_db_base_path: Some(format!("{}/waddles", base_path)),
            turso_url: None,
            turso_auth_token: None,
        }
    }

    /// Create a production configuration with Turso sync
    pub fn production(
        base_path: &str,
        turso_url: String,
        turso_auth_token: String,
    ) -> Self {
        Self {
            global_db_path: Some(format!("{}/global.db", base_path)),
            waddle_db_base_path: Some(format!("{}/waddles", base_path)),
            turso_url: Some(turso_url),
            turso_auth_token: Some(turso_auth_token),
        }
    }
}

/// Wrapper around a libsql database connection
#[derive(Clone)]
pub struct Database {
    db: Arc<LibSqlDatabase>,
    name: String,
}

impl Database {
    /// Create a new in-memory database
    #[instrument(skip_all)]
    pub async fn in_memory(name: &str) -> Result<Self, DatabaseError> {
        debug!("Creating in-memory database: {}", name);
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await?;

        Ok(Self {
            db: Arc::new(db),
            name: name.to_string(),
        })
    }

    /// Create or open a local file-based database
    #[instrument(skip_all, fields(path = %path.as_ref().display()))]
    pub async fn open_local(name: &str, path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let path = path.as_ref();
        debug!("Opening local database '{}' at: {:?}", name, path);

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DatabaseError::ConnectionFailed(format!("Failed to create database directory: {}", e))
            })?;
        }

        let db = libsql::Builder::new_local(path)
            .build()
            .await?;

        info!("Opened database '{}' at {:?}", name, path);
        Ok(Self {
            db: Arc::new(db),
            name: name.to_string(),
        })
    }

    /// Create a database with Turso sync (embedded replica)
    #[instrument(skip_all, fields(name = %name))]
    pub async fn open_with_sync(
        name: &str,
        local_path: impl AsRef<Path>,
        turso_url: &str,
        auth_token: &str,
    ) -> Result<Self, DatabaseError> {
        let path = local_path.as_ref();
        debug!("Opening synced database '{}' with Turso", name);

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DatabaseError::ConnectionFailed(format!("Failed to create database directory: {}", e))
            })?;
        }

        let db = libsql::Builder::new_remote_replica(
            path,
            turso_url.to_string(),
            auth_token.to_string(),
        )
        .build()
        .await?;

        info!("Opened synced database '{}' with Turso", name);
        Ok(Self {
            db: Arc::new(db),
            name: name.to_string(),
        })
    }

    /// Get a connection to the database
    pub fn connect(&self) -> Result<Connection, DatabaseError> {
        Ok(self.db.connect()?)
    }

    /// Get the database name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sync the database with Turso (only for remote replica databases)
    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn sync(&self) -> Result<(), DatabaseError> {
        debug!("Syncing database '{}'", self.name);
        self.db.sync().await?;
        Ok(())
    }

    /// Check if the database is healthy by executing a simple query
    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn health_check(&self) -> Result<bool, DatabaseError> {
        let conn = self.connect()?;
        match conn.query("SELECT 1", ()).await {
            Ok(_) => Ok(true),
            Err(e) => {
                tracing::warn!("Database health check failed: {}", e);
                Ok(false)
            }
        }
    }

    /// Execute a simple query (for testing/health checks)
    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn execute(&self, sql: &str) -> Result<u64, DatabaseError> {
        let conn = self.connect()?;
        let rows = conn.execute(sql, ()).await?;
        Ok(rows)
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

        // Create a test table
        db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)").await.unwrap();

        // Insert a row
        db.execute("INSERT INTO test (name) VALUES ('hello')").await.unwrap();

        // Query should succeed
        let conn = db.connect().unwrap();
        let mut rows = conn.query("SELECT * FROM test", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let name: String = row.get(1).unwrap();
        assert_eq!(name, "hello");
    }
}
