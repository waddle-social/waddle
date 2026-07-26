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

/// Typed payload for a row in `channel_space_links`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSpaceLink {
    pub channel_jid: BareJid,
    pub space_jid: BareJid,
    pub space_node: SpaceNode,
    /// Unix seconds.
    pub created_at: i64,
}

/// Errors returned by [`ChannelSpaceLinkStore`] implementations.
#[derive(Debug, Error)]
pub enum ChannelSpaceLinkError {
    #[error("channel-space link storage error: {0}")]
    Storage(String),
    #[error("invalid JID '{raw}': {source}")]
    InvalidJid {
        raw: String,
        #[source]
        source: jid::Error,
    },
}

/// Storage contract for the channel → space link. Admin V2 reaches the
/// table through this trait so the wire layer never speaks SQL directly.
#[async_trait]
pub trait ChannelSpaceLinkStore: Send + Sync {
    /// Insert or replace the link keyed by `channel_jid`. A channel
    /// belongs to at most one space; calling `set` overwrites any prior
    /// row for that channel.
    ///
    /// The caller is responsible for supplying `created_at` (typically
    /// `time::now_unix_seconds()` at the request boundary). On an
    /// overwrite, the implementation preserves the original
    /// `created_at` so `list_*` ordering is stable.
    async fn set(&self, link: &ChannelSpaceLink) -> Result<(), ChannelSpaceLinkError>;

    /// Remove the link for `channel_jid`. Returns `true` when a row was
    /// removed, `false` when no row matched.
    async fn clear(&self, channel_jid: &BareJid) -> Result<bool, ChannelSpaceLinkError>;

    /// Fetch the link row for `channel_jid` (`Ok(None)` when absent).
    async fn get(
        &self,
        channel_jid: &BareJid,
    ) -> Result<Option<ChannelSpaceLink>, ChannelSpaceLinkError>;

    /// Return every `channel_jid` linked to `space_jid`, ordered by
    /// ascending `created_at` so callers see channels in insertion order.
    async fn list_channels_in_space(
        &self,
        space_jid: &BareJid,
    ) -> Result<Vec<BareJid>, ChannelSpaceLinkError>;

    /// Return every `channel_jid` linked to the exact XEP-0060 Spaces node id.
    async fn list_channels_in_space_node(
        &self,
        space_node: &SpaceNode,
    ) -> Result<Vec<BareJid>, ChannelSpaceLinkError>;

    /// Return every link row, ordered by ascending `created_at` then
    /// `channel_jid` so the listing is deterministic.
    async fn list_all(&self) -> Result<Vec<ChannelSpaceLink>, ChannelSpaceLinkError>;
}

/// SQLx-backed [`ChannelSpaceLinkStore`].
#[derive(Clone)]
pub struct DatabaseChannelSpaceLinkStore {
    db: Database,
}

impl DatabaseChannelSpaceLinkStore {
    /// Open (or create) the link storage at `database_url`. When `None`,
    /// an ephemeral in-memory SQLite database is used — this mirrors the
    /// spaces-metadata storage so tests can spin up without a DSN.
    pub async fn open(database_url: Option<&str>) -> Result<Self, ChannelSpaceLinkError> {
        let db = match database_url {
            Some(url) => open_database(url).await?,
            None => Database::in_memory("channel_space_links")
                .await
                .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?,
        };
        let store = Self { db };
        schema::initialize(&store).await?;
        info!(driver = ?store.db.driver(), "Channel-space link storage initialized");
        Ok(store)
    }

    pub(crate) async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, ChannelSpaceLinkError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))
    }

    pub(crate) async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, ChannelSpaceLinkError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))
    }
}

/// Build a trait-object [`ChannelSpaceLinkStore`] for `AppState`.
/// Mirrors [`crate::spaces_metadata::build_spaces_metadata_store`].
pub async fn build_channel_space_link_store(
    database_url: Option<String>,
) -> Result<Arc<dyn ChannelSpaceLinkStore>, ChannelSpaceLinkError> {
    Ok(Arc::new(
        DatabaseChannelSpaceLinkStore::open(database_url.as_deref()).await?,
    ))
}

async fn open_database(database_url: &str) -> Result<Database, ChannelSpaceLinkError> {
    ensure_sqlite_parent_dir(database_url)?;
    let driver = infer_database_driver(database_url)?;
    Database::from_config(
        "channel_space_links",
        &DatabaseConfig::new(driver, database_url.to_string()),
    )
    .await
    .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))
}

fn infer_database_driver(database_url: &str) -> Result<DatabaseDriver, ChannelSpaceLinkError> {
    let lower = database_url.to_ascii_lowercase();
    if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
        return Ok(DatabaseDriver::Postgres);
    }
    if lower.starts_with("sqlite:") {
        return Ok(DatabaseDriver::Sqlite);
    }
    Err(ChannelSpaceLinkError::Storage(format!(
        "unsupported channel_space_links database URL '{database_url}': expected sqlite: or postgres://"
    )))
}

