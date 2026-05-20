//! XEP-0502: MUC Activity Indicator
//!
//! Lightweight activity tracking for MUC rooms. Clients can subscribe
//! to activity indicators without joining rooms, enabling sidebar badges
//! showing which rooms have new messages.
//!
//! ## XML Format
//!
//! Subscribe to activity for a room:
//! ```xml
//! <iq type='set' to='muc.example.com' id='act-1'>
//!   <activity xmlns='urn:xmpp:muc-activity:0'>
//!     <subscribe jid='room@muc.example.com'/>
//!   </activity>
//! </iq>
//! ```
//!
//! Activity notification from server:
//! ```xml
//! <message from='muc.example.com' to='user@example.com'>
//!   <activity xmlns='urn:xmpp:muc-activity:0'>
//!     <active jid='room@muc.example.com' last-activity='2024-06-01T12:00:00Z'/>
//!   </activity>
//! </message>
//! ```
//!
//! ## Use Cases
//!
//! - Show activity dots on rooms in the sidebar
//! - Track new messages in rooms the user hasn't joined
//! - Lightweight alternative to full room presence

use chrono::{DateTime, Utc};
use minidom::Element;

/// Namespace for XEP-0502 MUC Activity Indicator.
pub const NS_MUC_ACTIVITY: &str = "urn:xmpp:muc-activity:0";

/// Activity state for a room.
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

/// Tracks activity across multiple rooms.
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

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is an `<activity/>` element.
pub fn is_activity_element(elem: &Element) -> bool {
    elem.ns() == NS_MUC_ACTIVITY && elem.name() == "activity"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse activity notifications from an `<activity/>` element.
pub fn parse_activity_notifications(elem: &Element) -> Vec<RoomActivity> {
    if !is_activity_element(elem) {
        return Vec::new();
    }

    elem.children()
        .filter(|c| c.name() == "active" && c.ns() == NS_MUC_ACTIVITY)
        .filter_map(|c| {
            let jid = c.attr("jid").filter(|s| !s.is_empty())?.to_owned();
            let ts = c
                .attr("last-activity")
                .and_then(|s| s.parse::<DateTime<Utc>>().ok());
            Some(RoomActivity {
                room_jid: jid,
                last_activity: ts,
                has_new_messages: true,
            })
        })
        .collect()
}

// ── Building ─────────────────────────────────────────────────────────

/// Build an `<activity/>` notification element.
pub fn build_activity_notification(activities: &[&RoomActivity]) -> Element {
    let mut activity = Element::builder("activity", NS_MUC_ACTIVITY).build();

    for ra in activities {
        let mut active = Element::builder("active", NS_MUC_ACTIVITY)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                ra.room_jid.as_str(),
            )
            .build();
        if let Some(ts) = ra.last_activity {
            active.set_attr(
                minidom::rxml::Namespace::NONE,
                minidom::rxml::xml_ncname!("last-activity").to_owned(),
                ts.to_rfc3339(),
            );
        }
        activity.append_child(active);
    }

    activity
}

/// Build a subscribe request element.
pub fn build_subscribe_element(room_jids: &[&str]) -> Element {
    let mut activity = Element::builder("activity", NS_MUC_ACTIVITY).build();
    for jid in room_jids {
        let sub = Element::builder("subscribe", NS_MUC_ACTIVITY)
            .attr(minidom::rxml::xml_ncname!("jid").to_owned(), *jid)
            .build();
        activity.append_child(sub);
    }
    activity
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
    fn test_is_activity_element() {
        let elem = Element::builder("activity", NS_MUC_ACTIVITY).build();
        assert!(is_activity_element(&elem));

        let wrong = Element::builder("activity", "jabber:client").build();
        assert!(!is_activity_element(&wrong));
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
        assert!(tracker.has_activity("room2@muc"));
        assert_eq!(tracker.active_count(), 2);
        assert_eq!(tracker.active_rooms().len(), 2);

        tracker.mark_read("room1@muc");
        assert!(!tracker.has_activity("room1@muc"));
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn test_tracker_remove() {
        let mut tracker = ActivityTracker::new();
        tracker.record_activity("room@muc", test_time());
        tracker.remove("room@muc");
        assert!(tracker.get("room@muc").is_none());
    }

    #[test]
    fn test_tracker_clear() {
        let mut tracker = ActivityTracker::new();
        tracker.record_activity("a@muc", test_time());
        tracker.record_activity("b@muc", test_time());
        tracker.clear();
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn test_build_and_parse_notifications() {
        let ra1 = RoomActivity::new("room1@muc").with_last_activity(test_time());
        let ra2 = RoomActivity::new("room2@muc").with_activity_now();

        let elem = build_activity_notification(&[&ra1, &ra2]);
        assert_eq!(elem.name(), "activity");
        assert_eq!(elem.ns(), NS_MUC_ACTIVITY);

        let parsed = parse_activity_notifications(&elem);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].room_jid, "room1@muc");
        assert!(parsed[0].has_new_messages);
        assert_eq!(parsed[1].room_jid, "room2@muc");
    }

    #[test]
    fn test_parse_empty() {
        let elem = Element::builder("activity", NS_MUC_ACTIVITY).build();
        let parsed = parse_activity_notifications(&elem);
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_parse_wrong_element() {
        let elem = Element::builder("other", NS_MUC_ACTIVITY).build();
        let parsed = parse_activity_notifications(&elem);
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_build_subscribe() {
        let elem = build_subscribe_element(&["room1@muc", "room2@muc"]);
        assert_eq!(elem.children().count(), 2);
        let first = elem.children().next().expect("first child");
        assert_eq!(first.name(), "subscribe");
        assert_eq!(first.attr("jid"), Some("room1@muc"));
    }
}
