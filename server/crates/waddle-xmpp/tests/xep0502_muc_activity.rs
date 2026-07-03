//! XEP-0502: MUC Activity Indicator — dedicated suite.
//!
//! Pins the `<activity/>` subscribe/notify payload shapes this module
//! implements, their build → serialize → reparse round-trips, the
//! RFC 3339 `last-activity` timestamp handling, and the client-side
//! `ActivityTracker` bookkeeping.
//!
//! Known spec divergence (reported, not pinned as conformant):
//! xep-0502.xml defines a disco#info extension field
//! `{urn:xmpp:muc-activity}message-activity` (no version suffix on
//! the namespace) rather than a subscribe/notify element protocol;
//! this module uses `urn:xmpp:muc-activity:0` and a bespoke
//! `<activity><subscribe/></activity>` / `<activity><active/></activity>`
//! exchange. The tests below pin the module's actual behaviour;
//! reconciling with the published XEP is production work outside this
//! suite's scope.

use chrono::{DateTime, TimeZone, Utc};
use minidom::Element;
use waddle_xmpp::xep::xep0502::{
    build_activity_notification, build_subscribe_element, is_activity_element,
    parse_activity_notifications, ActivityTracker, RoomActivity, NS_MUC_ACTIVITY,
};

fn test_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
        .single()
        .expect("valid test date")
}

// ── Element classifier ───────────────────────────────────────────────

#[test]
fn xep0502_classifier_requires_name_and_namespace() {
    assert!(is_activity_element(
        &Element::builder("activity", NS_MUC_ACTIVITY).build()
    ));
    assert!(!is_activity_element(
        &Element::builder("activity", "jabber:client").build()
    ));
    assert!(!is_activity_element(
        &Element::builder("active", NS_MUC_ACTIVITY).build()
    ));
}

// ── Notification round-trips ─────────────────────────────────────────

#[test]
fn xep0502_notification_survives_serialize_reparse_round_trip() {
    let ra1 = RoomActivity::new("room1@muc.example.com").with_last_activity(test_time());
    let ra2 = RoomActivity::new("room2@muc.example.com").with_activity_now();

    let elem = build_activity_notification(&[&ra1, &ra2]);
    let xml = String::from(&elem);
    let reparsed: Element = xml.parse().expect("built element must reparse");

    assert!(is_activity_element(&reparsed));
    let parsed = parse_activity_notifications(&reparsed);
    assert_eq!(parsed.len(), 2);

    assert_eq!(parsed[0].room_jid, "room1@muc.example.com");
    assert!(parsed[0].has_new_messages);
    assert_eq!(
        parsed[0].last_activity,
        Some(test_time()),
        "RFC 3339 timestamp must survive serialize → reparse"
    );
    assert_eq!(parsed[1].room_jid, "room2@muc.example.com");
    assert!(parsed[1].last_activity.is_some());
}

#[test]
fn xep0502_notification_without_timestamp_parses_with_none() {
    let ra = RoomActivity::new("room@muc.example.com");
    let elem = build_activity_notification(&[&ra]);

    let active = elem.children().next().expect("one <active/> child");
    assert_eq!(active.name(), "active");
    assert_eq!(active.ns(), NS_MUC_ACTIVITY);
    assert_eq!(
        active.attr("last-activity"),
        None,
        "no timestamp must mean no attribute, not an empty string"
    );

    let parsed = parse_activity_notifications(&elem);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].last_activity, None);
    assert!(parsed[0].has_new_messages);
}

// ── Parser robustness ────────────────────────────────────────────────

#[test]
fn xep0502_parse_ignores_wrong_wrapper_element() {
    let wrong_name = Element::builder("other", NS_MUC_ACTIVITY).build();
    assert!(parse_activity_notifications(&wrong_name).is_empty());

    let wrong_ns: Element = "<activity xmlns='urn:xmpp:evil:0'>\
            <active jid='room@muc'/>\
        </activity>"
        .parse()
        .expect("valid xml");
    assert!(parse_activity_notifications(&wrong_ns).is_empty());
}

