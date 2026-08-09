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
mod runner;
mod sql;
#[cfg(test)]
mod tests;
mod types;
pub mod waddle;

pub use checksum::migration_checksum;
pub use ledger_error::MigrationLedgerError;
pub use namespace::{MigrationNamespace, WADDLE_NAMESPACE_START};
pub use runner::MigrationRunner;
pub use types::Migration;
