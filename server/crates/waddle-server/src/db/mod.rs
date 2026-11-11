//! Database module for Waddle Server.
//!
//! Core infrastructure now uses SQLx adapters and a single logical database.

pub mod actor;
mod backend;
pub(crate) use backend::{bind_postgres, rewrite_positional_for_postgres};
pub mod blocking;
pub mod bootstrap;
mod catalog_fence;
pub(crate) use catalog_fence::{try_catalog_fence_lock, CatalogFenceLock};
pub mod lineage;
mod migrations;
pub mod physical_attestation;
mod pool;
pub(crate) mod private_xml;
pub(crate) mod read_only_timeout;
pub mod roster;
pub mod runtime_startup;
pub(crate) mod runtime_store_acl;
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

use backend::{connect_backend, DatabaseBackend};
pub use backend::{ConnectionGuard, DatabaseDriver, Transaction};
pub use backend::{CONTROL_PLANE_POOL_ACQUIRE_TIMEOUT, MAIN_POOL_ACQUIRE_TIMEOUT};
pub(crate) use migrations::verify_relation_guard_candidate_in_transaction;
pub(crate) use migrations::verify_room_observation_migration_v1023_in_transaction;
pub(crate) use migrations::MIGRATION_LEDGER_ADVISORY_LOCK_KEY;
pub use migrations::{
    acquire_dark_shared_migration_ledger_v1023, acquire_dark_shared_migration_ledger_v1030_read,
    acquire_dark_shared_migration_ledger_v1030_write, bootstrap_dark_postgres_mam_owner,
    grant_dark_owner_rejection_runtime_access, inspect_dark_owner_rejection_runtime_access,
    inspect_dark_postgres_epoch_acl, migration_checksum, run_dark_postgres_owner,
    DarkEpochAclStage, DarkLedgerPreflightError, DarkMamOwnerError, DarkMigrationLedgerPins,
    DarkMigrationOwnerConfig, DarkMigrationOwnerError, MigrationLedgerError, MigrationNamespace,
    MigrationRunner, OwnerRejectionGrantError, OwnerRejectionGrantState, SharedMigrationLedger,
    WADDLE_NAMESPACE_START,
};
#[cfg(feature = "test-support")]
pub use migrations::{
    observe_disposable_epoch_transition_lab, run_disposable_epoch_transition_lab,
    DisposableEpochTransitionAttempt, DisposableEpochTransitionConfig,
    DisposableEpochTransitionError, DisposableEpochTransitionFault,
    DisposableEpochTransitionObservation,
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
mod catalog_fence_tests;

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
