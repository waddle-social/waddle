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

/// Typed payload for a row in `spaces_metadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceMetadata {
    pub space_jid: BareJid,
    pub space_node: SpaceNode,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

/// Errors returned by [`SpacesMetadataStore`] implementations.
#[derive(Debug, Error)]
pub enum SpacesMetadataError {
    #[error("spaces metadata storage error: {0}")]
    Storage(String),
    #[error("invalid space JID '{raw}': {source}")]
    InvalidJid {
        raw: String,
        #[source]
        source: jid::Error,
    },
}

/// Storage contract for spaces metadata. Admin V2's `spaces:create` /
/// `spaces:update` / `spaces:delete` handlers (added in a follow-up PR)
/// reach the table through this trait so the wire layer never speaks SQL
/// directly.
#[async_trait]
pub trait SpacesMetadataStore: Send + Sync {
    /// Fetch the metadata row for `space_jid` (`Ok(None)` when absent).
    async fn get(&self, space_jid: &BareJid) -> Result<Option<SpaceMetadata>, SpacesMetadataError>;

    /// Fetch the metadata row for the exact XEP-0060 Spaces node id.
    async fn get_by_node(
        &self,
        space_node: &SpaceNode,
    ) -> Result<Option<SpaceMetadata>, SpacesMetadataError>;

    /// Insert or replace the metadata row keyed by `metadata.space_jid`.
    ///
    /// On conflict the row is overwritten; the caller is responsible for
    /// supplying a monotonically-correct `updated_at` (typically
    /// `time::now_unix_seconds()` at the request boundary).
    async fn upsert(&self, metadata: &SpaceMetadata) -> Result<(), SpacesMetadataError>;

    /// Delete the metadata row for `space_jid`.
    ///
    /// Returns `true` when a row was removed, `false` when no row matched.
    async fn delete(&self, space_jid: &BareJid) -> Result<bool, SpacesMetadataError>;

    /// Delete the metadata row for the exact XEP-0060 Spaces node id.
    async fn delete_by_node(&self, space_node: &SpaceNode) -> Result<bool, SpacesMetadataError>;

    /// Return every metadata row, ordered by ascending `created_at` so
    /// callers see rows in insertion order. Pagination is intentionally
    /// out of scope here; the wire layer (admin V2 retry) layers cursors
    /// on top.
    async fn list_all(&self) -> Result<Vec<SpaceMetadata>, SpacesMetadataError>;
}

/// SQLx-backed [`SpacesMetadataStore`].
#[derive(Clone)]
pub struct DatabaseSpacesMetadataStore {
    db: Database,
}

impl DatabaseSpacesMetadataStore {
    pub fn database(&self) -> Database {
        self.db.clone()
    }

    /// Open (or create) the spaces-metadata storage at `database_url`.
    /// When `None`, an ephemeral in-memory SQLite database is used; this
    /// mirrors the inbox storage so tests can spin up without a DSN.
    pub async fn open(database_url: Option<&str>) -> Result<Self, SpacesMetadataError> {
        let db = match database_url {
            Some(url) => open_database(url).await?,
            None => Database::in_memory("spaces_metadata")
                .await
                .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?,
        };
        let store = Self { db };
        schema::initialize(&store).await?;
        info!(driver = ?store.db.driver(), "Spaces metadata storage initialized");
        Ok(store)
    }

    pub(crate) async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, SpacesMetadataError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))
    }

    pub(crate) async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, SpacesMetadataError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))
    }
}

/// Build a trait-object [`SpacesMetadataStore`] for `AppState`. Mirrors
/// [`crate::inbox::build_inbox_storage`].
pub async fn build_spaces_metadata_store(
    database_url: Option<String>,
) -> Result<Arc<dyn SpacesMetadataStore>, SpacesMetadataError> {
    Ok(Arc::new(
        DatabaseSpacesMetadataStore::open(database_url.as_deref()).await?,
    ))
}

async fn open_database(database_url: &str) -> Result<Database, SpacesMetadataError> {
    ensure_sqlite_parent_dir(database_url)?;
    let driver = infer_database_driver(database_url)?;
    Database::from_config(
        "spaces_metadata",
        &DatabaseConfig::new(driver, database_url.to_string()),
    )
    .await
    .map_err(|error| SpacesMetadataError::Storage(error.to_string()))
}

