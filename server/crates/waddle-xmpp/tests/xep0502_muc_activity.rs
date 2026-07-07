//! XEP-0502: MUC Activity Indicator dedicated suite.
//!
//! Pins the conformant disco#info extension field
//! `{urn:xmpp:muc-activity}message-activity` and the local tracker model used
//! to maintain activity state.

use chrono::{DateTime, TimeZone, Utc};
use minidom::Element;
use waddle_xmpp::xep::xep0004::NS_DATA_FORMS;
use waddle_xmpp::xep::xep0502::{
    build_message_activity_field, build_muc_activity_roominfo_form, parse_message_activity_field,
    ActivityTracker, RoomActivity, FIELD_MESSAGE_ACTIVITY, NS_MUC_ACTIVITY,
};

fn test_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
        .single()
        .expect("valid test date")
}

#[test]
fn xep0502_namespace_has_no_version_suffix() {
    assert_eq!(NS_MUC_ACTIVITY, "urn:xmpp:muc-activity");
    assert_eq!(
        FIELD_MESSAGE_ACTIVITY,
        "{urn:xmpp:muc-activity}message-activity"
    );
}

#[test]
fn xep0502_activity_field_round_trips_messages_per_hour() {
    let field = build_message_activity_field(12.5);
    let xml = String::from(&field);
    let reparsed: Element = xml.parse().expect("field reparses");

    assert_eq!(reparsed.name(), "field");
    assert_eq!(reparsed.ns(), NS_DATA_FORMS);
    assert_eq!(reparsed.attr("var"), Some(FIELD_MESSAGE_ACTIVITY));
    assert_eq!(parse_message_activity_field(&reparsed), Some(12.5));
}

#[test]
fn xep0502_activity_field_rejects_wrong_var_or_bad_value() {
    let wrong_var: Element = "<field xmlns='jabber:x:data' var='other'><value>1</value></field>"
        .parse()
        .expect("valid xml");
    assert_eq!(parse_message_activity_field(&wrong_var), None);

    let bad_value: Element = "<field xmlns='jabber:x:data' var='{urn:xmpp:muc-activity}message-activity'><value>NaN</value></field>"
        .parse()
        .expect("valid xml");
    assert_eq!(parse_message_activity_field(&bad_value), None);
}

#[test]
fn xep0502_roominfo_form_contains_message_activity_field() {
    let form = build_muc_activity_roominfo_form(3.25);
    assert_eq!(form.name(), "x");
    assert_eq!(form.ns(), NS_DATA_FORMS);
    assert_eq!(form.attr("type"), Some("result"));

    let form_type = form
        .children()
        .find(|child| child.attr("var") == Some("FORM_TYPE"))
        .expect("FORM_TYPE field");
    assert_eq!(
        form_type
            .get_child("value", NS_DATA_FORMS)
            .map(|value| value.text()),
        Some("http://jabber.org/protocol/muc#roominfo".to_owned())
    );

    let field = form
        .children()
        .find(|child| child.attr("var") == Some(FIELD_MESSAGE_ACTIVITY))
        .expect("message activity field");
    assert_eq!(
        field
            .get_child("value", NS_DATA_FORMS)
            .map(|value| value.text()),
        Some("3.25".to_owned())
    );
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
