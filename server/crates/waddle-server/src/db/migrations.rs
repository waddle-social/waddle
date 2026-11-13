//! Database migration system for Waddle Server.
//!
//! The migration catalog is the schema baseline (version 2000, generated
//! from the retired migration history and embedded from
//! `sql/baseline/`) followed by the migrations in [`after_baseline`]. The
//! ledger table `_migrations` records each applied version with its
//! checksum. A database the retired history built is recognised by its
//! ledger and structure and stamped onto the baseline; see [`baseline`].

mod after_baseline;
mod baseline;
mod checksum;
mod ledger_error;
mod owner;
mod postgres_ledger;
mod runner;
mod sql;
#[cfg(test)]
mod tests;
mod types;

pub(crate) use baseline::postgres_function_body;
pub use baseline::{
    select_from_env as select_schema_baseline_from_env, BaselineAction, BaselineActionError,
    BASELINE_ACTION_ENV, BASELINE_VERSION,
};
pub use checksum::migration_checksum;
pub use ledger_error::MigrationLedgerError;
pub(crate) use owner::is_installed_epoch_guard_body;
pub(crate) use owner::verify_relation_guard_candidate_in_transaction;
pub(crate) use owner::verify_room_observation_migration_v1023_in_transaction;
pub use owner::{
    acquire_dark_shared_migration_ledger_v1023, acquire_dark_shared_migration_ledger_v1030_read,
    acquire_dark_shared_migration_ledger_v1030_write, bootstrap_dark_postgres_mam_owner,
    grant_dark_owner_rejection_runtime_access, inspect_dark_owner_rejection_runtime_access,
    inspect_dark_postgres_epoch_acl, run_dark_postgres_owner,
    verify_dark_shared_migration_ledger_v1030_read_sqlx, DarkEpochAclStage,
    DarkLedgerPreflightError, DarkMamOwnerError, DarkMigrationLedgerPins, DarkMigrationOwnerConfig,
    DarkMigrationOwnerError, OwnerRejectionGrantError, OwnerRejectionGrantState,
    SharedMigrationLedger,
};
#[cfg(feature = "test-support")]
pub use owner::{
    observe_disposable_epoch_transition_lab, run_disposable_epoch_transition_lab,
    DisposableEpochTransitionAttempt, DisposableEpochTransitionConfig,
    DisposableEpochTransitionError, DisposableEpochTransitionFault,
    DisposableEpochTransitionObservation,
};
pub(crate) use owner::{
    verify_dark_shared_migration_ledger_v1030_partial_profile_sqlx,
    verify_dark_shared_migration_ledger_v1030_write_sqlx,
};
pub use runner::MigrationRunner;
pub(crate) use runner::MIGRATION_LEDGER_ADVISORY_LOCK_KEY;
pub use types::Migration;
