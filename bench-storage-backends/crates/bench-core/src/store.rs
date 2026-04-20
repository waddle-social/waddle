//! The `StanzaStore` trait every backend implements.

use async_trait::async_trait;
use thiserror::Error;

use crate::message::{ArchivedMessage, MamQuery};
use crate::metrics::OpStats;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("backend error: {0}")]
    Backend(String),
    #[error("queue backpressure: {0}")]
    Backpressure(String),
}

impl StoreError {
    pub fn backend<E: std::fmt::Display>(e: E) -> Self {
        Self::Backend(e.to_string())
    }
}

/// Every backend (SQLite, Postgres, DuckDB, ...) implements this trait.
/// The runner talks to it exclusively — swapping backends is a one-line change.
#[async_trait]
pub trait StanzaStore: Send + Sync + 'static {
    /// Create schema + indexes. Idempotent.
    async fn init(&self) -> Result<(), StoreError>;

    /// Insert one archived message.
    async fn store_message(&self, m: &ArchivedMessage) -> Result<(), StoreError>;

    /// Run a MAM-style query and return rows.
    async fn query_messages(&self, q: &MamQuery) -> Result<Vec<ArchivedMessage>, StoreError>;

    /// Count rows for a room (diagnostic).
    async fn count_messages(&self, room_jid: &str) -> Result<u64, StoreError>;

    /// Optional hook: backend-reported on-disk size in bytes.
    async fn db_size_bytes(&self) -> Result<u64, StoreError> {
        Ok(0)
    }

    /// Backend-internal diagnostic histograms (e.g. queue-wait vs. exec time
    /// for stores that queue writes). Appended verbatim to the JSON report.
    ///
    /// Default: none.
    async fn diagnostics(&self) -> Vec<OpStats> {
        Vec::new()
    }
}