fn ensure_sqlite_parent_dir(database_url: &str) -> Result<(), ChannelSpaceLinkError> {
    let Some(path) = sqlite_database_path(database_url) else {
        return Ok(());
    };

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
    }
    Ok(())
}

fn sqlite_database_path(database_url: &str) -> Option<&Path> {
    let path = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))?;
    if path.is_empty() || path.starts_with(":memory:") || path.starts_with("file:") {
        return None;
    }
    Some(Path::new(path))
}

fn decode_row(row: &crate::db::Row) -> Result<ChannelSpaceLink, ChannelSpaceLinkError> {
    let channel_jid_raw: String = row
        .get(0)
        .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
    let channel_jid: BareJid =
        channel_jid_raw
            .parse()
            .map_err(|error: jid::Error| ChannelSpaceLinkError::InvalidJid {
                raw: channel_jid_raw.clone(),
                source: error,
            })?;
    let space_jid_raw: String = row
        .get(1)
        .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
    let space_jid: BareJid =
        space_jid_raw
            .parse()
            .map_err(|error: jid::Error| ChannelSpaceLinkError::InvalidJid {
                raw: space_jid_raw.clone(),
                source: error,
            })?;
    let space_node: String = row
        .get(2)
        .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
    let created_at: i64 = row
        .get(3)
        .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
    Ok(ChannelSpaceLink {
        channel_jid,
        space_jid,
        space_node: SpaceNode::from(space_node),
        created_at,
    })
}

const SELECT_COLS: &str = "channel_jid, space_jid, space_node, created_at";

#[async_trait]
impl ChannelSpaceLinkStore for DatabaseChannelSpaceLinkStore {
    #[instrument(skip(self, link), fields(channel = %link.channel_jid, space = %link.space_jid))]
    async fn set(&self, link: &ChannelSpaceLink) -> Result<(), ChannelSpaceLinkError> {
        // Preserve the original `created_at` on overwrite so list
        // ordering is stable.
        let sql = r#"
            INSERT INTO channel_space_links (
                channel_jid, space_jid, space_node, created_at
            ) VALUES (?, ?, ?, ?)
            ON CONFLICT(channel_jid) DO UPDATE SET
                space_jid = excluded.space_jid,
                space_node = excluded.space_node
        "#;
        self.execute(
            sql,
            crate::db_params![
                link.channel_jid.to_string(),
                link.space_jid.to_string(),
                link.space_node.as_str(),
                link.created_at,
            ],
        )
        .await?;
        Ok(())
    }

    #[instrument(skip(self, channel_jid), fields(channel = %channel_jid))]
    async fn clear(&self, channel_jid: &BareJid) -> Result<bool, ChannelSpaceLinkError> {
        let affected = self
            .execute(
                "DELETE FROM channel_space_links WHERE channel_jid = ?",
                crate::db_params![channel_jid.to_string()],
            )
            .await?;
        Ok(affected > 0)
    }

    #[instrument(skip(self, channel_jid), fields(channel = %channel_jid))]
    async fn get(
        &self,
        channel_jid: &BareJid,
    ) -> Result<Option<ChannelSpaceLink>, ChannelSpaceLinkError> {
        let sql = format!("SELECT {SELECT_COLS} FROM channel_space_links WHERE channel_jid = ?");
        let mut rows = self
            .query(&sql, crate::db_params![channel_jid.to_string()])
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_row(&row)?))
    }

    #[instrument(skip(self, space_jid), fields(space = %space_jid))]
    async fn list_channels_in_space(
        &self,
        space_jid: &BareJid,
    ) -> Result<Vec<BareJid>, ChannelSpaceLinkError> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM channel_space_links \
             WHERE space_jid = ? \
             ORDER BY created_at ASC, channel_jid ASC"
        );
        let mut rows = self
            .query(&sql, crate::db_params![space_jid.to_string()])
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?
        {
            out.push(decode_row(&row)?.channel_jid);
        }
        Ok(out)
    }

    #[instrument(skip(self), fields(space_node = %space_node))]
    async fn list_channels_in_space_node(
        &self,
        space_node: &SpaceNode,
    ) -> Result<Vec<BareJid>, ChannelSpaceLinkError> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM channel_space_links \
             WHERE space_node = ? \
             ORDER BY created_at ASC, channel_jid ASC"
        );
        let mut rows = self
            .query(&sql, crate::db_params![space_node.as_str()])
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?
        {
            out.push(decode_row(&row)?.channel_jid);
        }
        Ok(out)
    }

    #[instrument(skip(self))]
    async fn list_all(&self) -> Result<Vec<ChannelSpaceLink>, ChannelSpaceLinkError> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM channel_space_links \
             ORDER BY created_at ASC, channel_jid ASC"
        );
        let mut rows = self.query(&sql, ()).await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?
        {
            out.push(decode_row(&row)?);
        }
        Ok(out)
    }
}
