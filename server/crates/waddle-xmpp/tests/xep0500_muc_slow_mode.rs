//! XEP-0500: MUC Slow Mode — dedicated suite.
//!
//! Pins the room-config field parsing, the `SlowModeConfig` /
//! `SlowModeCheck` semantics, and the per-occupant `SlowModeTracker`
//! enforcement rules (first message allowed, cooldown rate-limits,
//! moderator exemption, per-occupant isolation, leave/clear resets).
//!
//! Known spec divergence (reported, not pinned as conformant):
//! xep-0500.xml names the room-config field
//! `muc#roomconfig_slow_mode_duration` (and the disco surface
//! `muc#roominfo_slow_mode_duration`), whereas this module registers
//! `muc#roomconfig_slow_mode_interval`. The tests below pin the
//! module's actual constant; renaming to the spec field is production
//! work outside this suite's scope.

use std::time::Duration;
use waddle_xmpp::xep::xep0500::{
    parse_slow_mode_interval, SlowModeCheck, SlowModeConfig, SlowModeTracker,
    FIELD_SLOW_MODE_INTERVAL, SLOW_MODE_DISABLED,
};

// ── Config field + value parsing ─────────────────────────────────────

#[test]
fn xep0500_field_constant_is_a_muc_roomconfig_field() {
    // See the module-level divergence note: the exact suffix differs
    // from xep-0500.xml (`_duration`), but the field must at minimum
    // stay in the `muc#roomconfig_` registry namespace so it rides
    // the XEP-0045 configuration form.
    assert!(FIELD_SLOW_MODE_INTERVAL.starts_with("muc#roomconfig_"));
    assert_eq!(
        FIELD_SLOW_MODE_INTERVAL,
        "muc#roomconfig_slow_mode_interval"
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
fn xep0500_parse_interval_accepts_positive_integers_only() {
    assert_eq!(parse_slow_mode_interval("20"), 20);
    assert_eq!(parse_slow_mode_interval(" 60 "), 60, "whitespace trimmed");
    assert_eq!(parse_slow_mode_interval("0"), 0);
}

#[test]
fn xep0500_parse_interval_degrades_malformed_values_to_disabled() {
    // A malformed form value must fail safe (slow mode off), never
    // panic or produce a bogus interval.
    for raw in ["", "abc", "-5", "1.5", "10s", "99999999999999999999999"] {
        assert_eq!(
            parse_slow_mode_interval(raw),
            SLOW_MODE_DISABLED,
            "`{raw}` must parse to disabled"
        );
    }
}

#[test]
fn xep0500_config_interval_is_seconds() {
    assert_eq!(SlowModeConfig::new(30).interval(), Duration::from_secs(30));
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
