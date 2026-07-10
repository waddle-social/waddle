//! XEP-0500: MUC Slow Mode
//!
//! Provides rate limiting for MUC rooms. When enabled, occupants must
//! wait a configurable duration between messages. Moderators are exempt.
//!
//! ## Configuration
//!
//! Set via room configuration form (XEP-0045):
//! ```xml
//! <field var='muc#roomconfig_slow_mode_duration' type='text-single'>
//!   <value>30</value>
//! </field>
//! ```
//!
//! ## Server Behavior
//!
//! When slow mode is active:
//! - Track last message timestamp per occupant
//! - If an occupant sends before the interval expires, return error:
//! ```xml
//! <message type='error'>
//!   <error type='wait'>
//!     <policy-violation xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
//!     <text>Slow mode: please wait N seconds</text>
//!   </error>
//! </message>
//! ```
//! - Moderators and admins are exempt from the limit
//!
//! ## Client Behavior
//!
//! - Parse the slow mode duration from room config
//! - Show a countdown timer after sending a message
//! - Disable the send button during cooldown

use std::collections::HashMap;
use std::time::{Duration, Instant};

use minidom::Element;

use super::xep0004::{Field, ToElement};

/// Room configuration field for slow mode duration.
pub const FIELD_SLOW_MODE_DURATION: &str = "muc#roomconfig_slow_mode_duration";

/// MUC roominfo disco field for slow mode duration.
pub const FIELD_ROOMINFO_SLOW_MODE_DURATION: &str = "muc#roominfo_slow_mode_duration";

/// Default: slow mode disabled (0 seconds).
pub const SLOW_MODE_DISABLED: u64 = 0;

/// Slow mode configuration for a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlowModeConfig {
    /// Interval in seconds between messages. 0 = disabled.
    pub interval_secs: u64,
}

impl SlowModeConfig {
    /// Create a new slow mode config.
    pub fn new(interval_secs: u64) -> Self {
        Self { interval_secs }
    }

    /// Slow mode disabled.
    pub fn disabled() -> Self {
        Self {
            interval_secs: SLOW_MODE_DISABLED,
        }
    }

    /// Returns `true` if slow mode is active.
    pub fn is_enabled(&self) -> bool {
        self.interval_secs > 0
    }

    /// Get the interval as a Duration.
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }
}

impl Default for SlowModeConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Result of a slow mode rate limit check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlowModeCheck {
    /// Message is allowed.
    Allowed,
    /// Message is rate-limited. Contains seconds remaining.
    RateLimited(u64),
    /// Slow mode is not active for this room.
    Disabled,
}

impl SlowModeCheck {
    /// Returns `true` if the message should be allowed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed | Self::Disabled)
    }

    /// Returns the remaining cooldown seconds, if rate-limited.
    pub fn remaining_secs(&self) -> Option<u64> {
        match self {
            Self::RateLimited(secs) => Some(*secs),
            _ => None,
        }
    }
}

/// Tracks per-occupant message timestamps for slow mode enforcement.
#[derive(Debug)]
pub struct SlowModeTracker {
    /// The slow mode configuration.
    config: SlowModeConfig,
    /// Last message timestamp per occupant (by bare JID or nick).
    last_message: HashMap<String, Instant>,
}

impl SlowModeTracker {
    /// Create a new tracker with the given config.
    pub fn new(config: SlowModeConfig) -> Self {
        Self {
            config,
            last_message: HashMap::new(),
        }
    }

    /// Create a disabled tracker.
    pub fn disabled() -> Self {
        Self::new(SlowModeConfig::disabled())
    }

    /// Update the slow mode interval.
    pub fn set_interval(&mut self, interval_secs: u64) {
        self.config = SlowModeConfig::new(interval_secs);
        if interval_secs == 0 {
            self.last_message.clear();
        }
    }

    /// Check if an occupant can send a message.
    ///
    /// `occupant_id` should be the bare JID or a stable identifier.
    /// `is_moderator` exempts the user from rate limiting.
    pub fn check(&self, occupant_id: &str, is_moderator: bool) -> SlowModeCheck {
        if !self.config.is_enabled() {
            return SlowModeCheck::Disabled;
        }

        if is_moderator {
            return SlowModeCheck::Allowed;
        }

        if let Some(last) = self.last_message.get(occupant_id) {
            let elapsed = last.elapsed();
            let interval = self.config.interval();
            if elapsed < interval {
                let remaining = (interval - elapsed).as_secs() + 1;
                return SlowModeCheck::RateLimited(remaining);
            }
        }

        SlowModeCheck::Allowed
    }

    /// Record that an occupant sent a message.
    pub fn record_message(&mut self, occupant_id: &str) {
        if self.config.is_enabled() {
            self.last_message
                .insert(occupant_id.to_owned(), Instant::now());
        }
    }

    /// Remove an occupant's tracking (e.g., on leave).
    pub fn remove_occupant(&mut self, occupant_id: &str) {
        self.last_message.remove(occupant_id);
    }

    /// Clear all tracking data.
    pub fn clear(&mut self) {
        self.last_message.clear();
    }

    /// Get the current config.
    pub fn config(&self) -> &SlowModeConfig {
        &self.config
    }
}

/// Parse slow mode duration from a room config or disco field value.
pub fn parse_slow_mode_duration(value: &str) -> u64 {
    value.trim().parse().unwrap_or(SLOW_MODE_DISABLED)
}

