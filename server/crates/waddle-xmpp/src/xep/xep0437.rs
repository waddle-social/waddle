//! XEP-0437: Room Activity Indicators
//!
//! Lightweight indicators showing which MUC rooms have new messages.
//! Enables unread badges in chat sidebars without requiring full
//! message history queries.

use std::collections::{HashMap, HashSet};

/// Tracks room activity for unread indicators.
#[derive(Debug, Default)]
pub struct UnreadTracker {
    unread: HashMap<String, u32>,
    active_room: Option<String>,
}

impl UnreadTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_active_room(&mut self, room_jid: Option<String>) {
        if let Some(ref jid) = room_jid {
            self.unread.remove(jid);
        }
        self.active_room = room_jid;
    }

    pub fn record_message(&mut self, room_jid: &str) {
        if self.active_room.as_deref() == Some(room_jid) {
            return;
        }
        *self.unread.entry(room_jid.to_owned()).or_insert(0) += 1;
    }

    pub fn mark_read(&mut self, room_jid: &str) {
        self.unread.remove(room_jid);
    }

    pub fn mark_all_read(&mut self) {
        self.unread.clear();
    }

    pub fn has_unread(&self, room_jid: &str) -> bool {
        self.unread.get(room_jid).is_some_and(|&c| c > 0)
    }

    pub fn unread_count(&self, room_jid: &str) -> u32 {
        self.unread.get(room_jid).copied().unwrap_or(0)
    }

    pub fn unread_rooms(&self) -> HashSet<&str> {
        self.unread
            .iter()
            .filter(|(_, &count)| count > 0)
            .map(|(jid, _)| jid.as_str())
            .collect()
    }

    pub fn total_unread(&self) -> u32 {
        self.unread.values().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_count() {
        let mut t = UnreadTracker::new();
        t.record_message("r1@muc");
        t.record_message("r1@muc");
        t.record_message("r2@muc");
        assert_eq!(t.unread_count("r1@muc"), 2);
        assert_eq!(t.total_unread(), 3);
    }

    #[test]
    fn test_active_room_suppressed() {
        let mut t = UnreadTracker::new();
        t.set_active_room(Some("r1@muc".into()));
        t.record_message("r1@muc");
        assert!(!t.has_unread("r1@muc"));
    }

    #[test]
    fn test_mark_read() {
        let mut t = UnreadTracker::new();
        t.record_message("r1@muc");
        t.mark_read("r1@muc");
        assert!(!t.has_unread("r1@muc"));
    }

    #[test]
    fn test_set_active_clears() {
        let mut t = UnreadTracker::new();
        t.record_message("r1@muc");
        t.set_active_room(Some("r1@muc".into()));
        assert!(!t.has_unread("r1@muc"));
    }

    #[test]
    fn test_unread_rooms() {
        let mut t = UnreadTracker::new();
        t.record_message("a@muc");
        t.record_message("b@muc");
        assert_eq!(t.unread_rooms().len(), 2);
    }

    #[test]
    fn test_mark_all_read() {
        let mut t = UnreadTracker::new();
        t.record_message("a@muc");
        t.record_message("b@muc");
        t.mark_all_read();
        assert_eq!(t.total_unread(), 0);
    }
}
