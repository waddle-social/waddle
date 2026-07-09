use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jid::{BareJid, Jid};
use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedTombstone, MamQuery, MamResult};

use crate::muc::RoomClaimFenceContext;

use super::MamStorageError;

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
    /// Returns the unique archive ID assigned to the message.
    async fn store_message(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<String, MamStorageError>;

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
    /// `fence` carries the SAME typed `(Entity, ClaimEpoch, NodeIdentity)`
    /// context [`crate::muc::MucDurableStore::current_claim_fence`]
    /// resolves from `dispatch_to_room`'s own fencing mechanism — threaded
    /// through rather than re-derived, so both fencing checks (the
    /// standalone pre-fan-out one and this write-adjacent one) agree by
    /// construction.
    ///
    /// The default implementation fails with
    /// [`MamStorageError::FencingUnavailable`]. Single-node callers use
    /// [`Self::store_message`] directly and never enter this method; once a
    /// clustered caller supplies a fence, silently falling back to an
    /// unfenced insert would violate the ownership boundary. A
    /// cluster-aware implementation overrides this to run the fencing
    /// check and the insert in one transaction, mirroring
    /// `pending_delivery::storage::PendingDeliveryStorage::insert_fenced`'s
    /// identical pattern one table over. On a failed fence, returns
    /// [`MamStorageError::NotOwner`] and the write never touches
    /// `mam_messages`.
    async fn store_message_fenced(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
        fence: &RoomClaimFenceContext,
    ) -> Result<String, MamStorageError> {
        let _ = (archive_jid, message);
        Err(MamStorageError::FencingUnavailable {
            entity: fence.entity.clone(),
        })
    }

    /// Query messages from the archive.
    ///
    /// The `archive_jid` identifies which archive to query:
    /// - For MUC archives: the room bare JID
    /// - For personal archives: the user's bare JID
    ///
    /// Supports filtering by time range, sender, and RSM pagination.
    async fn query_messages(
        &self,
        archive_jid: &BareJid,
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
        tombstone: ArchivedTombstone,
    ) -> Result<bool, MamStorageError>;

    /// Atomically archive a canonical XEP-0425 moderation event and replace
    /// its exact room-scoped target with a tombstone under the same room
    /// ownership fence. `Ok(false)` means the target did not exist in this
    /// room; neither write is committed.
    async fn moderate_message_fenced(
        &self,
        archive_jid: &BareJid,
        moderation: &ArchivedMessage,
        target_archive_id: &str,
        tombstone: ArchivedTombstone,
        fence: &RoomClaimFenceContext,
    ) -> Result<bool, MamStorageError> {
        let _ = (archive_jid, moderation, target_archive_id, tombstone);
        Err(MamStorageError::FencingUnavailable {
            entity: fence.entity.clone(),
        })
    }

    /// Replace an exact room-scoped XEP-0424 target only while the exact
    /// room claim is still held and the matching retraction event has already
    /// been archived in that room by the authenticated occupant sender.
    async fn replace_with_tombstone_fenced(
        &self,
        archive_jid: &BareJid,
        target_archive_id: &str,
        retraction_archive_id: &str,
        retraction_from: &Jid,
        tombstone: ArchivedTombstone,
        fence: &RoomClaimFenceContext,
    ) -> Result<bool, MamStorageError> {
        let _ = (
            archive_jid,
            target_archive_id,
            retraction_archive_id,
            retraction_from,
            tombstone,
        );
        Err(MamStorageError::FencingUnavailable {
            entity: fence.entity.clone(),
        })
    }

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
