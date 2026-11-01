//! Database module for Waddle Server.
//!
//! Core infrastructure now uses SQLx adapters and a single logical database.

pub mod actor;
mod backend;
pub mod blocking;
pub mod bootstrap;
pub mod lineage;
mod migrations;
pub mod physical_attestation;
mod pool;
pub mod roster;
mod schema;
mod value;

use sqlx::postgres::PgPool;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqlitePool;
#[cfg(test)]
use std::path::Path;
use std::str::FromStr;
use thiserror::Error;
use tracing::{debug, info, instrument};

use backend::CONTROL_PLANE_POOL_ACQUIRE_TIMEOUT;
pub use backend::MAIN_POOL_ACQUIRE_TIMEOUT;
use backend::{connect_backend, DatabaseBackend};
pub use backend::{ConnectionGuard, DatabaseDriver, Transaction};
pub use migrations::{
    migration_checksum, run_dark_postgres_owner, DarkMigrationOwnerConfig, DarkMigrationOwnerError,
    MigrationLedgerError, MigrationNamespace, MigrationRunner, WADDLE_NAMESPACE_START,
};
pub use pool::{DatabasePool, PoolConfig, PoolHealth};
pub use schema::{i64_sql_type, null_safe_eq, widen_postgres_i64_column_to_bigint};
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

#[cfg(test)]
mod cancelled_begin_tests;

#[cfg(test)]
mod tests;

mod database;
pub(crate) use database::sqlite_url_is_in_memory;
use database::sqlite_url_query_requests_memory;
pub use database::ControlPlanePoolConfig;
pub use database::Database;
pub use database::DatabaseConfig;
pub use database::DatabaseError;
pub(crate) use database::SqlitePoolStorage;
pub use database::DEFAULT_CONTROL_PLANE_POOL_SIZE;
pub use database::DEFAULT_POOL_SIZE;