fn infer_database_driver(database_url: &str) -> Result<DatabaseDriver, SpacesMetadataError> {
    let lower = database_url.to_ascii_lowercase();
    if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
        return Ok(DatabaseDriver::Postgres);
    }
    if lower.starts_with("sqlite:") {
        return Ok(DatabaseDriver::Sqlite);
    }
    Err(SpacesMetadataError::Storage(format!(
        "unsupported spaces metadata database URL '{database_url}': expected sqlite: or postgres://"
    )))
}

fn ensure_sqlite_parent_dir(database_url: &str) -> Result<(), SpacesMetadataError> {
    let Some(path) = sqlite_database_path(database_url) else {
        return Ok(());
    };

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
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

fn decode_row(row: &crate::db::Row) -> Result<SpaceMetadata, SpacesMetadataError> {
    let space_jid_raw: String = row
        .get(0)
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    let space_jid: BareJid =
        space_jid_raw
            .parse()
            .map_err(|error: jid::Error| SpacesMetadataError::InvalidJid {
                raw: space_jid_raw.clone(),
                source: error,
            })?;
    let space_node: String = row
        .get(1)
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    let name: String = row
        .get(2)
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    let description: Option<String> = row
        .get(3)
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    let icon_url: Option<String> = row
        .get(4)
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    let created_at: i64 = row
        .get(5)
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    let updated_at: i64 = row
        .get(6)
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    Ok(SpaceMetadata {
        space_jid,
        space_node: SpaceNode::from(space_node),
        name,
        description,
        icon_url,
        created_at,
        updated_at,
    })
}

const SELECT_COLS: &str =
    "space_jid, space_node, name, description, icon_url, created_at, updated_at";

#[async_trait]
impl SpacesMetadataStore for DatabaseSpacesMetadataStore {
    #[instrument(skip(self, space_jid), fields(space = %space_jid))]
    async fn get(&self, space_jid: &BareJid) -> Result<Option<SpaceMetadata>, SpacesMetadataError> {
        let sql = format!("SELECT {SELECT_COLS} FROM spaces_metadata WHERE space_jid = ?");
        let mut rows = self
            .query(&sql, crate::db_params![space_jid.to_string()])
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_row(&row)?))
    }

    #[instrument(skip(self), fields(space_node = %space_node))]
    async fn get_by_node(
        &self,
        space_node: &SpaceNode,
    ) -> Result<Option<SpaceMetadata>, SpacesMetadataError> {
        let sql = format!("SELECT {SELECT_COLS} FROM spaces_metadata WHERE space_node = ?");
        let mut rows = self
            .query(&sql, crate::db_params![space_node.as_str()])
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_row(&row)?))
    }

    #[instrument(skip(self, metadata), fields(space = %metadata.space_jid))]
    async fn upsert(&self, metadata: &SpaceMetadata) -> Result<(), SpacesMetadataError> {
        let sql = r#"
            INSERT INTO spaces_metadata (
                space_jid, space_node, name, description, icon_url, created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(space_node) DO UPDATE SET
                space_jid = excluded.space_jid,
                name = excluded.name,
                description = excluded.description,
                icon_url = excluded.icon_url,
                updated_at = excluded.updated_at
        "#;
        self.execute(
            sql,
            crate::db_params![
                metadata.space_jid.to_string(),
                metadata.space_node.as_str(),
                metadata.name.clone(),
                metadata.description.clone(),
                metadata.icon_url.clone(),
                metadata.created_at,
                metadata.updated_at,
            ],
        )
        .await?;
        Ok(())
    }

    #[instrument(skip(self, space_jid), fields(space = %space_jid))]
    async fn delete(&self, space_jid: &BareJid) -> Result<bool, SpacesMetadataError> {
        let affected = self
            .execute(
                "DELETE FROM spaces_metadata WHERE space_jid = ?",
                crate::db_params![space_jid.to_string()],
            )
            .await?;
        Ok(affected > 0)
    }

    #[instrument(skip(self), fields(space_node = %space_node))]
    async fn delete_by_node(&self, space_node: &SpaceNode) -> Result<bool, SpacesMetadataError> {
        let affected = self
            .execute(
                "DELETE FROM spaces_metadata WHERE space_node = ?",
                crate::db_params![space_node.as_str()],
            )
            .await?;
        Ok(affected > 0)
    }

    #[instrument(skip(self))]
    async fn list_all(&self) -> Result<Vec<SpaceMetadata>, SpacesMetadataError> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM spaces_metadata ORDER BY created_at ASC, space_node ASC"
        );
        let mut rows = self.query(&sql, ()).await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?
        {
            out.push(decode_row(&row)?);
        }
        Ok(out)
    }
}
