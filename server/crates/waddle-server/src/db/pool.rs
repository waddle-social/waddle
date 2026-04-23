//! Database pool for a single logical Waddle database.
//!
//! Persistence routes through `DbActor` over one shared SQLx-backed database.

use super::actor::{DbActor, DbHealthCheck};
use super::{Database, DatabaseConfig, DatabaseError};
use kameo::actor::ActorRef;
use tracing::{debug, info, instrument};

/// Configuration for the database pool.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Whether to run migrations on startup.
    #[allow(dead_code)]
    pub run_migrations: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            run_migrations: true,
        }
    }
}

/// Database pool managing a single logical database.
pub struct DatabasePool {
    global_actor: ActorRef<DbActor>,
    global_db: Database,
}

impl DatabasePool {
    #[instrument(skip_all)]
    pub async fn new(
        config: DatabaseConfig,
        _pool_config: PoolConfig,
    ) -> Result<Self, DatabaseError> {
        info!(driver = ?config.driver, "Initializing database pool");

        let global_db = Database::from_config("global", &config).await?;
        let global_actor = kameo::spawn(DbActor::new(global_db.clone()));

        Ok(Self {
            global_actor,
            global_db,
        })
    }

    pub fn global(&self) -> &Database {
        &self.global_db
    }

    pub fn global_actor(&self) -> &ActorRef<DbActor> {
        &self.global_actor
    }

    /// Single-database model: waddle DB requests map to the global DB.
    #[instrument(skip_all, fields(waddle_id = %waddle_id))]
    pub async fn get_waddle_db(&self, waddle_id: &str) -> Result<Database, DatabaseError> {
        debug!(waddle_id = %waddle_id, "Single database mode: returning global database handle");
        Ok(self.global_db.clone())
    }

    /// Single-database model: waddle actor requests map to the global actor.
    pub async fn get_waddle_actor(
        &self,
        waddle_id: &str,
    ) -> Result<ActorRef<DbActor>, DatabaseError> {
        debug!(waddle_id = %waddle_id, "Single database mode: returning global actor");
        Ok(self.global_actor.clone())
    }

    /// Single-database model: creation is a no-op returning global handle.
    #[instrument(skip_all, fields(waddle_id = %waddle_id))]
    pub async fn create_waddle_db(&self, waddle_id: &str) -> Result<Database, DatabaseError> {
        debug!(waddle_id = %waddle_id, "Single database mode: create_waddle_db is a no-op");
        Ok(self.global_db.clone())
    }

    #[allow(dead_code)]
    pub fn waddle_db_exists(&self, _waddle_id: &str) -> bool {
        true
    }

    pub fn unload_waddle_db(&self, _waddle_id: &str) {
        debug!("Single database mode: unload_waddle_db is a no-op");
    }

    #[allow(dead_code)]
    pub fn loaded_waddle_count(&self) -> usize {
        1
    }

    #[instrument(skip_all)]
    pub async fn health_check(&self) -> Result<PoolHealth, DatabaseError> {
        let global_healthy = match self.global_actor.ask(DbHealthCheck).await {
            Ok(healthy) => healthy,
            Err(e) => {
                tracing::warn!(error = %e, "Global DB health check failed");
                false
            }
        };

        Ok(PoolHealth {
            global_healthy,
            waddle_dbs_healthy: global_healthy,
            loaded_waddle_count: 1,
        })
    }

    #[allow(dead_code)]
    pub async fn sync_all(&self) -> Result<(), DatabaseError> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct PoolHealth {
    pub global_healthy: bool,
    pub waddle_dbs_healthy: bool,
    pub loaded_waddle_count: usize,
}

impl PoolHealth {
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

        let health = pool.health_check().await.unwrap();
        assert!(health.global_healthy);
        assert_eq!(health.loaded_waddle_count, 1);
    }

    #[tokio::test]
    async fn test_single_database_aliases() {
        let config = DatabaseConfig::default();
        let pool = DatabasePool::new(config, PoolConfig::default())
            .await
            .unwrap();

        let db = pool.create_waddle_db("test-waddle").await.unwrap();
        let db2 = pool.get_waddle_db("test-waddle").await.unwrap();

        assert_eq!(db.name(), "global");
        assert_eq!(db2.name(), "global");
        assert_eq!(pool.loaded_waddle_count(), 1);
    }
}
