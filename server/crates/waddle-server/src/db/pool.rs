//! Database pool for a single logical Waddle database.
//!
//! Persistence routes through `DbActor` over one shared SQLx-backed database.

use super::actor::{DbActor, DbHealthCheck};
use super::{Database, DatabaseConfig, DatabaseError};
use kameo::actor::{ActorRef, Spawn};
use tracing::{info, instrument};

/// Configuration for the database pool.
#[derive(Debug, Clone, Default)]
pub struct PoolConfig;

/// Database pool managing a single logical database.
pub struct DatabasePool {
    global_actor: ActorRef<DbActor>,
    global_db: Database,
}

impl DatabasePool {
    #[instrument(skip_all, err)]
    pub async fn new(
        config: DatabaseConfig,
        _pool_config: PoolConfig,
    ) -> Result<Self, DatabaseError> {
        info!(driver = ?config.driver, "Initializing database pool");

        let global_db = Database::from_config("global", &config).await?;
        let global_actor = DbActor::spawn(DbActor::new(global_db.clone()));

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

    #[cfg(test)]
    pub fn loaded_waddle_count(&self) -> usize {
        1
    }

    #[instrument(skip_all)]
    pub async fn health_check(&self) -> Result<PoolHealth, DatabaseError> {
        let global_healthy = match self.global_actor.ask(DbHealthCheck).await {
            Ok(healthy) => healthy,
            Err(e) => {
                crate::telemetry::mark_span_error("global database health check failed");
                tracing::warn!(error = %e, "Global DB health check failed");
                // Query-level failures count inside
                // `Database::health_check`; this branch is the one case
                // that never reaches it (actor unreachable).
                super::Database::record_health_check_failure();
                false
            }
        };

        Ok(PoolHealth {
            global_healthy,
            waddle_dbs_healthy: global_healthy,
            loaded_waddle_count: 1,
        })
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
        let pool_config = PoolConfig;
        let pool = DatabasePool::new(config, pool_config).await.unwrap();

        let health = pool.health_check().await.unwrap();
        assert!(health.global_healthy);
        assert_eq!(health.loaded_waddle_count, 1);
    }

    #[tokio::test]
    async fn failed_health_check_increments_the_failure_counter() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let pool = DatabasePool::new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("pool");
        pool.global_actor().kill();
        pool.global_actor().wait_for_shutdown().await;

        let health = pool.health_check().await.expect("health result");

        assert!(!health.global_healthy);
        assert_eq!(
            metrics.counter_sum("waddle.db.health_check.failed", &[]),
            Some(1),
            "a failed DB health check must export the alertable counter"
        );
    }

    #[tokio::test]
    async fn test_single_database_accessors() {
        let config = DatabaseConfig::default();
        let pool = DatabasePool::new(config, PoolConfig).await.unwrap();

        assert_eq!(pool.global().name(), "global");
        let health = pool.health_check().await.unwrap();
        assert_eq!(health.loaded_waddle_count, 1);
    }
}
