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

pub mod global;
mod runner;
mod sql;
#[cfg(test)]
mod tests;
mod types;
pub mod waddle;

pub use runner::MigrationRunner;
pub use types::Migration;
