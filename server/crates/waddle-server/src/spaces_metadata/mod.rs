//! Storage layer for XEP-0503 spaces metadata.
//!
//! Spaces today derive their identity from their pubsub-tree config; there
//! is no durable place to record the human-facing fields (`name`,
//! `description`, `icon_url`) that admin V2's `spaces:create` /
//! `spaces:update` commands need. This module is the typed, server-side
//! projection that fills that gap.
//!
//! Wire shape lives elsewhere — the admin V2 retry adds the IQs that
//! mutate metadata; this PR ships plumbing only.
//!
//! Hard rules:
//! - typed values (`SpaceMetadata`, `BareJid`, typed error enum) at every
//!   boundary,
//! - no `unwrap` in storage paths,
//! - `CREATE TABLE IF NOT EXISTS` idempotent schema, mirroring the inbox
//!   migration pattern.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use jid::BareJid;
use thiserror::Error;
use tracing::{info, instrument};

use crate::db::{Database, DatabaseConfig, DatabaseDriver, IntoParams};
use crate::space_identity::SpaceNode;

mod schema;
pub mod storage;
#[cfg(test)]
mod tests;

pub use storage::InMemorySpacesMetadataStore;

mod metadata;
pub use metadata::build_spaces_metadata_store;
use metadata::decode_row;
use metadata::ensure_sqlite_parent_dir;
use metadata::infer_database_driver;
use metadata::open_database;
use metadata::sqlite_database_path;
pub use metadata::DatabaseSpacesMetadataStore;
pub use metadata::SpaceMetadata;
pub use metadata::SpacesMetadataError;
pub use metadata::SpacesMetadataStore;
use metadata::SELECT_COLS;
