//! XEP-0502: MUC Activity Indicator
//!
//! Local activity tracking support. Waddle does not currently emit the optional
//! disco#info activity field because there is no truthful messages/hour value
//! available at disco time.

use chrono::{DateTime, Utc};

/// Activity state for a room. This is a local model, not a XEP-0502 stanza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomActivity {
    /// The room JID.
    pub room_jid: String,
    /// Timestamp of last activity.
    pub last_activity: Option<DateTime<Utc>>,
    /// Whether the room currently has new messages.
    pub has_new_messages: bool,
}

impl RoomActivity {
    /// Create a new room activity entry.
    pub fn new(room_jid: impl Into<String>) -> Self {
        Self {
            room_jid: room_jid.into(),
            last_activity: None,
            has_new_messages: false,
        }
    }

    /// Mark as having new activity now.
    pub fn with_activity_now(mut self) -> Self {
        self.last_activity = Some(Utc::now());
        self.has_new_messages = true;
        self
    }

    /// Set the last activity timestamp.
    pub fn with_last_activity(mut self, ts: DateTime<Utc>) -> Self {
        self.last_activity = Some(ts);
        self.has_new_messages = true;
        self
    }

    /// Mark as read (no new messages).
    pub fn mark_read(&mut self) {
        self.has_new_messages = false;
    }
}

/// Tracks activity across multiple rooms locally.
#[derive(Debug, Default)]
pub struct ActivityTracker {
    rooms: std::collections::HashMap<String, RoomActivity>,
}

impl ActivityTracker {
    /// Create a new tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record activity in a room.
    pub fn record_activity(&mut self, room_jid: &str, timestamp: DateTime<Utc>) {
        let entry = self
            .rooms
            .entry(room_jid.to_owned())
            .or_insert_with(|| RoomActivity::new(room_jid));
        entry.last_activity = Some(timestamp);
        entry.has_new_messages = true;
    }

    /// Mark a room as read.
    pub fn mark_read(&mut self, room_jid: &str) {
        if let Some(entry) = self.rooms.get_mut(room_jid) {
            entry.mark_read();
        }
    }

    /// Check if a room has new messages.
    pub fn has_activity(&self, room_jid: &str) -> bool {
        self.rooms.get(room_jid).is_some_and(|r| r.has_new_messages)
    }

    /// Get all rooms with new activity.
    pub fn active_rooms(&self) -> Vec<&RoomActivity> {
        self.rooms.values().filter(|r| r.has_new_messages).collect()
    }

    /// Get the activity state for a room.
    pub fn get(&self, room_jid: &str) -> Option<&RoomActivity> {
        self.rooms.get(room_jid)
    }

    /// Number of rooms with new activity.
    pub fn active_count(&self) -> usize {
        self.rooms.values().filter(|r| r.has_new_messages).count()
    }

    /// Remove a room from tracking.
    pub fn remove(&mut self, room_jid: &str) {
        self.rooms.remove(room_jid);
    }

    /// Clear all activity.
    pub fn clear(&mut self) {
        self.rooms.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn test_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
            .single()
            .expect("valid test date")
    }

    #[test]
    fn test_room_activity_new() {
        let ra = RoomActivity::new("room@muc");
        assert_eq!(ra.room_jid, "room@muc");
        assert!(!ra.has_new_messages);
        assert_eq!(ra.last_activity, None);
    }

    #[test]
    fn test_room_activity_with_activity() {
        let ra = RoomActivity::new("room@muc").with_last_activity(test_time());
        assert!(ra.has_new_messages);
        assert_eq!(ra.last_activity, Some(test_time()));
    }

    #[test]
    fn test_room_activity_mark_read() {
        let mut ra = RoomActivity::new("room@muc").with_activity_now();
        assert!(ra.has_new_messages);
        ra.mark_read();
        assert!(!ra.has_new_messages);
    }

    #[test]
    fn test_activity_tracker() {
        let mut tracker = ActivityTracker::new();

        assert!(!tracker.has_activity("room1@muc"));
        assert_eq!(tracker.active_count(), 0);

        tracker.record_activity("room1@muc", test_time());
        tracker.record_activity("room2@muc", test_time());

        assert!(tracker.has_activity("room1@muc"));
        assert_eq!(tracker.active_count(), 2);
        assert_eq!(tracker.active_rooms().len(), 2);

        tracker.mark_read("room1@muc");
        assert!(!tracker.has_activity("room1@muc"));
        assert!(tracker.has_activity("room2@muc"));
        assert_eq!(tracker.active_count(), 1);

        tracker.remove("room2@muc");
        assert_eq!(tracker.active_count(), 0);

        tracker.clear();
        assert_eq!(tracker.active_count(), 0);
    }
}
