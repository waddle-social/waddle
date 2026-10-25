//! Storage layer for the channel → space link.
//!
//! XEP-0503 in Waddle treats a channel as belonging to at most one space.
//! Admin V2's `channels:list` accepts a `space_jid` filter argument that
//! today no-ops because no persistent channel↔space association exists in
//! the server. This module is the typed projection that records the
//! association so the filter actually narrows results.
//!
//! Mirrors the [`crate::spaces_metadata`] pattern:
//! - typed values (`ChannelSpaceLink`, `BareJid`, typed error enum) at
//!   every boundary,
//! - no `unwrap` in storage paths,
//! - `CREATE TABLE IF NOT EXISTS` idempotent schema migration,
//! - in-memory + SQLite-backed implementations under one trait.
//!
//! Schema is intentionally minimal — one row per `channel_jid`, pointing
//! at the parent `space_jid`. There is no per-channel metadata table
//! today; channel name/topic/visibility live on the MUC `RoomConfig`. If
//! we ever want richer channel metadata, this module is the natural place
//! to grow it (extra columns / sibling tables).

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

pub use storage::InMemoryChannelSpaceLinkStore;

mod links;
pub use links::build_channel_space_link_store;
use links::decode_row;
use links::ensure_sqlite_parent_dir;
use links::infer_database_driver;
use links::open_database;
use links::sqlite_database_path;
pub use links::ChannelSpaceLink;
pub use links::ChannelSpaceLinkError;
pub use links::ChannelSpaceLinkStore;
pub use links::DatabaseChannelSpaceLinkStore;
use links::SELECT_COLS;
