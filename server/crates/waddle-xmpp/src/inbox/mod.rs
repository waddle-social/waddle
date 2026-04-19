//! Unified inbox — "conversation list with unread counts".
//!
//! An *inbox entry* is a per-user materialised view of the last-message of
//! every conversation the user is part of (1:1 and MUC room), plus an
//! unread counter and timestamp. It gives clients a single query to paint
//! the chat list on app launch without having to fan out to MAM per room.
//!
//! Entries are keyed by [`InboxKey`] — a `(partner, thread_id)` tuple — so
//! that both channel-level and thread-level conversations share the same
//! inbox infrastructure.
//!
//! This module defines the typed in-process model. The protocol wrapper
//! lives in [`crate::xep::xep0430`]; the persistent projection lives in
//! [`storage`].

pub mod runtime;
pub mod storage;

use std::collections::HashMap;

use jid::BareJid;
use minidom::Element;
use serde::{Deserialize, Serialize};

/// Composite key identifying one inbox conversation.
///
/// For channel-level entries `thread_id` is `None`; for thread-level
/// entries it carries the RFC 6121 thread identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InboxKey {
    /// The conversation partner — either the other user (Direct) or the
    /// MUC room JID (MucRoom).
    pub partner: BareJid,
    /// When `Some`, this entry tracks a specific thread inside the room.
    pub thread_id: Option<String>,
}

impl InboxKey {
    /// Create a channel-level key (no thread).
    pub fn channel(partner: BareJid) -> Self {
        Self {
            partner,
            thread_id: None,
        }
    }

    /// Create a thread-level key.
    pub fn thread(partner: BareJid, thread_id: impl Into<String>) -> Self {
        Self {
            partner,
            thread_id: Some(thread_id.into()),
        }
    }

    /// Returns `true` when this key refers to a thread within a room.
    pub fn is_thread(&self) -> bool {
        self.thread_id.is_some()
    }
}

/// Trait for types that can be stored in the inbox.
///
/// Both channel-level and thread-level entries implement this, allowing
/// the storage and protocol layers to operate generically.
pub trait InboxMessage: Send + Sync + Clone {
    /// Composite key identifying this conversation.
    fn key(&self) -> InboxKey;

    /// Epoch seconds of the last observed message.
    fn timestamp(&self) -> i64;

    /// XEP-0359 stanza-id of the last message.
    fn stanza_id(&self) -> &str;

    /// Serialize to XML element for wire transmission.
    fn to_element(&self) -> Element;
}

/// Conversation shape — 1:1 DM or MUC room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConversationKind {
    /// A direct-message conversation with another bare JID.
    Direct,
    /// A MUC room at `<room>@conference.<domain>`.
    MucRoom,
}

/// One row in the inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEntry {
    /// The conversation partner — either the other user (Direct) or the
    /// MUC room JID (MucRoom).
    pub partner: BareJid,
    pub kind: ConversationKind,
    /// XEP-0359 stanza-id of the last observed message in the conversation.
    pub last_stanza_id: String,
    /// Epoch seconds of the last observed message.
    pub last_updated: i64,
    /// Unread messages since the user last read this conversation.
    pub unread: u32,
    /// Optional short preview the server materialised from the last message
    /// body (may be absent when privacy rules preclude storing body text).
    pub preview: Option<String>,
    /// RFC 6121 thread identifier — `None` for channel-level entries.
    pub thread_id: Option<String>,
    /// Thread title (XEP-0508 `thread-create` title or first-message preview).
    pub thread_title: Option<String>,
    /// Total replies in this thread.
    pub reply_count: u32,
    /// Nick of the thread starter.
    pub author: Option<String>,
}

impl InboxEntry {
    pub fn new(
        partner: BareJid,
        kind: ConversationKind,
        last_stanza_id: impl Into<String>,
        last_updated: i64,
    ) -> Self {
        Self {
            partner,
            kind,
            last_stanza_id: last_stanza_id.into(),
            last_updated,
            unread: 0,
            preview: None,
            thread_id: None,
            thread_title: None,
            reply_count: 0,
            author: None,
        }
    }

    pub fn with_unread(mut self, unread: u32) -> Self {
        self.unread = unread;
        self
    }

    pub fn with_preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = Some(preview.into());
        self
    }

    pub fn with_thread(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }

    pub fn with_thread_title(mut self, title: impl Into<String>) -> Self {
        self.thread_title = Some(title.into());
        self
    }

    pub fn with_reply_count(mut self, count: u32) -> Self {
        self.reply_count = count;
        self
    }

    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }
}

impl InboxMessage for InboxEntry {
    fn key(&self) -> InboxKey {
        InboxKey {
            partner: self.partner.clone(),
            thread_id: self.thread_id.clone(),
        }
    }

    fn timestamp(&self) -> i64 {
        self.last_updated
    }

    fn stanza_id(&self) -> &str {
        &self.last_stanza_id
    }

    fn to_element(&self) -> Element {
        crate::xep::xep0430::build_entry_element(self)
    }
}

