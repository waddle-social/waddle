//! Database connection pool for Waddle Server
//!
//! Manages both the global database and per-Waddle databases.
//! Internally uses Kameo actors: each database is owned by a `DbActor`,
//! ensuring sequential access without external Mutex locking.

use super::actor::{DbActor, DbHealthCheck, GetDatabase, GetIdleMs};
use super::{Database, DatabaseConfig, DatabaseError};
use dashmap::DashMap;
use kameo::actor::ActorRef;
use std::path::PathBuf;
use tracing::{debug, info, instrument};

/// Configuration for the database pool
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of per-waddle databases to keep in memory
    pub max_waddle_dbs: usize,
    /// Whether to run migrations on startup
    #[allow(dead_code)]
    pub run_migrations: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_waddle_dbs: 100,
            run_migrations: true,
        }
    }
}

/// Database pool managing global and per-Waddle databases
///
/// The pool provides:
/// - A single global database for users, sessions, waddle registry
/// - Per-Waddle databases for channels and messages
/// - Lazy loading of per-Waddle databases
///
/// Internally each database is owned by a `DbActor`. The `global()` and
/// `get_waddle_db()` methods return `Database` clones for backward
/// compatibility; callers may also obtain the `ActorRef<DbActor>` directly.
pub struct DatabasePool {
    /// Global database actor (always running)
    global_actor: ActorRef<DbActor>,
    /// Cached clone of the global database for cheap access
    global_db: Database,
    /// Per-Waddle database actors (spawned on demand)
    waddle_actors: DashMap<String, ActorRef<DbActor>>,
    /// Database configuration
    config: DatabaseConfig,
    /// Pool configuration
    pool_config: PoolConfig,
}

impl DatabasePool {
    /// Initialize the database pool with the global database
    #[instrument(skip_all)]
    pub async fn new(
        config: DatabaseConfig,
        pool_config: PoolConfig,
    ) -> Result<Self, DatabaseError> {
        info!("Initializing database pool");

        // Create the global database
        let global_db = match (
            &config.global_db_path,
            &config.turso_url,
            &config.turso_auth_token,
        ) {
            // Turso sync enabled
            (Some(path), Some(url), Some(token)) => {
                Database::open_with_sync("global", path, url, token).await?
            }
            // Local file-based
            (Some(path), _, _) => Database::open_local("global", path).await?,
            // In-memory (default for development)
            _ => Database::in_memory("global").await?,
        };

        info!("Global database initialized");

        // Spawn the global actor
        let global_actor = kameo::spawn(DbActor::new(global_db.clone()));

        // Ensure waddle database directory exists if configured
        if let Some(base_path) = &config.waddle_db_base_path {
            std::fs::create_dir_all(base_path).map_err(|e| {
                DatabaseError::ConnectionFailed(format!(
                    "Failed to create waddle database directory: {}",
                    e
                ))
            })?;
        }

        Ok(Self {
            global_actor,
            global_db,
            waddle_actors: DashMap::new(),
            config,
            pool_config,
        })
    }

    /// Get a reference to the global database
    pub fn global(&self) -> &Database {
        &self.global_db
    }

    /// Get the actor ref for the global database
    pub fn global_actor(&self) -> &ActorRef<DbActor> {
        &self.global_actor
    }

    /// Get or create a per-Waddle database
    ///
    /// This lazily loads the database if not already in memory.
    #[instrument(skip_all, fields(waddle_id = %waddle_id))]
    pub async fn get_waddle_db(&self, waddle_id: &str) -> Result<Database, DatabaseError> {
        let actor = self.get_or_spawn_waddle_actor(waddle_id).await?;
        actor
            .ask(GetDatabase)
            .await
            .map_err(|e| DatabaseError::Internal(libsql::Error::ConnectionFailed(e.to_string())))
    }

    /// Get or spawn the actor for a per-Waddle database.
    pub async fn get_waddle_actor(
        &self,
        waddle_id: &str,
    ) -> Result<ActorRef<DbActor>, DatabaseError> {
        self.get_or_spawn_waddle_actor(waddle_id).await
    }

