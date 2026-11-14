//! IQ correlation across S2S streams: at-most-once completion with expiry,
//! on whichever replica the response arrives (ADR-0020 transport; gate
//! G8).
//!
//! A request is registered before it is written to a stream and removed by
//! exactly one of: the response that completes it, the request's own
//! failure before it was written, or its expiry. Each of these removes the
//! row with one `DELETE` on its key, and only the caller whose `DELETE`
//! removed the row acts on it, so a request is completed at most once even
//! when its response reaches a different replica from the one that sent it,
//! or when a response is replayed.

use async_trait::async_trait;
use jid::Jid;
use thiserror::Error;

use crate::db::Database;

/// Which side of a server-to-server exchange made the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CorrelationDirection {
    /// A local entity's request to a remote entity, completed by the
    /// response that arrives on an inbound stream.
    Outbound,
    /// A remote entity's request to a local entity, completed by the local
    /// response sent on an outbound stream.
    Inbound,
}

impl CorrelationDirection {
    /// The stored code.
    pub fn code(self) -> &'static str {
        match self {
            Self::Outbound => "outbound",
            Self::Inbound => "inbound",
        }
    }
}

/// What identifies a pending request: its direction, the local and remote
/// addresses exactly as the request named them, and its id.
///
/// A response matches only when it comes from the request's recipient and
/// is addressed to the request's sender with the same id (RFC 6120
/// §8.2.3); for an outbound request the local address is the bound full JID
/// of the resource that sent it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CorrelationKey {
    pub direction: CorrelationDirection,
    pub local: Jid,
    pub remote: Jid,
    pub id: String,
}

/// The type of a pending request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Get,
    Set,
}

impl RequestKind {
    fn code(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Set => "set",
        }
    }
}

/// A request awaiting its response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRequest {
    pub key: CorrelationKey,
    pub kind: RequestKind,
    /// The namespace of the request's payload. A result carrying a payload
    /// completes the request only when the payload is in this namespace.
    pub payload_namespace: String,
    /// When the request expires, in milliseconds since the Unix epoch.
    pub expires_at_ms: i64,
}

/// Why the correlation table could not be used.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CorrelationError {
    /// A request with the same key is already pending.
    #[error("a request with the same key is pending")]
    Duplicate,
    /// The store failed; the outcome of a write is unknown.
    #[error("the correlation store failed: {0}")]
    Storage(String),
}

/// Pending S2S IQ requests, shared by every replica of a deployment.
#[async_trait]
pub trait IqCorrelations: Send + Sync {
    /// Record `request` as pending. A request with the same key that has
    /// expired is replaced; one that has not is a
    /// [`CorrelationError::Duplicate`].
    async fn register(&self, request: &PendingRequest, now_ms: i64)
        -> Result<(), CorrelationError>;

    /// Complete the unexpired request `key` names with a response whose
    /// payload is in `payload_namespace` (`None` for a response without a
    /// payload, or an error). `true` when this call completed it; the
    /// caller then, and only then, delivers the response.
    async fn complete(
        &self,
        key: &CorrelationKey,
        payload_namespace: Option<&str>,
        now_ms: i64,
    ) -> Result<bool, CorrelationError>;

    /// Remove the request `key` names because it will not be written.
    /// `true` when this call removed it; the caller then answers it.
    async fn withdraw(&self, key: &CorrelationKey) -> Result<bool, CorrelationError>;

    /// Remove the request `key` names if it has expired. `true` when this
    /// call removed it, so no response completed it; the caller then
    /// answers it.
    async fn expire(&self, key: &CorrelationKey, now_ms: i64) -> Result<bool, CorrelationError>;

    /// Remove every request that expired at or before `before_ms`, such as
    /// those left by a replica that stopped. Returns how many were removed.
    async fn prune(&self, before_ms: i64) -> Result<u64, CorrelationError>;
}

