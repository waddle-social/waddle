//! XEP-0160 offline-message delivery (issue #209).
//!
//! Holds the typed [`PendingRow`] / [`PendingPayload`] model and the
//! [`storage::PendingDeliveryStorage`] persistence contract. The actual
//! libSQL-backed implementation lives in `waddle-server`; this crate
//! ships an in-memory fake usable by handler and routing tests.
//!
//! Design lock-ins (issue #209 grilling session):
//!
//! - **Q1 = C**: MAM is canonical, `pending_delivery` is a thin pointer
//!   table. Rows reference MAM via XEP-0359 stanza-id (Archived) or
//!   carry an inline payload for `<no-permanent-store/>` (Transient).
//! - **Q4 = A**: Tagged payload — [`PendingPayload::Archived`] vs
//!   [`PendingPayload::Transient`].
//! - **Q7b = B1**: Rows tagged with `flushed_in_session` during the
//!   in-flight window between flush-write and SM-ack; deletion happens
//!   on SM-ack only.
//! - **Q7c = C**: Per-user-bare-JID lock — first non-negative-priority
//!   presence wins; rows are claimed (marked `flushed_in_session`)
//!   rather than deleted, so a session that dies pre-ack can release
//!   them for reflush.
//! - **Q9 = count cap, refuse-on-full**: [`QuotaPolicy::CountCap`] caps
//!   per-recipient row count; over-cap inserts return
//!   [`InsertOutcome::QuotaExceeded`] so the routing layer can return
//!   `<service-unavailable/>` per XEP-0160 §3 step 3.

pub mod flush;
pub mod storage;

use crate::protocol::event::StanzaIdRef;
use chrono::{DateTime, Utc};
use jid::BareJid;
use xmpp_parsers::message::Message;

/// Opaque per-row identifier for a `pending_delivery` row.
///
/// Newtype around `String` so a row id cannot be accidentally swapped
/// for a stream-id, stanza-id, or any other opaque string at the
/// storage boundary (typed-payloads hard rule). Production storage
/// generates UUID v7 values so `ORDER BY id` reproduces FIFO without
/// driver-specific autoincrement syntax.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PendingRowId(String);

impl PendingRowId {
    /// Generate a fresh UUID-v7-based id (sortable by time of generation).
    pub fn fresh() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    /// Wrap an existing id value (e.g. from a SELECT result).
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PendingRowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque XEP-0198 stream-id, identifying a single SM session.
///
/// Two SM sessions for the same user have different `SmSessionId`s.
/// Reusing the existing `stream_id: String` shape from
/// [`crate::stream_management::session_registry::DetachedSession`] —
/// kept as a newtype here so a session id cannot be silently swapped
/// for another opaque string at the storage boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SmSessionId(String);

impl SmSessionId {
    /// Wrap a stream-id value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SmSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Typed payload of a [`PendingRow`].
///
/// `Archived` rows are flushed by reading the stanza back out of MAM
/// (so [`xmpp_parsers::message::Message`] body / corrections / reactions
/// applied after intake but before flush propagate). `Transient` rows
/// carry the stanza inline because no MAM row exists for them.
#[derive(Debug, Clone)]
pub enum PendingPayload {
    /// Pointer into MAM. The flush handler reads the archived message
    /// by `StanzaIdRef` and pushes that.
    Archived(StanzaIdRef),
    /// Inline payload — `<no-permanent-store/>` stanzas have no MAM row.
    /// Boxed so the size of [`PendingPayload`] doesn't blow up.
    Transient(Box<Message>),
}

impl PendingPayload {
    /// True when this row's payload is `Archived(_)`.
    pub fn is_archived(&self) -> bool {
        matches!(self, PendingPayload::Archived(_))
    }

    /// True when this row's payload is `Transient(_)`.
    pub fn is_transient(&self) -> bool {
        matches!(self, PendingPayload::Transient(_))
    }
}

/// One row in the `pending_delivery` table.
#[derive(Debug, Clone)]
pub struct PendingRow {
    /// Per-row identifier — generated at insert time (UUID v7) so
    /// per-row delete/release can target this row without conflating
    /// with neighbours that share the same `flushed_in_session` claim.
    pub id: PendingRowId,
    /// Recipient (always a bare JID — XEP-0160 §3 stores per-user, not
    /// per-resource).
    pub recipient: BareJid,
    /// Original receipt time at the server. Stamped onto `<delay/>` per
    /// XEP-0203 §4.1 + XEP-0198 §5: must be the original (failed)
    /// delivery timestamp, NOT flush time.
    pub original_receipt_at: DateTime<Utc>,
    /// Tagged payload (Archived → MAM lookup; Transient → inline).
    pub payload: PendingPayload,
    /// Session that has claimed this row for flush via the per-user
    /// lock (Q7c). `None` until claimed; `Some(_)` between claim and
    /// SM-ack. Released back to `None` on session expiry pre-ack
    /// (Q7c re-flush).
    pub flushed_in_session: Option<SmSessionId>,
}

/// Quota policy controlling [`storage::PendingDeliveryStorage::insert`]
/// admission (locked Q9 — count cap, refuse on overflow).
#[derive(Debug, Clone, Copy)]
pub enum QuotaPolicy {
    /// No cap — insertion always accepted.
    Unlimited,
    /// Per-recipient row count cap. Inserts that would exceed `max_rows`
    /// return [`InsertOutcome::QuotaExceeded`] so the caller can return
    /// `<service-unavailable/>` to the sender per XEP-0160 §3 step 3.
    CountCap { max_rows: u32 },
}

impl QuotaPolicy {
    /// Default per-recipient cap (Q9e: server-wide config; default 1000).
    pub const DEFAULT_MAX_ROWS: u32 = 1000;

    /// The default policy used when no override is configured.
    pub fn default_policy() -> Self {
        Self::CountCap {
            max_rows: Self::DEFAULT_MAX_ROWS,
        }
    }
}

/// Outcome of [`storage::PendingDeliveryStorage::insert`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOutcome {
    /// Row accepted and persisted.
    Inserted,
    /// Quota would be exceeded — caller MUST return `<service-unavailable/>`
    /// to the sender per XEP-0160 §3 step 3.
    QuotaExceeded,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_payload_classification() {
        let archived = PendingPayload::Archived(StanzaIdRef {
            by: "alice@example.com".parse().unwrap(),
            id: crate::protocol::event::StanzaIdValue::new("opaque-id"),
        });
        assert!(archived.is_archived());
        assert!(!archived.is_transient());

        let transient = PendingPayload::Transient(Box::new(Message::new(None::<jid::Jid>)));
        assert!(!transient.is_archived());
        assert!(transient.is_transient());
    }

    #[test]
    fn quota_default_is_count_cap() {
        match QuotaPolicy::default_policy() {
            QuotaPolicy::CountCap { max_rows } => {
                assert_eq!(max_rows, QuotaPolicy::DEFAULT_MAX_ROWS);
            }
            _ => panic!("default should be a count cap"),
        }
    }

    #[test]
    fn sm_session_id_roundtrip() {
        let sid = SmSessionId::new("stream-abc-123");
        assert_eq!(sid.as_str(), "stream-abc-123");
        assert_eq!(sid.to_string(), "stream-abc-123");
    }
}