    /// Create a new per-Waddle database
    ///
    /// This is called when a new Waddle is created.
    #[instrument(skip_all, fields(waddle_id = %waddle_id))]
    pub async fn create_waddle_db(&self, waddle_id: &str) -> Result<Database, DatabaseError> {
        // Check if it already exists
        if self.waddle_actors.contains_key(waddle_id) {
            return Err(DatabaseError::AlreadyExists(waddle_id.to_string()));
        }

        // Check if the file already exists on disk
        if let Some(base_path) = &self.config.waddle_db_base_path {
            let db_path = PathBuf::from(base_path).join(format!("{}.db", waddle_id));
            if db_path.exists() {
                return Err(DatabaseError::AlreadyExists(waddle_id.to_string()));
            }
        }

        info!("Creating new waddle database: {}", waddle_id);
        let db = self.open_waddle_db(waddle_id).await?;
        let db_clone = db.clone();

        // Spawn actor and cache it
        let actor = kameo::spawn(DbActor::new(db));
        self.waddle_actors.insert(waddle_id.to_string(), actor);

        Ok(db_clone)
    }

    /// Internal: get an existing actor or spawn a new one for this waddle.
    async fn get_or_spawn_waddle_actor(
        &self,
        waddle_id: &str,
    ) -> Result<ActorRef<DbActor>, DatabaseError> {
        // Check if already loaded
        if let Some(actor) = self.waddle_actors.get(waddle_id) {
            debug!("Returning cached waddle actor: {}", waddle_id);
            return Ok(actor.clone());
        }

        // Load or create the database, spawn an actor
        debug!("Spawning waddle actor: {}", waddle_id);
        let db = self.open_waddle_db(waddle_id).await?;
        let actor = kameo::spawn(DbActor::new(db));

        // Cache it
        self.waddle_actors
            .insert(waddle_id.to_string(), actor.clone());

        // Evict the most idle actor if we're over the limit
        if self.waddle_actors.len() > self.pool_config.max_waddle_dbs {
            self.evict_most_idle(waddle_id).await;
        }

        Ok(actor)
    }

    /// Open a waddle database (create if needed)
    async fn open_waddle_db(&self, waddle_id: &str) -> Result<Database, DatabaseError> {
        let name = format!("waddle_{}", waddle_id);

        match (
            &self.config.waddle_db_base_path,
            &self.config.turso_url,
            &self.config.turso_auth_token,
        ) {
            // Turso sync enabled (url and token will be used in future when implementing Turso sync)
            (Some(base_path), Some(_url), Some(_token)) => {
                let db_path = PathBuf::from(base_path).join(format!("{}.db", waddle_id));
                Database::open_local(&name, db_path).await
            }
            // Local file-based
            (Some(base_path), _, _) => {
                let db_path = PathBuf::from(base_path).join(format!("{}.db", waddle_id));
                Database::open_local(&name, db_path).await
            }
            // In-memory (for testing)
            _ => Database::in_memory(&name).await,
        }
    }

    /// Evict the most idle waddle actor (by idle time), skipping the given id.
    async fn evict_most_idle(&self, skip_id: &str) {
        // Snapshot actor refs to avoid holding DashMap shard locks across .await
        let actors: Vec<(String, ActorRef<DbActor>)> = self
            .waddle_actors
            .iter()
            .filter(|e| e.key() != skip_id)
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();

        let mut most_idle_key: Option<String> = None;
        let mut most_idle_ms: u64 = 0;

        for (key, actor) in actors {
            if let Ok(idle) = actor.ask(GetIdleMs).await {
                if idle > most_idle_ms {
                    most_idle_ms = idle;
                    most_idle_key = Some(key);
                }
            }
        }

        if let Some(key) = most_idle_key {
            self.waddle_actors.remove(&key);
            debug!(
                "Evicted most idle waddle actor from cache: {} (idle {}ms)",
                key, most_idle_ms
            );
        }
    }

    /// Check if a waddle database exists
    #[allow(dead_code)]
    pub fn waddle_db_exists(&self, waddle_id: &str) -> bool {
        if self.waddle_actors.contains_key(waddle_id) {
            return true;
        }

        // Check on disk
        if let Some(base_path) = &self.config.waddle_db_base_path {
            let db_path = PathBuf::from(base_path).join(format!("{}.db", waddle_id));
            return db_path.exists();
        }

        false
    }

