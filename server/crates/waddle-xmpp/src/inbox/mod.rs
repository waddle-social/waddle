//! Unified inbox — "conversation list with unread counts".
//!
//! An *inbox entry* is a per-user materialised view of the last-message of
//! every conversation the user is part of (1:1 and MIX-channel), plus an
//! unread counter and timestamp. It gives clients a single query to paint
//! the chat list on app launch without having to fan out to MAM per room.
//!
//! This module defines the typed in-process model. The protocol wrapper
//! lives in [`crate::xep::xep0430`]; the persistent projection lives in
//! [`storage`].

pub mod storage;

use std::collections::HashMap;

use jid::BareJid;
use serde::{Deserialize, Serialize};

/// Conversation shape — 1:1 DM or MIX channel. MUC is intentionally not a
/// variant: the plan migrates all group conversations to MIX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConversationKind {
    /// A direct-message conversation with another bare JID.
    Direct,
    /// A MIX channel at `<channel>@mix.<domain>`.
    MixChannel,
}

/// One row in the inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEntry {
    /// The conversation partner — either the other user (Direct) or the
    /// MIX channel JID (MixChannel).
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
}

/// In-memory inbox — useful for tests and as the canonical API shape of
/// the persistent store.
#[derive(Debug, Clone, Default)]
pub struct InboxView {
    by_partner: HashMap<BareJid, InboxEntry>,
}

impl InboxView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.by_partner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_partner.is_empty()
    }

    pub fn get(&self, partner: &BareJid) -> Option<&InboxEntry> {
        self.by_partner.get(partner)
    }

    /// Update the entry for `partner`. When `increment_unread` is true the
    /// unread counter ticks by one (for a fresh entry, that means the first
    /// observed message counts as one unread); when false the entry is
    /// refreshed without touching the counter.
    pub fn observe_message(&mut self, entry: InboxEntry, increment_unread: bool) {
        let partner = entry.partner.clone();
        let next = match self.by_partner.remove(&partner) {
            Some(mut prev) => {
                prev.last_stanza_id = entry.last_stanza_id;
                prev.last_updated = entry.last_updated;
                prev.preview = entry.preview;
                if increment_unread {
                    prev.unread = prev.unread.saturating_add(1);
                }
                prev
            }
            None => {
                let mut fresh = entry;
                fresh.unread = if increment_unread { 1 } else { 0 };
                fresh
            }
        };
        self.by_partner.insert(partner, next);
    }

    pub fn mark_read(&mut self, partner: &BareJid) {
        if let Some(e) = self.by_partner.get_mut(partner) {
            e.unread = 0;
        }
    }

    pub fn remove(&mut self, partner: &BareJid) -> Option<InboxEntry> {
        self.by_partner.remove(partner)
    }

    /// Returns a snapshot sorted newest-first.
    pub fn snapshot(&self) -> Vec<InboxEntry> {
        let mut v: Vec<_> = self.by_partner.values().cloned().collect();
        v.sort_by(|a, b| b.last_updated.cmp(&a.last_updated));
        v
    }

    /// Total unread across every conversation.
    pub fn total_unread(&self) -> u64 {
        self.by_partner.values().map(|e| e.unread as u64).sum()
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
            entry("g@mix.example.com", ConversationKind::MixChannel, "s3", 20),
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
}