/// The correlation table in the deployment's global database
/// (`s2s_iq_correlations`), which every replica shares.
#[derive(Clone)]
pub struct SqlIqCorrelations {
    db: Database,
}

const KEY_CLAUSE: &str = "direction = ? AND local_jid = ? AND remote_jid = ? AND stanza_id = ?";

impl SqlIqCorrelations {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    async fn execute(
        &self,
        sql: &str,
        params: Vec<crate::db::Value>,
    ) -> Result<u64, CorrelationError> {
        let guard = self
            .db
            .guard()
            .await
            .map_err(|error| CorrelationError::Storage(error.to_string()))?;
        guard
            .execute(sql, params)
            .await
            .map_err(|error| CorrelationError::Storage(error.to_string()))
    }
}

fn key_params(key: &CorrelationKey) -> Vec<crate::db::Value> {
    crate::db_params![
        key.direction.code(),
        key.local.as_str(),
        key.remote.as_str(),
        key.id.as_str(),
    ]
}

#[async_trait]
impl IqCorrelations for SqlIqCorrelations {
    async fn register(
        &self,
        request: &PendingRequest,
        now_ms: i64,
    ) -> Result<(), CorrelationError> {
        let mut params = key_params(&request.key);
        params.push(now_ms.into());
        self.execute(
            &[
                "DELETE FROM s2s_iq_correlations WHERE ",
                KEY_CLAUSE,
                " AND expires_at_ms <= ?",
            ]
            .concat(),
            params,
        )
        .await?;
        let mut params = key_params(&request.key);
        params.extend(crate::db_params![
            request.kind.code(),
            request.payload_namespace.as_str(),
            now_ms,
            request.expires_at_ms,
        ]);
        let inserted = self
            .execute(
                "INSERT INTO s2s_iq_correlations \
                 (direction, local_jid, remote_jid, stanza_id, request_type, payload_ns, \
                 created_at_ms, expires_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT DO NOTHING",
                params,
            )
            .await?;
        if inserted == 1 {
            Ok(())
        } else {
            Err(CorrelationError::Duplicate)
        }
    }

    async fn complete(
        &self,
        key: &CorrelationKey,
        payload_namespace: Option<&str>,
        now_ms: i64,
    ) -> Result<bool, CorrelationError> {
        let mut params = key_params(key);
        params.push(now_ms.into());
        let sql = match payload_namespace {
            Some(namespace) => {
                params.push(namespace.into());
                [
                    "DELETE FROM s2s_iq_correlations WHERE ",
                    KEY_CLAUSE,
                    " AND expires_at_ms > ? AND payload_ns = ?",
                ]
                .concat()
            }
            None => [
                "DELETE FROM s2s_iq_correlations WHERE ",
                KEY_CLAUSE,
                " AND expires_at_ms > ?",
            ]
            .concat(),
        };
        Ok(self.execute(&sql, params).await? == 1)
    }

    async fn withdraw(&self, key: &CorrelationKey) -> Result<bool, CorrelationError> {
        let removed = self
            .execute(
                &["DELETE FROM s2s_iq_correlations WHERE ", KEY_CLAUSE].concat(),
                key_params(key),
            )
            .await?;
        Ok(removed == 1)
    }

    async fn expire(&self, key: &CorrelationKey, now_ms: i64) -> Result<bool, CorrelationError> {
        let mut params = key_params(key);
        params.push(now_ms.into());
        let removed = self
            .execute(
                &[
                    "DELETE FROM s2s_iq_correlations WHERE ",
                    KEY_CLAUSE,
                    " AND expires_at_ms <= ?",
                ]
                .concat(),
                params,
            )
            .await?;
        Ok(removed == 1)
    }

    async fn prune(&self, before_ms: i64) -> Result<u64, CorrelationError> {
        self.execute(
            "DELETE FROM s2s_iq_correlations WHERE expires_at_ms <= ?",
            crate::db_params![before_ms],
        )
        .await
    }
}