/// In-memory inbox keyed by [`InboxKey`].
#[derive(Debug, Clone, Default)]
pub struct InboxView {
    entries: HashMap<InboxKey, InboxEntry>,
}

impl InboxView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a channel-level entry by partner JID.
    pub fn get(&self, partner: &BareJid) -> Option<&InboxEntry> {
        self.entries.get(&InboxKey::channel(partner.clone()))
    }

    /// Look up an entry by full key (partner + optional thread).
    pub fn get_by_key(&self, key: &InboxKey) -> Option<&InboxEntry> {
        self.entries.get(key)
    }

    /// Update the entry for the given key. When `increment_unread` is true the
    /// unread counter ticks by one; when false the entry is refreshed without
    /// touching the counter. For thread entries, `reply_count` is incremented.
    pub fn observe_message(&mut self, entry: InboxEntry, increment_unread: bool) -> InboxEntry {
        let key = entry.key();
        let is_thread = key.is_thread();
        let next = match self.entries.remove(&key) {
            Some(mut prev) => {
                prev.last_stanza_id = entry.last_stanza_id;
                prev.last_updated = entry.last_updated;
                prev.preview = entry.preview;
                if increment_unread {
                    prev.unread = prev.unread.saturating_add(1);
                }
                if is_thread {
                    prev.reply_count = prev.reply_count.saturating_add(1);
                }
                // Preserve existing title/author if not provided
                if entry.thread_title.is_some() {
                    prev.thread_title = entry.thread_title;
                }
                if entry.author.is_some() {
                    prev.author = entry.author;
                }
                prev
            }
            None => {
                let mut fresh = entry;
                fresh.unread = if increment_unread { 1 } else { 0 };
                fresh
            }
        };
        self.entries.insert(key, next.clone());
        next
    }

    /// Mark a channel-level conversation as read.
    pub fn mark_read(&mut self, partner: &BareJid) {
        self.mark_read_by_key(&InboxKey::channel(partner.clone()));
    }

    /// Mark a specific conversation (channel or thread) as read.
    pub fn mark_read_by_key(&mut self, key: &InboxKey) {
        if let Some(e) = self.entries.get_mut(key) {
            e.unread = 0;
        }
    }

    pub fn remove(&mut self, partner: &BareJid) -> Option<InboxEntry> {
        self.entries.remove(&InboxKey::channel(partner.clone()))
    }

    /// Returns a snapshot of channel-level entries sorted newest-first.
    pub fn snapshot(&self) -> Vec<InboxEntry> {
        let mut v: Vec<_> = self
            .entries
            .values()
            .filter(|e| e.thread_id.is_none())
            .cloned()
            .collect();
        v.sort_by(|a, b| b.last_updated.cmp(&a.last_updated));
        v
    }

    /// Returns thread-level entries for a specific room, sorted newest-first.
    pub fn threads_for_room(&self, room: &BareJid) -> Vec<InboxEntry> {
        let mut v: Vec<_> = self
            .entries
            .values()
            .filter(|e| e.thread_id.is_some() && &e.partner == room)
            .cloned()
            .collect();
        v.sort_by(|a, b| b.last_updated.cmp(&a.last_updated));
        v
    }

    /// Total unread across channel-level conversations (excludes thread entries).
    pub fn total_unread(&self) -> u64 {
        self.entries
            .values()
            .filter(|e| e.thread_id.is_none())
            .map(|e| e.unread as u64)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jid(s: &str) -> BareJid {
        s.parse().unwrap()
    }

    fn entry(partner: &str, kind: ConversationKind, id: &str, ts: i64) -> InboxEntry {
        InboxEntry::new(jid(partner), kind, id, ts)
    }

    #[test]
    fn test_observe_new_and_update() {
        let mut inbox = InboxView::new();
        inbox.observe_message(
            entry("alice@example.com", ConversationKind::Direct, "sid-1", 100).with_preview("hi"),
            true,
        );
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox.total_unread(), 1);
        inbox.observe_message(
            entry("alice@example.com", ConversationKind::Direct, "sid-2", 200).with_preview("!"),
            true,
        );
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox.total_unread(), 2);
        let e = inbox.get(&jid("alice@example.com")).unwrap();
        assert_eq!(e.last_stanza_id, "sid-2");
        assert_eq!(e.preview.as_deref(), Some("!"));
    }

    #[test]
    fn test_mark_read_resets_only_that_partner() {
        let mut inbox = InboxView::new();
        inbox.observe_message(
            entry("a@example.com", ConversationKind::Direct, "s1", 1),
            true,
        );
        inbox.observe_message(
            entry("b@example.com", ConversationKind::Direct, "s2", 2),
            true,
        );
        inbox.mark_read(&jid("a@example.com"));
        assert_eq!(inbox.get(&jid("a@example.com")).unwrap().unread, 0);
        assert_eq!(inbox.get(&jid("b@example.com")).unwrap().unread, 1);
        assert_eq!(inbox.total_unread(), 1);
    }

    #[test]
    fn test_snapshot_sorted_newest_first() {
        let mut inbox = InboxView::new();
        inbox.observe_message(
            entry("a@example.com", ConversationKind::Direct, "s1", 10),
            false,
        );
        inbox.observe_message(
            entry("b@example.com", ConversationKind::Direct, "s2", 30),
            false,
        );
        inbox.observe_message(
            entry(
                "g@conference.example.com",
                ConversationKind::MucRoom,
                "s3",
                20,
            ),
            false,
        );
        let snap = inbox.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].last_stanza_id, "s2");
        assert_eq!(snap[1].last_stanza_id, "s3");
        assert_eq!(snap[2].last_stanza_id, "s1");
    }

    #[test]
    fn test_observe_without_increment_leaves_unread_alone() {
        let mut inbox = InboxView::new();
        inbox.observe_message(
            entry("a@example.com", ConversationKind::Direct, "s1", 1),
            true,
        );
        assert_eq!(inbox.total_unread(), 1);
        inbox.observe_message(
            entry("a@example.com", ConversationKind::Direct, "s2", 2),
            false,
        );
        assert_eq!(inbox.total_unread(), 1);
    }

    #[test]
    fn test_remove() {
        let mut inbox = InboxView::new();
        inbox.observe_message(
            entry("a@example.com", ConversationKind::Direct, "s1", 1),
            false,
        );
        assert!(inbox.remove(&jid("a@example.com")).is_some());
        assert!(inbox.is_empty());
    }

    #[test]
    fn test_thread_entry_separate_from_channel() {
        let mut inbox = InboxView::new();
        let room = "room@muc.example.com";

        // Channel-level entry
        inbox.observe_message(entry(room, ConversationKind::MucRoom, "s1", 100), true);

        // Thread-level entry
        inbox.observe_message(
            entry(room, ConversationKind::MucRoom, "s2", 200)
                .with_thread("thread-1")
                .with_thread_title("Discussion")
                .with_author("alice"),
            true,
        );

        assert_eq!(inbox.len(), 2);
        // Channel unread excludes threads
        assert_eq!(inbox.total_unread(), 1);

        let channel = inbox.get(&jid(room)).unwrap();
        assert!(channel.thread_id.is_none());

        let threads = inbox.threads_for_room(&jid(room));
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].thread_id.as_deref(), Some("thread-1"));
        assert_eq!(threads[0].thread_title.as_deref(), Some("Discussion"));
        assert_eq!(threads[0].unread, 1);
    }

    #[test]
    fn test_thread_reply_count_increments() {
        let mut inbox = InboxView::new();
        let room = "room@muc.example.com";

        inbox.observe_message(
            entry(room, ConversationKind::MucRoom, "s1", 100)
                .with_thread("t1")
                .with_thread_title("Topic"),
            true,
        );
        inbox.observe_message(
            entry(room, ConversationKind::MucRoom, "s2", 200).with_thread("t1"),
            true,
        );

        let key = InboxKey::thread(jid(room), "t1");
        let e = inbox.get_by_key(&key).unwrap();
        assert_eq!(e.reply_count, 1);
        assert_eq!(e.unread, 2);
        // Title preserved from first message
        assert_eq!(e.thread_title.as_deref(), Some("Topic"));
    }

    #[test]
    fn test_mark_read_by_key_for_thread() {
        let mut inbox = InboxView::new();
        let room = "room@muc.example.com";

        inbox.observe_message(
            entry(room, ConversationKind::MucRoom, "s1", 100).with_thread("t1"),
            true,
        );
        inbox.observe_message(
            entry(room, ConversationKind::MucRoom, "s2", 200).with_thread("t2"),
            true,
        );

        let key = InboxKey::thread(jid(room), "t1");
        inbox.mark_read_by_key(&key);

        let t1 = inbox.get_by_key(&key).unwrap();
        assert_eq!(t1.unread, 0);

        let t2 = inbox
            .get_by_key(&InboxKey::thread(jid(room), "t2"))
            .unwrap();
        assert_eq!(t2.unread, 1);
    }

    #[test]
    fn test_snapshot_excludes_thread_entries() {
        let mut inbox = InboxView::new();
        inbox.observe_message(
            entry("room@muc.example.com", ConversationKind::MucRoom, "s1", 10),
            false,
        );
        inbox.observe_message(
            entry("room@muc.example.com", ConversationKind::MucRoom, "s2", 20).with_thread("t1"),
            false,
        );

        let snap = inbox.snapshot();
        assert_eq!(snap.len(), 1);
        assert!(snap[0].thread_id.is_none());
    }

    #[test]
    fn test_inbox_key_equality() {
        let k1 = InboxKey::channel(jid("a@example.com"));
        let k2 = InboxKey::channel(jid("a@example.com"));
        let k3 = InboxKey::thread(jid("a@example.com"), "t1");

        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
        assert!(k3.is_thread());
        assert!(!k1.is_thread());
    }
}
