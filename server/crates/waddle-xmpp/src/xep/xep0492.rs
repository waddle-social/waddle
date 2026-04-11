//! XEP-0492: Chat Notification Settings
//!
//! Per-room notification preferences allowing users to control how
//! they receive notifications for each channel: all messages,
//! mentions only, or fully muted.
//!
//! ## XML Format
//!
//! Stored via PEP:
//! ```xml
//! <item id='room@muc.example.com' xmlns='http://jabber.org/protocol/pubsub'>
//!   <notification-settings xmlns='urn:xmpp:notification-settings:0'
//!                          level='mentions-only'/>
//! </item>
//! ```
//!
//! ## Notification Levels
//!
//! - **all**: Notify for every message (default)
//! - **mentions-only**: Only notify when @mentioned
//! - **mute**: No notifications at all
//!
//! ## Use Cases
//!
//! - Mute noisy channels
//! - Get mentions-only for low-priority rooms
//! - Per-room customization in the channel sidebar

use minidom::Element;

/// Namespace for XEP-0492 Chat Notification Settings.
pub const NS_NOTIFICATION_SETTINGS: &str = "urn:xmpp:notification-settings:0";

/// PEP node for notification settings.
pub const PEP_NODE_NOTIFICATION_SETTINGS: &str = "urn:xmpp:notification-settings:0";

/// Notification level for a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationLevel {
    /// Notify for all messages (default).
    All,
    /// Notify only when @mentioned.
    MentionsOnly,
    /// No notifications.
    Mute,
}

impl NotificationLevel {
    /// Parse from attribute string.
    pub fn from_str_attr(s: &str) -> Option<Self> {
        match s {
            "all" => Some(Self::All),
            "mentions-only" => Some(Self::MentionsOnly),
            "mute" => Some(Self::Mute),
            _ => None,
        }
    }

    /// Convert to attribute string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::MentionsOnly => "mentions-only",
            Self::Mute => "mute",
        }
    }

    /// Returns `true` if this level suppresses notifications.
    pub fn is_suppressed(self) -> bool {
        matches!(self, Self::Mute)
    }

    /// Returns `true` if this level only allows mention notifications.
    pub fn is_mentions_only(self) -> bool {
        matches!(self, Self::MentionsOnly)
    }

    /// Check if a message should generate a notification given this level.
    pub fn should_notify(self, is_mention: bool) -> bool {
        match self {
            Self::All => true,
            Self::MentionsOnly => is_mention,
            Self::Mute => false,
        }
    }
}

impl Default for NotificationLevel {
    fn default() -> Self {
        Self::All
    }
}

impl std::fmt::Display for NotificationLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Notification setting for a specific room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomNotificationSetting {
    /// The room JID.
    pub room_jid: String,
    /// The notification level.
    pub level: NotificationLevel,
}

impl RoomNotificationSetting {
    /// Create a new room notification setting.
    pub fn new(room_jid: impl Into<String>, level: NotificationLevel) -> Self {
        Self {
            room_jid: room_jid.into(),
            level,
        }
    }
}

/// Collection of notification settings across rooms.
#[derive(Debug, Default)]
pub struct NotificationSettings {
    settings: std::collections::HashMap<String, NotificationLevel>,
}

impl NotificationSettings {
    /// Create empty settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the notification level for a room.
    pub fn set(&mut self, room_jid: &str, level: NotificationLevel) {
        if level == NotificationLevel::All {
            // Default level - remove the override
            self.settings.remove(room_jid);
        } else {
            self.settings.insert(room_jid.to_owned(), level);
        }
    }

    /// Get the notification level for a room.
    pub fn get(&self, room_jid: &str) -> NotificationLevel {
        self.settings.get(room_jid).copied().unwrap_or_default()
    }

    /// Remove the setting for a room (reverts to default).
    pub fn remove(&mut self, room_jid: &str) {
        self.settings.remove(room_jid);
    }

    /// Check if a message in a room should trigger a notification.
    pub fn should_notify(&self, room_jid: &str, is_mention: bool) -> bool {
        self.get(room_jid).should_notify(is_mention)
    }

    /// Get all rooms with non-default notification levels.
    pub fn overrides(&self) -> Vec<RoomNotificationSetting> {
        self.settings
            .iter()
            .map(|(jid, &level)| RoomNotificationSetting::new(jid.clone(), level))
            .collect()
    }