#[test]
fn xep0502_parse_skips_active_entries_without_jid() {
    let elem: Element = "<activity xmlns='urn:xmpp:muc-activity:0'>\
            <active/>\
            <active jid=''/>\
            <active jid='kept@muc.example.com'/>\
        </activity>"
        .parse()
        .expect("valid xml");
    let parsed = parse_activity_notifications(&elem);
    assert_eq!(parsed.len(), 1, "jid-less entries must be dropped");
    assert_eq!(parsed[0].room_jid, "kept@muc.example.com");
}

#[test]
fn xep0502_parse_ignores_foreign_namespace_children() {
    let elem: Element = "<activity xmlns='urn:xmpp:muc-activity:0'>\
            <active xmlns='urn:xmpp:evil:0' jid='evil@muc'/>\
            <active jid='good@muc'/>\
        </activity>"
        .parse()
        .expect("valid xml");
    let parsed = parse_activity_notifications(&elem);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].room_jid, "good@muc");
}

#[test]
fn xep0502_malformed_timestamp_degrades_to_none() {
    let elem: Element = "<activity xmlns='urn:xmpp:muc-activity:0'>\
            <active jid='room@muc' last-activity='yesterday-ish'/>\
        </activity>"
        .parse()
        .expect("valid xml");
    let parsed = parse_activity_notifications(&elem);
    assert_eq!(parsed.len(), 1);
    assert_eq!(
        parsed[0].last_activity, None,
        "an unparseable timestamp must not fail the whole entry"
    );
}

// ── Subscribe shape ──────────────────────────────────────────────────

#[test]
fn xep0502_subscribe_element_lists_each_room() {
    let elem = build_subscribe_element(&["room1@muc", "room2@muc"]);
    assert!(is_activity_element(&elem));

    let jids: Vec<&str> = elem
        .children()
        .filter(|c| c.name() == "subscribe" && c.ns() == NS_MUC_ACTIVITY)
        .filter_map(|c| c.attr("jid"))
        .collect();
    assert_eq!(jids, vec!["room1@muc", "room2@muc"]);
}

#[test]
fn xep0502_empty_subscribe_list_builds_childless_element() {
    let elem = build_subscribe_element(&[]);
    assert!(is_activity_element(&elem));
    assert_eq!(elem.children().count(), 0);
}

// ── Model + tracker bookkeeping ──────────────────────────────────────

#[test]
fn xep0502_room_activity_read_state_transitions() {
    let mut ra = RoomActivity::new("room@muc");
    assert!(!ra.has_new_messages);
    assert_eq!(ra.last_activity, None);

    ra = ra.with_last_activity(test_time());
    assert!(ra.has_new_messages);

    ra.mark_read();
    assert!(!ra.has_new_messages);
    assert_eq!(
        ra.last_activity,
        Some(test_time()),
        "mark_read clears the badge, not the timestamp"
    );
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
    // The room stays tracked with its timestamp for ordering.
    assert_eq!(
        tracker.get("room1@muc").and_then(|r| r.last_activity),
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
        tracker.get("room@muc").and_then(|r| r.last_activity),
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

// ── Wire → tracker integration ───────────────────────────────────────

#[test]
fn xep0502_parsed_notifications_feed_the_tracker() {
    let ra = RoomActivity::new("room@muc.example.com").with_last_activity(test_time());
    let elem = build_activity_notification(&[&ra]);
    let xml = String::from(&elem);
    let reparsed: Element = xml.parse().expect("reparses");

    let mut tracker = ActivityTracker::new();
    for activity in parse_activity_notifications(&reparsed) {
        let ts = activity.last_activity.expect("timestamp present");
        tracker.record_activity(&activity.room_jid, ts);
    }

    assert!(tracker.has_activity("room@muc.example.com"));
    assert_eq!(tracker.active_count(), 1);
}
