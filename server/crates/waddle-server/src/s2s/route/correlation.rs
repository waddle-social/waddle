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
//!
//! Each row names the replica that registered it ([`CorrelationOwner`]).
//! That replica reads its own rows to learn which of its requests another
//! replica completed ([`IqCorrelations::outstanding`]), and records that it
//! is alive ([`IqCorrelations::heartbeat`]); only the rows of a replica that
//! has stopped recording are pruned.

use async_trait::async_trait;
use jid::Jid;
use thiserror::Error;

use crate::db::Database;

/// The longest IQ id a correlated request may carry, in bytes.
pub const MAX_REQUEST_ID_BYTES: usize = 256;

/// The longest local or remote address a correlated request may name, in
/// bytes. With [`MAX_REQUEST_ID_BYTES`] it keeps the table's primary key
/// below PostgreSQL's B-tree entry limit (2704 bytes with the default 8 kB
/// page): two addresses, the id and the direction stay under 2400 bytes
/// with their headers.
pub const MAX_CORRELATED_ADDRESS_BYTES: usize = 1023;

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

impl CorrelationKey {
    /// Whether the key can be stored: a non-empty id of at most
    /// [`MAX_REQUEST_ID_BYTES`] and addresses of at most
    /// [`MAX_CORRELATED_ADDRESS_BYTES`] each.
    pub fn fits(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= MAX_REQUEST_ID_BYTES
            && self.local.as_str().len() <= MAX_CORRELATED_ADDRESS_BYTES
            && self.remote.as_str().len() <= MAX_CORRELATED_ADDRESS_BYTES
    }
}

/// The type of a pending request.
///
/// It is stored with the request but not matched on completion: RFC 6120
/// §8.2.3 answers a `get` and a `set` with the same response types, so a
/// response says nothing about which of the two it answers.
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

/// The replica that registered a request: one per router, never reused.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CorrelationOwner(String);

impl CorrelationOwner {
    /// A new, unique owner.
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// The owner named `id`.
    pub fn named(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
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
    /// The replica that registers it.
    pub owner: CorrelationOwner,
    /// When the request expires, in milliseconds since the Unix epoch.
    pub expires_at_ms: i64,
}

/// Why the correlation table could not be used.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CorrelationError {
    /// A request with the same key is already pending.
    #[error("a request with the same key is pending")]
    Duplicate,
    /// The key is longer than the table stores ([`CorrelationKey::fits`]).
    #[error("the request's addresses or id are too long to correlate")]
    KeyTooLong,
    /// The store failed; the outcome of a write is unknown.
    #[error("the correlation store failed: {0}")]
    Storage(String),
}

/// Pending S2S IQ requests, shared by every replica of a deployment.
#[async_trait]
pub trait IqCorrelations: Send + Sync {
    /// Record `request` as pending. A request with the same key that has
    /// expired is replaced; one that has not is a
    /// [`CorrelationError::Duplicate`]. A key that does not
    /// [fit](CorrelationKey::fits) is a [`CorrelationError::KeyTooLong`] and
    /// nothing is written.
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

    /// The keys of the outbound requests `owner` registered that are still
    /// stored. A request `owner` registered that is missing here was
    /// completed, withdrawn or expired by whoever removed it.
    async fn outstanding(
        &self,
        owner: &CorrelationOwner,
    ) -> Result<Vec<CorrelationKey>, CorrelationError>;

    /// Record that `owner` is alive at `now_ms`, so its requests are not
    /// pruned.
    async fn heartbeat(
        &self,
        owner: &CorrelationOwner,
        now_ms: i64,
    ) -> Result<(), CorrelationError>;

    /// Remove every request that expired at or before `before_ms` and whose
    /// owner has not recorded that it is alive since then, such as those a
    /// stopped replica left, and forget such owners. A live owner answers
    /// its own expired requests. Returns how many requests were removed.
    async fn prune(&self, before_ms: i64) -> Result<u64, CorrelationError>;
}

