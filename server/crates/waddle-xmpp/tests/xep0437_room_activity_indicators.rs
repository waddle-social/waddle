//! XEP-0437: Room Activity Indicators dedicated suite.
//!
//! The module implements the unread-tracking state machine behind
//! activity indicators (`UnreadTracker`); there is no `urn:xmpp:rai:0`
//! wire surface in this crate. These tests pin the tracker's
//! semantics: active-room suppression, read transitions, and
//! aggregate counts.

use waddle_xmpp::xep::UnreadTracker;

#[test]
fn xep0437_messages_accumulate_per_room() {
    let mut tracker = UnreadTracker::new();
    tracker.record_message("plays@muc.shakespeare.lit");
    tracker.record_message("plays@muc.shakespeare.lit");
    tracker.record_message("sonnets@muc.shakespeare.lit");

    assert_eq!(tracker.unread_count("plays@muc.shakespeare.lit"), 2);
    assert_eq!(tracker.unread_count("sonnets@muc.shakespeare.lit"), 1);
    assert_eq!(tracker.total_unread(), 3);
}

#[test]
fn xep0437_active_room_never_accumulates_unread() {
    let mut tracker = UnreadTracker::new();
    tracker.set_active_room(Some("plays@muc.shakespeare.lit".to_owned()));

    tracker.record_message("plays@muc.shakespeare.lit");
    tracker.record_message("sonnets@muc.shakespeare.lit");

    assert!(!tracker.has_unread("plays@muc.shakespeare.lit"));
    assert!(tracker.has_unread("sonnets@muc.shakespeare.lit"));
}

#[test]
fn xep0437_entering_a_room_clears_its_unread() {
    let mut tracker = UnreadTracker::new();
    tracker.record_message("plays@muc.shakespeare.lit");
    assert!(tracker.has_unread("plays@muc.shakespeare.lit"));

    tracker.set_active_room(Some("plays@muc.shakespeare.lit".to_owned()));
    assert!(!tracker.has_unread("plays@muc.shakespeare.lit"));
    assert_eq!(tracker.total_unread(), 0);
}

#[test]
fn xep0437_leaving_the_active_room_resumes_accumulation() {
    let mut tracker = UnreadTracker::new();
    tracker.set_active_room(Some("plays@muc.shakespeare.lit".to_owned()));
    tracker.record_message("plays@muc.shakespeare.lit");
    assert_eq!(tracker.unread_count("plays@muc.shakespeare.lit"), 0);

    // User switches away; new activity must count again.
    tracker.set_active_room(None);
    tracker.record_message("plays@muc.shakespeare.lit");
    assert_eq!(tracker.unread_count("plays@muc.shakespeare.lit"), 1);
}

#[test]
fn xep0437_switching_rooms_clears_only_the_new_active_room() {
    let mut tracker = UnreadTracker::new();
    tracker.record_message("a@muc.example.com");
    tracker.record_message("b@muc.example.com");

    tracker.set_active_room(Some("a@muc.example.com".to_owned()));

    assert!(!tracker.has_unread("a@muc.example.com"));
    assert!(tracker.has_unread("b@muc.example.com"));
    assert_eq!(tracker.total_unread(), 1);
}

#[test]
fn xep0437_mark_read_clears_a_single_room() {
    let mut tracker = UnreadTracker::new();
    tracker.record_message("a@muc.example.com");
    tracker.record_message("b@muc.example.com");

    tracker.mark_read("a@muc.example.com");

    assert!(!tracker.has_unread("a@muc.example.com"));
    assert_eq!(tracker.unread_count("b@muc.example.com"), 1);
}

#[test]
fn xep0437_mark_all_read_clears_everything() {
    let mut tracker = UnreadTracker::new();
    tracker.record_message("a@muc.example.com");
    tracker.record_message("b@muc.example.com");
    tracker.mark_all_read();

    assert_eq!(tracker.total_unread(), 0);
    assert!(tracker.unread_rooms().is_empty());
}

#[test]
fn xep0437_unread_rooms_lists_only_rooms_with_activity() {
    let mut tracker = UnreadTracker::new();
    tracker.record_message("a@muc.example.com");
    tracker.record_message("b@muc.example.com");
    tracker.mark_read("b@muc.example.com");

    let rooms = tracker.unread_rooms();
    assert!(rooms.contains("a@muc.example.com"));
    assert!(!rooms.contains("b@muc.example.com"));
    assert_eq!(rooms.len(), 1);
}

#[test]
fn xep0437_unknown_room_reports_zero_unread() {
    let tracker = UnreadTracker::new();
    assert!(!tracker.has_unread("nowhere@muc.example.com"));
    assert_eq!(tracker.unread_count("nowhere@muc.example.com"), 0);
    assert_eq!(tracker.total_unread(), 0);
}
