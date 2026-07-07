//! XEP-0500: MUC Slow Mode — dedicated suite.
//!
//! Pins the room-config field parsing, the `SlowModeConfig` /
//! `SlowModeCheck` semantics, and the per-occupant `SlowModeTracker`
//! enforcement rules (first message allowed, cooldown rate-limits,
//! moderator exemption, per-occupant isolation, leave/clear resets).
//!
use std::time::Duration;
use waddle_xmpp::xep::xep0004::NS_DATA_FORMS;
use waddle_xmpp::xep::xep0500::{
    build_muc_slow_mode_roominfo_form, build_roominfo_slow_mode_duration_field,
    parse_slow_mode_duration, SlowModeCheck, SlowModeConfig, SlowModeTracker,
    FIELD_ROOMINFO_SLOW_MODE_DURATION, FIELD_SLOW_MODE_DURATION, SLOW_MODE_DISABLED,
};

// ── Config field + value parsing ─────────────────────────────────────

#[test]
fn xep0500_field_constant_is_a_muc_roomconfig_field() {
    assert!(FIELD_SLOW_MODE_DURATION.starts_with("muc#roomconfig_"));
    assert_eq!(
        FIELD_SLOW_MODE_DURATION,
        "muc#roomconfig_slow_mode_duration"
    );
    assert_eq!(
        FIELD_ROOMINFO_SLOW_MODE_DURATION,
        "muc#roominfo_slow_mode_duration"
    );
}

#[test]
fn xep0500_zero_means_disabled() {
    // xep-0500.xml: "0=disabled, any positive integer= users can send
    // a message every X seconds."
    assert_eq!(SLOW_MODE_DISABLED, 0);
    assert!(!SlowModeConfig::new(0).is_enabled());
    assert!(!SlowModeConfig::disabled().is_enabled());
    assert!(!SlowModeConfig::default().is_enabled());
    assert!(SlowModeConfig::new(1).is_enabled());
}

#[test]
fn xep0500_parse_duration_accepts_positive_integers_only() {
    assert_eq!(parse_slow_mode_duration("20"), 20);
    assert_eq!(parse_slow_mode_duration(" 60 "), 60, "whitespace trimmed");
    assert_eq!(parse_slow_mode_duration("0"), 0);
}

#[test]
fn xep0500_parse_duration_degrades_malformed_values_to_disabled() {
    // A malformed form value must fail safe (slow mode off), never
    // panic or produce a bogus interval.
    for raw in ["", "abc", "-5", "1.5", "10s", "99999999999999999999999"] {
        assert_eq!(
            parse_slow_mode_duration(raw),
            SLOW_MODE_DISABLED,
            "`{raw}` must parse to disabled"
        );
    }
}

#[test]
fn xep0500_config_interval_is_seconds() {
    assert_eq!(SlowModeConfig::new(30).interval(), Duration::from_secs(30));
}

#[test]
fn xep0500_roominfo_field_uses_spec_var() {
    let field = build_roominfo_slow_mode_duration_field(20);
    assert_eq!(field.name(), "field");
    assert_eq!(field.ns(), NS_DATA_FORMS);
    assert_eq!(field.attr("var"), Some(FIELD_ROOMINFO_SLOW_MODE_DURATION));
    assert_eq!(field.attr("type"), Some("text-single"));
    assert_eq!(
        field
            .get_child("value", NS_DATA_FORMS)
            .map(|value| value.text()),
        Some("20".to_owned())
    );
}

#[test]
fn xep0500_roominfo_form_carries_duration_field() {
    let form = build_muc_slow_mode_roominfo_form(0);
    assert_eq!(form.name(), "x");
    assert_eq!(form.ns(), NS_DATA_FORMS);
    assert_eq!(form.attr("type"), Some("result"));
    let field = form
        .children()
        .find(|child| child.attr("var") == Some(FIELD_ROOMINFO_SLOW_MODE_DURATION))
        .expect("slow mode roominfo field");
    assert_eq!(
        field
            .get_child("value", NS_DATA_FORMS)
            .map(|value| value.text()),
        Some("0".to_owned())
    );
}

// ── Check-outcome semantics ──────────────────────────────────────────