/// Build the XEP-0500 roominfo field for disco#info extension forms.
///
/// The field is emitted inside the room's single `muc#roominfo` form —
/// see `crate::muc::roominfo::MucRoomInfo`. XEP-0500 must not append
/// its own `muc#roominfo` form: two forms with the same FORM_TYPE make
/// the disco#info response ill-formed per XEP-0115 §5.4 (#1259).
pub fn build_roominfo_slow_mode_duration_field(duration_secs: u64) -> Element {
    Field::text_single(FIELD_ROOMINFO_SLOW_MODE_DURATION, duration_secs.to_string()).to_element()
}

#[cfg(test)]
mod tests {
    use super::super::xep0004::NS_DATA_FORMS;
    use super::*;
    use std::thread;

    #[test]
    fn test_slow_mode_config_disabled() {
        let config = SlowModeConfig::disabled();
        assert!(!config.is_enabled());
        assert_eq!(config.interval_secs, 0);
    }

    #[test]
    fn test_slow_mode_config_enabled() {
        let config = SlowModeConfig::new(30);
        assert!(config.is_enabled());
        assert_eq!(config.interval(), Duration::from_secs(30));
    }

    #[test]
    fn test_slow_mode_config_default() {
        let config = SlowModeConfig::default();
        assert!(!config.is_enabled());
    }

    #[test]
    fn test_tracker_disabled() {
        let tracker = SlowModeTracker::disabled();
        assert_eq!(tracker.check("alice", false), SlowModeCheck::Disabled);
    }

    #[test]
    fn test_tracker_first_message_allowed() {
        let tracker = SlowModeTracker::new(SlowModeConfig::new(30));
        assert_eq!(tracker.check("alice", false), SlowModeCheck::Allowed);
    }

    #[test]
    fn test_tracker_rate_limited() {
        let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
        tracker.record_message("alice");

        let result = tracker.check("alice", false);
        assert!(matches!(result, SlowModeCheck::RateLimited(_)));
        assert!(result.remaining_secs().is_some());
        assert!(!result.is_allowed());
    }

    #[test]
    fn test_tracker_moderator_exempt() {
        let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
        tracker.record_message("mod_user");

        assert_eq!(tracker.check("mod_user", true), SlowModeCheck::Allowed);
    }

    #[test]
    fn test_tracker_different_occupants() {
        let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
        tracker.record_message("alice");

        // Bob hasn't sent yet, should be allowed
        assert_eq!(tracker.check("bob", false), SlowModeCheck::Allowed);
    }

    #[test]
    fn test_tracker_after_interval() {
        // Use a very short interval for testing
        let mut tracker = SlowModeTracker::new(SlowModeConfig::new(0));
        tracker.set_interval(1);
        tracker.record_message("alice");

        // Wait just over 1 second
        thread::sleep(Duration::from_millis(1100));
        assert_eq!(tracker.check("alice", false), SlowModeCheck::Allowed);
    }

    #[test]
    fn test_tracker_remove_occupant() {
        let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
        tracker.record_message("alice");
        tracker.remove_occupant("alice");

        assert_eq!(tracker.check("alice", false), SlowModeCheck::Allowed);
    }

    #[test]
    fn test_tracker_clear() {
        let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
        tracker.record_message("alice");
        tracker.record_message("bob");
        tracker.clear();

        assert_eq!(tracker.check("alice", false), SlowModeCheck::Allowed);
        assert_eq!(tracker.check("bob", false), SlowModeCheck::Allowed);
    }

    #[test]
    fn test_tracker_set_interval() {
        let mut tracker = SlowModeTracker::new(SlowModeConfig::new(30));
        tracker.record_message("alice");

        // Disable slow mode
        tracker.set_interval(0);
        assert_eq!(tracker.check("alice", false), SlowModeCheck::Disabled);
    }

    #[test]
    fn test_slow_mode_check_helpers() {
        assert!(SlowModeCheck::Allowed.is_allowed());
        assert!(SlowModeCheck::Disabled.is_allowed());
        assert!(!SlowModeCheck::RateLimited(5).is_allowed());

        assert_eq!(SlowModeCheck::RateLimited(10).remaining_secs(), Some(10));
        assert_eq!(SlowModeCheck::Allowed.remaining_secs(), None);
    }

    #[test]
    fn test_parse_slow_mode_duration() {
        assert_eq!(parse_slow_mode_duration("30"), 30);
        assert_eq!(parse_slow_mode_duration("0"), 0);
        assert_eq!(parse_slow_mode_duration(""), 0);
        assert_eq!(parse_slow_mode_duration("abc"), 0);
        assert_eq!(parse_slow_mode_duration(" 60 "), 60);
    }

    #[test]
    fn test_build_roominfo_slow_mode_duration_field() {
        let field = build_roominfo_slow_mode_duration_field(20);
        assert_eq!(field.name(), "field");
        assert_eq!(field.ns(), NS_DATA_FORMS);
        assert_eq!(field.attr("var"), Some(FIELD_ROOMINFO_SLOW_MODE_DURATION));
        assert_eq!(field.attr("type"), Some("text-single"));
        assert_eq!(
            field.get_child("value", NS_DATA_FORMS).map(|v| v.text()),
            Some("20".to_owned())
        );
    }
}
