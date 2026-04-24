//! Database pool for a single logical Waddle database.
//!
//! Persistence routes through `DbActor` over one shared SQLx-backed database.

use super::actor::{DbActor, DbHealthCheck};
use super::{Database, DatabaseConfig, DatabaseError};
use kameo::actor::ActorRef;
use tracing::{info, instrument};

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
    async fn test_single_database_accessors() {
        let config = DatabaseConfig::default();
        let pool = DatabasePool::new(config, PoolConfig::default())
            .await
            .unwrap();

        assert_eq!(pool.global().name(), "global");
        assert_eq!(pool.loaded_waddle_count(), 1);
    }
}
