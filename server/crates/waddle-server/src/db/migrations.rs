//! Database migration system for Waddle Server
//!
//! This module provides:
//! - Compile-time embedded SQL migrations
//! - Version tracking via a migrations table
//! - Automatic migration on database initialization
//!
//! # Migration Naming Convention
//!
//! Migration files should be named: `NNNN_description.sql`
//! Where NNNN is a zero-padded version number (e.g., 0001, 0002).

mod checksum;
pub mod global;
mod ledger_error;
mod namespace;
mod owner;
mod runner;
mod sql;
#[cfg(test)]
mod tests;
mod types;
pub mod waddle;

pub use checksum::migration_checksum;
pub use ledger_error::MigrationLedgerError;
pub use namespace::{MigrationNamespace, WADDLE_NAMESPACE_START};
pub(crate) use owner::verify_relation_guard_candidate_in_transaction;
pub(crate) use owner::verify_room_observation_migration_v1023_in_transaction;
pub use owner::{
    bootstrap_dark_postgres_mam_owner, grant_dark_owner_rejection_runtime_access,
    inspect_dark_owner_rejection_runtime_access, inspect_dark_postgres_epoch_acl,
    run_dark_postgres_owner, DarkEpochAclStage, DarkMamOwnerError, DarkMigrationOwnerConfig,
    DarkMigrationOwnerError, OwnerRejectionGrantError, OwnerRejectionGrantState,
};
#[cfg(feature = "test-support")]
pub use owner::{
    observe_disposable_epoch_transition_lab, run_disposable_epoch_transition_lab,
    DisposableEpochTransitionAttempt, DisposableEpochTransitionConfig,
    DisposableEpochTransitionError, DisposableEpochTransitionFault,
    DisposableEpochTransitionObservation,
};
pub use runner::MigrationRunner;
pub(crate) use runner::MIGRATION_LEDGER_ADVISORY_LOCK_KEY;
pub use types::Migration;