#[test]
fn xep0500_check_outcome_helpers() {
    assert!(SlowModeCheck::Allowed.is_allowed());
    assert!(SlowModeCheck::Disabled.is_allowed());
    assert!(!SlowModeCheck::RateLimited(5).is_allowed());

    assert_eq!(SlowModeCheck::RateLimited(10).remaining_secs(), Some(10));
    assert_eq!(SlowModeCheck::Allowed.remaining_secs(), None);
    assert_eq!(SlowModeCheck::Disabled.remaining_secs(), None);
}

// ── Tracker enforcement ──────────────────────────────────────────────

#[test]
fn xep0500_first_message_is_always_allowed() {
    let tracker = SlowModeTracker::new(SlowModeConfig::new(30));
    assert_eq!(
        tracker.check("alice@example.com", false),
        SlowModeCheck::Allowed
    );
}

#[test]
fn xep0500_second_message_within_interval_is_rate_limited_with_countdown() {
    let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
    tracker.record_message("alice@example.com");

    let outcome = tracker.check("alice@example.com", false);
    let remaining = outcome
        .remaining_secs()
        .expect("second message inside the window must be rate-limited");
    // The countdown the error text surfaces must be within the
    // configured window (never zero, never beyond interval+1).
    assert!(
        (1..=31).contains(&remaining),
        "remaining {remaining}s must sit inside the 30s window"
    );
}

#[test]
fn xep0500_moderators_are_exempt() {
    // xep-0500.xml: room moderators SHOULD NOT be rate-limited.
    let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
    tracker.record_message("mod@example.com");
    assert_eq!(
        tracker.check("mod@example.com", true),
        SlowModeCheck::Allowed
    );
    // The same occupant without the moderator bit is limited.
    assert!(!tracker.check("mod@example.com", false).is_allowed());
}

#[test]
fn xep0500_occupants_are_tracked_independently() {
    let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
    tracker.record_message("alice@example.com");

    assert!(!tracker.check("alice@example.com", false).is_allowed());
    assert_eq!(
        tracker.check("bob@example.com", false),
        SlowModeCheck::Allowed,
        "bob has not sent anything yet"
    );
}

#[test]
fn xep0500_disabled_tracker_reports_disabled_and_records_nothing() {
    let mut tracker = SlowModeTracker::disabled();
    assert_eq!(
        tracker.check("alice@example.com", false),
        SlowModeCheck::Disabled
    );

    // record_message on a disabled tracker is a no-op: enabling slow
    // mode later must not retroactively rate-limit past messages.
    tracker.record_message("alice@example.com");
    tracker.set_interval(30);
    assert_eq!(
        tracker.check("alice@example.com", false),
        SlowModeCheck::Allowed
    );
}

#[test]
fn xep0500_disabling_clears_tracked_state() {
    let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
    tracker.record_message("alice@example.com");
    assert!(!tracker.check("alice@example.com", false).is_allowed());

    tracker.set_interval(0);
    assert_eq!(
        tracker.check("alice@example.com", false),
        SlowModeCheck::Disabled
    );
    assert_eq!(tracker.config(), &SlowModeConfig::disabled());

    // Re-enabling starts from a clean slate.
    tracker.set_interval(30);
    assert_eq!(
        tracker.check("alice@example.com", false),
        SlowModeCheck::Allowed
    );
}

#[test]
fn xep0500_leave_resets_the_occupant_cooldown() {
    let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
    tracker.record_message("alice@example.com");
    tracker.remove_occupant("alice@example.com");
    assert_eq!(
        tracker.check("alice@example.com", false),
        SlowModeCheck::Allowed
    );
}

#[test]
fn xep0500_clear_resets_every_occupant() {
    let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
    tracker.record_message("alice@example.com");
    tracker.record_message("bob@example.com");
    tracker.clear();
    assert_eq!(
        tracker.check("alice@example.com", false),
        SlowModeCheck::Allowed
    );
    assert_eq!(
        tracker.check("bob@example.com", false),
        SlowModeCheck::Allowed
    );
}

#[test]
fn xep0500_cooldown_expires_after_interval() {
    let mut tracker = SlowModeTracker::new(SlowModeConfig::new(1));
    tracker.record_message("alice@example.com");
    assert!(!tracker.check("alice@example.com", false).is_allowed());

    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(
        tracker.check("alice@example.com", false),
        SlowModeCheck::Allowed,
        "after the interval elapses the occupant may speak again"
    );
}