/// The correlation table in the deployment's global database
/// (`s2s_iq_correlations`, with `s2s_iq_correlation_owners`), which every
/// replica shares.
#[derive(Clone)]
pub struct SqlIqCorrelations {
    db: Database,
}

const KEY_CLAUSE: &str = "direction = ? AND local_jid = ? AND remote_jid = ? AND stanza_id = ?";

fn storage(error: impl std::fmt::Display) -> CorrelationError {
    CorrelationError::Storage(error.to_string())
}

impl SqlIqCorrelations {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    async fn execute(
        &self,
        sql: &str,
        params: Vec<crate::db::Value>,
    ) -> Result<u64, CorrelationError> {
        let guard = self.db.guard().await.map_err(storage)?;
        guard.execute(sql, params).await.map_err(storage)
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
        if !request.key.fits() {
            return Err(CorrelationError::KeyTooLong);
        }
        // Replacing an expired request and inserting the new one commit
        // together, so a failure leaves neither.
        let mut transaction = self.db.begin_immediate().await.map_err(storage)?;
        let mut params = key_params(&request.key);
        params.push(now_ms.into());
        transaction
            .execute(
                &[
                    "DELETE FROM s2s_iq_correlations WHERE ",
                    KEY_CLAUSE,
                    " AND expires_at_ms <= ?",
                ]
                .concat(),
                params,
            )
            .await
            .map_err(storage)?;
        let mut params = key_params(&request.key);
        params.extend(crate::db_params![
            request.kind.code(),
            request.payload_namespace.as_str(),
            request.owner.as_str(),
            now_ms,
            request.expires_at_ms,
        ]);
        let inserted = transaction
            .execute(
                "INSERT INTO s2s_iq_correlations \
                 (direction, local_jid, remote_jid, stanza_id, request_type, payload_ns, \
                 owner_id, created_at_ms, expires_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT DO NOTHING",
                params,
            )
            .await
            .map_err(storage)?;
        transaction.commit().await.map_err(storage)?;
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

    async fn outstanding(
        &self,
        owner: &CorrelationOwner,
    ) -> Result<Vec<CorrelationKey>, CorrelationError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard
            .query(
                "SELECT local_jid, remote_jid, stanza_id FROM s2s_iq_correlations \
                 WHERE owner_id = ? AND direction = ?",
                crate::db_params![owner.as_str(), CorrelationDirection::Outbound.code()],
            )
            .await
            .map_err(storage)?;
        let mut keys = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            let local: String = row.get(0).map_err(storage)?;
            let remote: String = row.get(1).map_err(storage)?;
            let id: String = row.get(2).map_err(storage)?;
            // Every stored address was written from a parsed JID. One that
            // no longer parses makes the whole answer unusable, rather than
            // reporting its request as removed.
            keys.push(CorrelationKey {
                direction: CorrelationDirection::Outbound,
                local: Jid::new(&local).map_err(storage)?,
                remote: Jid::new(&remote).map_err(storage)?,
                id,
            });
        }
        Ok(keys)
    }

    async fn heartbeat(
        &self,
        owner: &CorrelationOwner,
        now_ms: i64,
    ) -> Result<(), CorrelationError> {
        self.execute(
            "INSERT INTO s2s_iq_correlation_owners (owner_id, seen_at_ms) VALUES (?, ?) \
             ON CONFLICT (owner_id) DO UPDATE SET seen_at_ms = excluded.seen_at_ms",
            crate::db_params![owner.as_str(), now_ms],
        )
        .await
        .map(|_| ())
    }

    async fn prune(&self, before_ms: i64) -> Result<u64, CorrelationError> {
        let removed = self
            .execute(
                "DELETE FROM s2s_iq_correlations WHERE expires_at_ms <= ? AND owner_id NOT IN \
                 (SELECT owner_id FROM s2s_iq_correlation_owners WHERE seen_at_ms > ?)",
                crate::db_params![before_ms, before_ms],
            )
            .await?;
        self.execute(
            "DELETE FROM s2s_iq_correlation_owners WHERE seen_at_ms <= ?",
            crate::db_params![before_ms],
        )
        .await?;
        Ok(removed)
    }
}