    /// Get all muted rooms.
    pub fn muted_rooms(&self) -> Vec<&str> {
        self.settings
            .iter()
            .filter(|(_, &level)| level == NotificationLevel::Mute)
            .map(|(jid, _)| jid.as_str())
            .collect()
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a notification-settings element.
pub fn is_notification_settings_element(elem: &Element) -> bool {
    elem.ns() == NS_NOTIFICATION_SETTINGS && elem.name() == "notification-settings"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse a notification level from a settings element.
pub fn parse_notification_setting(elem: &Element) -> Option<NotificationLevel> {
    if !is_notification_settings_element(elem) {
        return None;
    }
    elem.attr("level")
        .and_then(NotificationLevel::from_str_attr)
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<notification-settings/>` element.
pub fn build_notification_settings_element(level: NotificationLevel) -> Element {
    Element::builder("notification-settings", NS_NOTIFICATION_SETTINGS)
        .attr("level", level.as_str())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_level_from_str() {
        assert_eq!(
            NotificationLevel::from_str_attr("all"),
            Some(NotificationLevel::All)
        );
        assert_eq!(
            NotificationLevel::from_str_attr("mentions-only"),
            Some(NotificationLevel::MentionsOnly)
        );
        assert_eq!(
            NotificationLevel::from_str_attr("mute"),
            Some(NotificationLevel::Mute)
        );
        assert_eq!(NotificationLevel::from_str_attr("invalid"), None);
    }

    #[test]
    fn test_notification_level_as_str() {
        assert_eq!(NotificationLevel::All.as_str(), "all");
        assert_eq!(NotificationLevel::MentionsOnly.as_str(), "mentions-only");
        assert_eq!(NotificationLevel::Mute.as_str(), "mute");
    }

    #[test]
    fn test_notification_level_display() {
        assert_eq!(NotificationLevel::All.to_string(), "all");
        assert_eq!(NotificationLevel::Mute.to_string(), "mute");
    }

    #[test]
    fn test_notification_level_default() {
        assert_eq!(NotificationLevel::default(), NotificationLevel::All);
    }

    #[test]
    fn test_should_notify() {
        // All: always notify
        assert!(NotificationLevel::All.should_notify(false));
        assert!(NotificationLevel::All.should_notify(true));

        // Mentions only: only when mentioned
        assert!(!NotificationLevel::MentionsOnly.should_notify(false));
        assert!(NotificationLevel::MentionsOnly.should_notify(true));

        // Mute: never notify
        assert!(!NotificationLevel::Mute.should_notify(false));
        assert!(!NotificationLevel::Mute.should_notify(true));
    }

    #[test]
    fn test_is_suppressed() {
        assert!(!NotificationLevel::All.is_suppressed());
        assert!(!NotificationLevel::MentionsOnly.is_suppressed());
        assert!(NotificationLevel::Mute.is_suppressed());
    }

    #[test]
    fn test_notification_settings() {
        let mut settings = NotificationSettings::new();

        // Default is All
        assert_eq!(settings.get("room@muc"), NotificationLevel::All);
        assert!(settings.should_notify("room@muc", false));

        // Set mentions-only
        settings.set("room@muc", NotificationLevel::MentionsOnly);
        assert_eq!(settings.get("room@muc"), NotificationLevel::MentionsOnly);
        assert!(!settings.should_notify("room@muc", false));
        assert!(settings.should_notify("room@muc", true));

        // Set mute
        settings.set("noisy@muc", NotificationLevel::Mute);
        assert!(!settings.should_notify("noisy@muc", true));

        // Overrides
        assert_eq!(settings.overrides().len(), 2);
        assert_eq!(settings.muted_rooms(), vec!["noisy@muc"]);

        // Remove override
        settings.remove("room@muc");
        assert_eq!(settings.get("room@muc"), NotificationLevel::All);
    }

    #[test]
    fn test_set_default_removes_override() {
        let mut settings = NotificationSettings::new();
        settings.set("room@muc", NotificationLevel::Mute);
        assert_eq!(settings.overrides().len(), 1);

        settings.set("room@muc", NotificationLevel::All);
        assert_eq!(settings.overrides().len(), 0);
    }

    #[test]
    fn test_build_and_parse() {
        let elem = build_notification_settings_element(NotificationLevel::MentionsOnly);
        assert_eq!(elem.name(), "notification-settings");
        assert_eq!(elem.ns(), NS_NOTIFICATION_SETTINGS);
        assert_eq!(elem.attr("level"), Some("mentions-only"));

        let parsed = parse_notification_setting(&elem);
        assert_eq!(parsed, Some(NotificationLevel::MentionsOnly));
    }

    #[test]
    fn test_is_notification_settings_element() {
        let elem = build_notification_settings_element(NotificationLevel::All);
        assert!(is_notification_settings_element(&elem));

        let wrong = Element::builder("notification-settings", "jabber:client").build();
        assert!(!is_notification_settings_element(&wrong));
    }

    #[test]
    fn test_room_notification_setting() {
        let s = RoomNotificationSetting::new("room@muc", NotificationLevel::Mute);
        assert_eq!(s.room_jid, "room@muc");
        assert_eq!(s.level, NotificationLevel::Mute);
    }

    #[test]
    fn test_pep_node() {
        assert_eq!(
            PEP_NODE_NOTIFICATION_SETTINGS,
            "urn:xmpp:notification-settings:0"
        );
    }
}
