//! Database pool for a single logical Waddle database.
//!
//! Persistence routes through `DbActor` over one shared SQLx-backed database.

use super::actor::{DbActor, DbHealthCheck};
use super::physical_attestation::AttestedPgPool;
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
    pub async fn try_new(
        config: DatabaseConfig,
        _pool_config: PoolConfig,
    ) -> Result<Self, DatabaseError> {
        info!(driver = ?config.driver, "Initializing database pool");

        let global_db = Database::from_config("global", &config).await?;
        Ok(Self::from_database(global_db))
    }

    /// Wrap the same physically attested PostgreSQL pool with disjoint main
    /// and ingress admission budgets. `pool` must have exactly the sum of the
    /// two budgets as its physical maximum; neither lane can borrow from the
    /// other. Runtime startup keeps using [`Self::try_new`] until trusted
    /// owner-to-runtime identity configuration is available.
    pub async fn from_attested_postgres_pool(
        name: &str,
        pool: AttestedPgPool,
        main_budget: u32,
        ingress_budget: u32,
    ) -> Result<Self, DatabaseError> {
        info!(
            main_budget,
            ingress_budget, "Wrapping attested database pool"
        );
        let global_db = Database::from_attested_postgres_pool_with_ingress(
            name,
            pool,
            main_budget,
            ingress_budget,
        )
        .await?;
        Ok(Self::from_database(global_db))
    }

    pub(crate) fn from_database(global_db: Database) -> Self {
        let global_actor = DbActor::spawn(DbActor::new(global_db.clone()));

        Self {
            global_actor,
            global_db,
        }
    }

    pub fn global(&self) -> &Database {
        &self.global_db
    }

    /// Return the ingress admission lane over the same private backend as
    /// [`Self::global`]. If no separate lane was configured, this is a clone
    /// of the global admission class.
    pub fn ingress(&self) -> Database {
        self.global_db.ingress_lane()
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
        let pool = DatabasePool::try_new(config, pool_config).await.unwrap();

        let health = pool.health_check().await.unwrap();
        assert!(health.global_healthy);
        assert_eq!(health.loaded_waddle_count, 1);
    }

    #[tokio::test]
    async fn failed_health_check_increments_the_failure_counter() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let pool = DatabasePool::try_new(DatabaseConfig::default(), PoolConfig)
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
        let pool = DatabasePool::try_new(config, PoolConfig).await.unwrap();

        assert_eq!(pool.global().name(), "global");
        let health = pool.health_check().await.unwrap();
        assert_eq!(health.loaded_waddle_count, 1);
    }
}
