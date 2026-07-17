use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jid::{BareJid, Jid};
use waddle_xmpp_core::mam::{ArchivedMessage, MamQuery, MamResult};
use waddle_xmpp_core::xep0359::OriginId;

use crate::muc::RoomClaimFenceContext;

use super::MamStorageError;

/// The XEP-0313 archive context that defines query semantics.
///
/// A bare JID alone cannot distinguish a personal archive from a room
/// archive. Callers must supply the protocol context explicitly so the
/// owner-self `with` rule is never inferred from domain naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MamArchiveKind {
    Personal,
    Room,
}

/// Outcome of an archive write attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOutcome {
    /// A new row was inserted under this archive id.
    Stored(String),
    /// An existing live row matched the origin-id retry-dedupe; no row was
    /// written. Carries the existing row's archive id.
    Deduplicated(String),
    /// A tombstoned (XEP-0424 retracted) groupchat row matched the retry;
    /// no row was written and the caller must swallow the message entirely.
    TombstoneHit(String),
}

/// Outcome of an atomic terminal-preserving tombstone replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalTombstoneOutcome {
    /// The live archive row was replaced with the supplied tombstone.
    Replaced,
    /// The row already carried a tombstone and was left unchanged.
    AlreadyTombstoned,
    /// No archive row matched the requested primary key.
    NotFound,
}

/// Trait for MAM message storage backends.
///
/// Per XEP-0313 §4.1, archive addressing is normatively a **bare JID**
/// — the user's bare JID for personal archives, the room's bare JID
/// for MUC archives. Typing `archive_jid: &BareJid` (not `&str`) makes
/// that invariant load-bearing in the type system: a caller cannot
/// accidentally pass a full JID with a resource part and silently
/// land in the wrong archive bucket. Internal SQL bindings serialize
/// to `String` once at the bind site (the SQL boundary is the only
/// place untyped textual representation is allowed).
#[async_trait]
pub trait MamStorage: Send + Sync {
    /// Store a message in the archive.
    ///
    /// The `archive_jid` identifies which archive to store in:
    /// - For MUC messages: the room bare JID
    /// - For 1:1 messages: the user's bare JID (personal archive)
    ///
    /// Returns whether a new row was stored or an existing retry target was
    /// found, together with the relevant archive id.
    async fn store_message(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<StoreOutcome, MamStorageError>;

    /// Fenced variant of [`Self::store_message`] for the MUC groupchat
    /// archive write path (ADR-0017 Phase 3 Slice 7 FIX 1,
    /// council-adjudicated): the two-part demotion protocol's guaranteed
    /// backstop (element 7) runs the `SELECT ... FOR SHARE` fencing check
    /// INSIDE the same transaction as the archive insert itself, closing
    /// the residual race window between `dispatch_to_room`'s own
    /// standalone pre-fan-out check
    /// ([`crate::muc::MucDurableStore::check_fenced_fanout`]) and the
    /// later archive write — a steal landing in that narrow gap could
    /// otherwise still commit a phantom archived row under a claim this
    /// node no longer holds.
    ///
    /// `fence` carries the SAME typed `(Entity, ClaimEpoch, node_id)`
    /// context [`crate::muc::MucDurableStore::current_claim_fence`]
    /// resolves from `dispatch_to_room`'s own fencing mechanism — threaded
    /// through rather than re-derived, so both fencing checks (the
    /// standalone pre-fan-out one and this write-adjacent one) agree by
    /// construction.
    ///
    /// Default impl ignores `fence` and falls back to [`Self::store_message`]
    /// — correct for every implementation with no clustering/fencing
    /// concept (the portable, single-node backend, and the in-memory test
    /// double). A cluster-aware implementation overrides this to run the
    /// fencing check and the insert in one transaction, mirroring
    /// `pending_delivery::storage::PendingDeliveryStorage::insert_fenced`'s
    /// identical pattern one table over. On a failed fence, returns
    /// [`MamStorageError::NotOwner`] and the write never touches
    /// `mam_messages`.
    async fn store_message_fenced(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
        fence: &RoomClaimFenceContext,
    ) -> Result<StoreOutcome, MamStorageError> {
        let _ = fence;
        self.store_message(archive_jid, message).await
    }

    /// Query messages from the archive.
    ///
    /// The `archive_jid` identifies which archive to query:
    /// - For MUC archives: the room bare JID
    /// - For personal archives: the user's bare JID
    ///
    /// `archive_kind` is mandatory because XEP-0313 §4.3.1 gives
    /// owner-equivalent `with` queries special semantics only for personal
    /// archives. The JID itself is not sufficient to infer that context.
    ///
    /// Supports filtering by time range, sender, and RSM pagination.
    async fn query_messages(
        &self,
        archive_jid: &BareJid,
        archive_kind: MamArchiveKind,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError>;

    /// Get a single message by its archive ID.
    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Replace an archived message with a XEP-0424 / XEP-0425 tombstone in
    /// place. Clears `body`, `stanza_xml`, `thread` (id and optional
    /// parent), `reply` (id and optional sender JID), and overwrites
    /// `rich_payload` with the typed `ArchivedRichPayload::Tombstone(...)`
    /// value, per XEP-0424 §Tombstones / XEP-0425 §Tombstones: "any
    /// related elements which might leak information about the original
    /// message".
    ///
    /// Looks up the row by `archive_id` (the storage primary key). Returns
    /// `Ok(true)` when a row was found and updated, `Ok(false)` when no row
    /// matched, and `Err` on storage failure.
    async fn replace_with_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError>;

    /// Atomically replace a live row with a tombstone without overwriting an
    /// existing XEP-0424 / XEP-0425 tombstone. This is the terminal-state
    /// operation used by groupchat author-retraction heal retries, where a
    /// concurrent moderation tombstone must retain its attribution and reason.
    async fn replace_with_terminal_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<TerminalTombstoneOutcome, MamStorageError>;

    /// Get a single message by its original message/stanza id inside an archive.
    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Get a message by its wire message id inside an archive.
    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Resolve a sender-owned origin id, falling back to the same sender's
    /// wire stanza id for legacy XEP-0308 clients that omit `<origin-id/>`.
    /// Backends must perform this as one bounded lookup rather than paging an
    /// archive. Personal archives compare the sender by bare account JID;
    /// room archives compare the exact full occupant JID.
    async fn get_message_by_sender_and_origin_id(
        &self,
        archive_jid: &BareJid,
        archive_kind: MamArchiveKind,
        sender: &Jid,
        origin_id: &OriginId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Get a message by server archive id or stanza id, excluding client origin-id.
    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError>;

    /// Get the total count of messages in an archive (for RSM).
    async fn count_messages(&self, room_jid: &BareJid) -> Result<u32, MamStorageError>;

    /// Delete messages older than a given timestamp.
    ///
    /// Used for archive maintenance/cleanup.
    async fn delete_before(
        &self,
        room_jid: &BareJid,
        before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError>;
}
