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

use chrono::{DateTime, Utc};
use jid::BareJid;
use waddle_xmpp_core::xep0359::StanzaId;
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

/// Exact identities deleted by a pending-delivery tombstone scrub.
///
/// `removed_count` preserves the existing count-returning contract while
/// `row_ids` lets callers capture the exact rows deleted when an
/// implementation can provide them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneScrubbedPendingRows {
    pub removed_count: u64,
    pub row_ids: Vec<PendingRowId>,
}

impl TombstoneScrubbedPendingRows {
    pub fn count_only(removed_count: u64) -> Self {
        Self {
            removed_count,
            row_ids: Vec::new(),
        }
    }
}

/// Wire length bound on [`SmSessionId`] (council-adjudicated FIX 7).
/// Production stream ids are server-generated UUIDs (36 bytes); 128 bytes
/// is a generous cap well above that, rejected only at
/// [`serde::Deserialize`] — the relay message carrying this type
/// (`waddle-server::clustering::relay::RelayResumeSteal`) is NOT a stanza,
/// so it never passes through the bounded XML stanza codec
/// (`waddle-server::clustering::codec`, `MAX_REMOTE_XML_BYTES` et al.) —
/// without a field-level bound of its own, a malicious (or buggy)
/// allowlisted peer could ship a multi-MB id, and every non-stanza relay
/// message needs its own such bound rather than relying on the stanza
/// codec's.
pub const SM_SESSION_ID_MAX_LEN: usize = 128;

/// [`SmSessionId`] exceeded [`SM_SESSION_ID_MAX_LEN`] at deserialization.
#[derive(Debug, Clone, thiserror::Error)]
#[error("SmSessionId of {len} bytes exceeds the {SM_SESSION_ID_MAX_LEN}-byte wire bound")]
pub struct SmSessionIdTooLong {
    pub len: usize,
}

/// Opaque XEP-0198 stream-id, identifying a single SM session.
///
/// Two SM sessions for the same user have different `SmSessionId`s.
/// Reusing the existing `stream_id: String` shape from
/// [`crate::stream_management::session_registry::DetachedSession`] —
/// kept as a newtype here so a session id cannot be silently swapped
/// for another opaque string at the storage boundary.
/// `Serialize`/`Deserialize` (ADR-0017 Phase 3 Slice 6): carried as a typed
/// field on the cross-node resume-handshake relay message
/// (`waddle-server::clustering::relay::RelayResumeSteal`) — `#[serde(transparent)]`
/// so the wire representation is the bare string, matching every other
/// stream-id-shaped value on the wire, while every in-process consumer still
/// only ever sees the typed newtype. `Deserialize` is hand-written (below),
/// not derived, so it can enforce [`SM_SESSION_ID_MAX_LEN`] — see
/// [`SM_SESSION_ID_MAX_LEN`]'s own doc comment for why. [`Self::new`] is for
/// server-minted identifiers; untrusted wire values must enter through
/// [`Self::try_from_wire`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(transparent)]
pub struct SmSessionId(String);

impl SmSessionId {
    /// Wrap a server-minted stream-id value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Validate and wrap an untrusted stream-id received from the wire.
    pub fn try_from_wire(value: impl Into<String>) -> Result<Self, SmSessionIdTooLong> {
        let value = value.into();
        if value.len() > SM_SESSION_ID_MAX_LEN {
            return Err(SmSessionIdTooLong { len: value.len() });
        }
        Ok(Self(value))
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

impl<'de> serde::Deserialize<'de> for SmSessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from_wire(value).map_err(serde::de::Error::custom)
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
    /// by the canonical [`StanzaId`] (`{ id, by }`) and pushes that.
    Archived(StanzaId),
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
    /// XEP-0198 outbound counter value assigned to this row's flush
    /// stanza when it was pushed onto the recovering session's
    /// outbound queue (Q7b). `None` until [`storage::PendingDeliveryStorage::record_pushed_at`]
    /// stamps it post-`record_outbound`. The SM `<a h='N'/>` ack
    /// handler uses this to range-delete only rows whose flush stanza
    /// was actually acknowledged: `flushed_in_session = session AND
    /// outbound_sequence IS NOT NULL AND outbound_sequence <= N`.
    pub outbound_sequence: Option<u32>,
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
        let archive_jid: jid::Jid = "alice@example.com".parse().unwrap();
        let archived = PendingPayload::Archived(StanzaId::new("opaque-id", archive_jid));
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

    /// Council-adjudicated FIX 7: the wire-deserialize bound.
    #[test]
    fn sm_session_id_deserialize_accepts_the_boundary_length() {
        let value = "a".repeat(SM_SESSION_ID_MAX_LEN);
        let json = serde_json::to_string(&value).expect("serialize the raw string");
        let sid: SmSessionId =
            serde_json::from_str(&json).expect("exactly at the cap must deserialize");
        assert_eq!(sid.as_str().len(), SM_SESSION_ID_MAX_LEN);
    }

    #[test]
    fn sm_session_id_deserialize_rejects_one_byte_over_the_cap() {
        let value = "a".repeat(SM_SESSION_ID_MAX_LEN + 1);
        let json = serde_json::to_string(&value).expect("serialize the raw string");
        let error = serde_json::from_str::<SmSessionId>(&json)
            .expect_err("one byte over the cap must be rejected");
        assert!(
            error.to_string().contains("exceeds"),
            "unexpected error message: {error}"
        );
    }

    #[test]
    fn sm_session_id_wire_constructor_rejects_one_byte_over_the_cap() {
        let value = "a".repeat(SM_SESSION_ID_MAX_LEN + 1);
        let error = SmSessionId::try_from_wire(value)
            .expect_err("untrusted wire ids must enforce the same bound as serde");
        assert_eq!(error.len, SM_SESSION_ID_MAX_LEN + 1);
    }

    #[test]
    fn sm_session_id_deserialize_rejects_a_malicious_multi_kb_id() {
        // Stands in for "a malicious allowlisted peer ships a multi-MB id" —
        // a few KB is already far past any real server-generated UUID and
        // well past the cap.
        let value = "x".repeat(64 * 1024);
        let json = serde_json::to_string(&value).expect("serialize the raw string");
        assert!(serde_json::from_str::<SmSessionId>(&json).is_err());
    }
}
