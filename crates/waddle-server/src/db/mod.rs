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

pub mod blocking;
mod migrations;
mod pool;
pub mod roster;

use libsql::{Connection, Database as LibSqlDatabase};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, info, instrument};

pub use migrations::MigrationRunner;
pub use pool::{DatabasePool, PoolConfig, PoolHealth};

/// Database-specific errors
#[derive(Error, Debug)]
#[allow(dead_code)] // API variants for future use
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
#[derive(Debug, Clone, Default)]
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

impl DatabaseConfig {
    /// Create a development configuration with file-based storage
    #[allow(dead_code)] // API function for production use
    pub fn development(base_path: &str) -> Self {
        Self {
            global_db_path: Some(format!("{}/global.db", base_path)),
            waddle_db_base_path: Some(format!("{}/waddles", base_path)),
            turso_url: None,
            turso_auth_token: None,
        }
    }

    /// Create a production configuration with Turso sync
    #[allow(dead_code)] // API function for production use
    pub fn production(base_path: &str, turso_url: String, turso_auth_token: String) -> Self {
        Self {
            global_db_path: Some(format!("{}/global.db", base_path)),
            waddle_db_base_path: Some(format!("{}/waddles", base_path)),
            turso_url: Some(turso_url),
            turso_auth_token: Some(turso_auth_token),
        }
    }
}

/// Wrapper around a libsql database connection
///
/// For in-memory databases, we store a persistent connection to ensure data
/// persists across multiple `connect()` calls. libSQL's `:memory:` creates
/// a new isolated database for each connection, so we need to reuse the same
/// connection to maintain data.
#[derive(Clone)]
pub struct Database {
    db: Arc<LibSqlDatabase>,
    name: String,
    /// Persistent connection for in-memory databases.
    /// We use a Mutex to allow shared mutable access since Connection is not Clone.
    /// For file-based databases, this is None and we create new connections.
    persistent_conn: Option<Arc<Mutex<Connection>>>,
}

impl Database {
    /// Create a new in-memory database
    ///
    /// For in-memory databases, we store a single persistent connection that is reused
    /// for all operations. This ensures data persists across multiple `connect()` calls,
    /// since libSQL's `:memory:` creates a new isolated database for each connection.
    #[instrument(skip_all)]
    pub async fn in_memory(name: &str) -> Result<Self, DatabaseError> {
        debug!("Creating in-memory database: {}", name);
        let db = libsql::Builder::new_local(":memory:").build().await?;

        // Create a persistent connection for in-memory databases
        let conn = db.connect()?;

        Ok(Self {
            db: Arc::new(db),
            name: name.to_string(),
            persistent_conn: Some(Arc::new(Mutex::new(conn))),
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
                DatabaseError::ConnectionFailed(format!(
                    "Failed to create database directory: {}",
                    e
                ))
            })?;
        }

        let db = libsql::Builder::new_local(path).build().await?;

        // Enable WAL mode and set a busy timeout for concurrent access.
        // Without these, concurrent writers get immediate "database is locked" errors.
        let conn = db.connect()?;
        conn.execute("PRAGMA journal_mode=WAL", ()).await?;
        conn.execute("PRAGMA busy_timeout=5000", ()).await?;

        info!(
            "Opened database '{}' at {:?} (WAL mode, 5s busy timeout)",
            name, path
        );
        Ok(Self {
            db: Arc::new(db),
            name: name.to_string(),
            persistent_conn: None,
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
                DatabaseError::ConnectionFailed(format!(
                    "Failed to create database directory: {}",
                    e
                ))
            })?;
        }

        let db = libsql::Builder::new_remote_replica(
            path,
            turso_url.to_string(),
            auth_token.to_string(),
        )
        .build()
        .await?;

        // Enable WAL mode and set a busy timeout for concurrent access.
        let conn = db.connect()?;
        conn.execute("PRAGMA journal_mode=WAL", ()).await?;
        conn.execute("PRAGMA busy_timeout=5000", ()).await?;

        info!(
            "Opened synced database '{}' with Turso (WAL mode, 5s busy timeout)",
            name
        );
        Ok(Self {
            db: Arc::new(db),
            name: name.to_string(),
            persistent_conn: None,
        })
    }

    /// Get a connection to the database.
    ///
    /// For in-memory databases, this returns a new connection from the same database.
    /// Since we use `:memory:` with libSQL, each connection shares the same in-memory
    /// database as long as it comes from the same `LibSqlDatabase` instance.
    ///
    /// For file-based databases, this creates a new connection to the file.
    pub fn connect(&self) -> Result<Connection, DatabaseError> {
        Ok(self.db.connect()?)
    }

    /// Execute a callback with a connection, using the persistent connection for in-memory databases.
    ///
    /// This method ensures that for in-memory databases, all operations use the same
    /// connection to maintain data consistency.
    #[allow(dead_code)] // API method for future use
    pub async fn with_connection<F, Fut, T>(&self, f: F) -> Result<T, DatabaseError>
    where
        F: FnOnce(&Connection) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        if let Some(ref persistent) = self.persistent_conn {
            let conn = persistent.lock().await;
            Ok(f(&conn).await)
        } else {
            let conn = self.db.connect()?;
            Ok(f(&conn).await)
        }
    }

    /// Get a reference to the persistent connection if this is an in-memory database.
    /// This is useful for operations that need to maintain connection state.
    pub fn persistent_connection(&self) -> Option<&Arc<Mutex<Connection>>> {
        self.persistent_conn.as_ref()
    }

    /// Get the database name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sync the database with Turso (only for remote replica databases)
    #[allow(dead_code)]
    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn sync(&self) -> Result<(), DatabaseError> {
        debug!("Syncing database '{}'", self.name);
        self.db.sync().await?;
        Ok(())
    }

    /// Check if the database is healthy by executing a simple query
    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn health_check(&self) -> Result<bool, DatabaseError> {
        // Use persistent connection for in-memory databases
        if let Some(ref persistent) = self.persistent_conn {
            let conn = persistent.lock().await;
            match conn.query("SELECT 1", ()).await {
                Ok(_) => Ok(true),
                Err(e) => {
                    tracing::warn!("Database health check failed: {}", e);
                    Ok(false)
                }
            }
        } else {
            let conn = self.connect()?;
            match conn.query("SELECT 1", ()).await {
                Ok(_) => Ok(true),
                Err(e) => {
                    tracing::warn!("Database health check failed: {}", e);
                    Ok(false)
                }
            }
        }
    }

    /// Execute a simple query (for testing/health checks)
    #[allow(dead_code)]
    #[instrument(skip_all, fields(name = %self.name))]
    pub async fn execute(&self, sql: &str) -> Result<u64, DatabaseError> {
        // Use persistent connection for in-memory databases
        if let Some(ref persistent) = self.persistent_conn {
            let conn = persistent.lock().await;
            let rows = conn.execute(sql, ()).await?;
            Ok(rows)
        } else {
            let conn = self.connect()?;
            let rows = conn.execute(sql, ()).await?;
            Ok(rows)
        }
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
        db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)")
            .await
            .unwrap();

        // Insert a row
        db.execute("INSERT INTO test (name) VALUES ('hello')")
            .await
            .unwrap();

        // Query should succeed - use persistent connection for in-memory database
        let conn = db.persistent_connection().unwrap();
        let conn = conn.lock().await;
        let mut rows = conn.query("SELECT * FROM test", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let name: String = row.get(1).unwrap();
        assert_eq!(name, "hello");
    }
}
