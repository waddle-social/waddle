use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jid::BareJid;
use waddle_xmpp_core::mam::{ArchivedMessage, MamQuery, MamResult};

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
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError>;

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
