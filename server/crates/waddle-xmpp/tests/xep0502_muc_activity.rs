//! XEP-0502: MUC Activity Indicator dedicated suite.
//!
//! Waddle does not currently have a truthful messages/hour value at disco time,
//! so it must not emit the optional roominfo field. This suite keeps coverage
//! for the local tracker model only.

use chrono::{DateTime, TimeZone, Utc};
use waddle_xmpp::xep::xep0502::{ActivityTracker, RoomActivity};

fn test_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
        .single()
        .expect("valid test date")
}

#[test]
fn xep0502_room_activity_read_state_transitions() {
    let mut ra = RoomActivity::new("room@muc");
    assert!(!ra.has_new_messages);
    assert_eq!(ra.last_activity, None);

    ra = ra.with_last_activity(test_time());
    assert!(ra.has_new_messages);

    ra.mark_read();
    assert!(!ra.has_new_messages);
    assert_eq!(ra.last_activity, Some(test_time()));
}

#[test]
fn xep0502_tracker_counts_active_rooms_and_marks_read() {
    let mut tracker = ActivityTracker::new();
    tracker.record_activity("room1@muc", test_time());
    tracker.record_activity("room2@muc", test_time());

    assert!(tracker.has_activity("room1@muc"));
    assert_eq!(tracker.active_count(), 2);
    assert_eq!(tracker.active_rooms().len(), 2);

    tracker.mark_read("room1@muc");
    assert!(!tracker.has_activity("room1@muc"));
    assert_eq!(tracker.active_count(), 1);
    assert_eq!(
        tracker.get("room1@muc").and_then(|room| room.last_activity),
        Some(test_time())
    );
}

#[test]
fn xep0502_tracker_reactivates_read_room_on_new_activity() {
    let mut tracker = ActivityTracker::new();
    tracker.record_activity("room@muc", test_time());
    tracker.mark_read("room@muc");

    let later = Utc
        .with_ymd_and_hms(2024, 6, 1, 13, 0, 0)
        .single()
        .expect("valid date");
    tracker.record_activity("room@muc", later);

    assert!(tracker.has_activity("room@muc"));
    assert_eq!(
        tracker.get("room@muc").and_then(|room| room.last_activity),
        Some(later)
    );
}

#[test]
fn xep0502_tracker_remove_and_clear() {
    let mut tracker = ActivityTracker::new();
    tracker.record_activity("a@muc", test_time());
    tracker.record_activity("b@muc", test_time());

    tracker.remove("a@muc");
    assert!(tracker.get("a@muc").is_none());
    assert_eq!(tracker.active_count(), 1);

    tracker.clear();
    assert_eq!(tracker.active_count(), 0);
    assert!(tracker.active_rooms().is_empty());
}