    /// Remove a waddle database from the pool (does not delete the file)
    pub fn unload_waddle_db(&self, waddle_id: &str) {
        self.waddle_actors.remove(waddle_id);
        debug!("Unloaded waddle database: {}", waddle_id);
    }

    /// Get the number of currently loaded waddle databases
    #[allow(dead_code)]
    pub fn loaded_waddle_count(&self) -> usize {
        self.waddle_actors.len()
    }

    /// Perform a health check on the pool
    #[instrument(skip_all)]
    pub async fn health_check(&self) -> Result<PoolHealth, DatabaseError> {
        let global_healthy = match self.global_actor.ask(DbHealthCheck).await {
            Ok(healthy) => healthy,
            Err(e) => {
                tracing::warn!("Global DB health check failed: {}", e);
                false
            }
        };

        // Snapshot actor refs to avoid holding DashMap shard locks across .await
        let sample: Vec<ActorRef<DbActor>> = self
            .waddle_actors
            .iter()
            .take(5)
            .map(|e| e.value().clone())
            .collect();

        let mut waddle_healthy = true;
        for actor in sample {
            match actor.ask(DbHealthCheck).await {
                Ok(healthy) if !healthy => {
                    waddle_healthy = false;
                    break;
                }
                Err(e) => {
                    tracing::warn!("Waddle DB health check failed: {}", e);
                    waddle_healthy = false;
                    break;
                }
                _ => {}
            }
        }

        Ok(PoolHealth {
            global_healthy,
            waddle_dbs_healthy: waddle_healthy,
            loaded_waddle_count: self.waddle_actors.len(),
        })
    }

    /// Sync all databases with Turso (if configured)
    #[allow(dead_code)]
    #[instrument(skip_all)]
    pub async fn sync_all(&self) -> Result<(), DatabaseError> {
        if self.config.turso_url.is_some() {
            self.global_db.sync().await?;
            // Snapshot actor refs to avoid holding DashMap shard locks across .await
            let actors: Vec<ActorRef<DbActor>> = self
                .waddle_actors
                .iter()
                .map(|e| e.value().clone())
                .collect();
            for actor in actors {
                if let Ok(db) = actor.ask(GetDatabase).await {
                    db.sync().await?;
                }
            }
        }
        Ok(())
    }
}

/// Health status of the database pool
#[derive(Debug, Clone)]
pub struct PoolHealth {
    /// Whether the global database is healthy
    pub global_healthy: bool,
    /// Whether sampled waddle databases are healthy
    pub waddle_dbs_healthy: bool,
    /// Number of currently loaded waddle databases
    pub loaded_waddle_count: usize,
}

impl PoolHealth {
    /// Returns true if all databases are healthy
    pub fn is_healthy(&self) -> bool {
        self.global_healthy && self.waddle_dbs_healthy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pool_creation_in_memory() {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Global database should be accessible
        let health = pool.health_check().await.unwrap();
        assert!(health.global_healthy);
        assert_eq!(health.loaded_waddle_count, 0);
    }

    #[tokio::test]
    async fn test_waddle_database_creation() {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Create a waddle database
        let db = pool.create_waddle_db("test-waddle-123").await.unwrap();
        assert_eq!(db.name(), "waddle_test-waddle-123");

        // Should be cached
        assert_eq!(pool.loaded_waddle_count(), 1);

        // Getting it again should return the cached version
        let db2 = pool.get_waddle_db("test-waddle-123").await.unwrap();
        assert_eq!(db2.name(), db.name());
    }

    #[tokio::test]
    async fn test_waddle_database_duplicate_error() {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Create a waddle database
        pool.create_waddle_db("test-waddle").await.unwrap();

        // Trying to create it again should fail
        let result = pool.create_waddle_db("test-waddle").await;
        assert!(matches!(result, Err(DatabaseError::AlreadyExists(_))));
    }

    #[tokio::test]
    async fn test_pool_health_check() {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let pool = DatabasePool::new(config, pool_config).await.unwrap();

        pool.create_waddle_db("waddle-1").await.unwrap();
        pool.create_waddle_db("waddle-2").await.unwrap();

        let health = pool.health_check().await.unwrap();
        assert!(health.is_healthy());
        assert!(health.global_healthy);
        assert!(health.waddle_dbs_healthy);
        assert_eq!(health.loaded_waddle_count, 2);
    }
}
