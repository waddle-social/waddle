//! XEP-0492: Chat Notification Settings
//!
//! Per-chat notification preferences allowing users to control whether
//! a conversation should notify always, only on mention, or never.
//!
//! ## XML Format
//!
//! Stored under an element that identifies a chat, such as a XEP-0402
//! `<extensions/>` element:
//! ```xml
//! <notify xmlns='urn:xmpp:notification-settings:1'>
//!   <on-mention/>
//! </notify>
//! ```
//!
//! ## Notification Settings
//!
//! - **always**: Notify for every message.
//! - **on-mention**: Only notify when explicitly mentioned.
//! - **never**: No notifications.
//!
//! ## Use Cases
//!
//! - Mute noisy channels
//! - Get mentions-only for low-priority rooms
//! - Per-room customization in the channel sidebar

use std::collections::HashSet;

use minidom::Element;

/// Namespace for XEP-0492 Chat Notification Settings.
pub const NS_NOTIFICATION_SETTINGS: &str = "urn:xmpp:notification-settings:1";

/// Waddle-specific XEP-0492 `<advanced/>` extension namespace.
///
/// XEP-0492 §2.3 reserves the optional `<advanced/>` child for
/// "finer-grained notification settings using custom namespaces". This
/// is the conformant home for the Waddle-specific opt-in to a rich
/// XEP-0357 push summary (`last-message-sender` + `last-message-body`):
/// the `<advanced/>` element is the XEP-defined container, and the
/// `<rich-payload/>` child carries the Waddle semantics under a
/// `urn:waddle:*` namespace. Presence of the child means opt-in;
/// absence means the minimal default payload.
pub const NS_PUSH_RICH_PAYLOAD: &str = "urn:waddle:push:rich:0";

/// Notification setting for a chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NotificationLevel {
    /// Notify for every message.
    #[default]
    Always,
    /// Notify only when explicitly mentioned.
    OnMention,
    /// No notifications.
    Never,
}

impl NotificationLevel {
    /// Parse from a XEP-0492 notification child element name.
    pub fn from_element_name(name: &str) -> Option<Self> {
        match name {
            "always" => Some(Self::Always),
            "on-mention" => Some(Self::OnMention),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    /// Convert to the XEP-0492 notification child element name.
    pub fn element_name(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::OnMention => "on-mention",
            Self::Never => "never",
        }
    }

    /// Returns `true` if this level suppresses notifications.
    pub fn is_suppressed(self) -> bool {
        matches!(self, Self::Never)
    }

    /// Returns `true` if this level only allows mention notifications.
    pub fn is_mentions_only(self) -> bool {
        matches!(self, Self::OnMention)
    }

    /// Check if a message should generate a notification given this level.
    pub fn should_notify(self, is_mention: bool) -> bool {
        match self {
            Self::Always => true,
            Self::OnMention => is_mention,
            Self::Never => false,
        }
    }
}

impl std::fmt::Display for NotificationLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.element_name())
    }
}

/// Parse or update error for a XEP-0492 `<notify/>` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationSettingsError {
    /// The supplied element is not a XEP-0492 `<notify/>`.
    NotNotifyElement,
    /// The `<notify/>` element contains a child not allowed by XEP-0492.
    InvalidNotifyChild(String),
    /// A notification setting contains a child not allowed by XEP-0492.
    InvalidSettingChild(String),
    /// More than one `<advanced/>` child is present on a setting.
    MultipleAdvancedElements,
    /// More than one account-wide fallback child is present.
    MultipleFallbackSettings,
    /// More than one setting has the same name and identity attributes.
    DuplicateNotificationSetting,
    /// `identity-type` appeared without `identity-category`.
    IdentityTypeWithoutCategory,
}

impl std::fmt::Display for NotificationSettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotNotifyElement => f.write_str("not a XEP-0492 notify element"),
            Self::InvalidNotifyChild(child) => {
                write!(f, "invalid XEP-0492 notify child element: {child}")
            }
            Self::InvalidSettingChild(child) => {
                write!(f, "invalid XEP-0492 setting child element: {child}")
            }
            Self::MultipleAdvancedElements => {
                f.write_str("multiple XEP-0492 advanced elements on one setting")
            }
            Self::MultipleFallbackSettings => {
                f.write_str("multiple XEP-0492 fallback notification settings")
            }
            Self::DuplicateNotificationSetting => {
                f.write_str("duplicate XEP-0492 notification setting")
            }
            Self::IdentityTypeWithoutCategory => {
                f.write_str("identity-type must not appear without identity-category")
            }
        }
    }
}

impl std::error::Error for NotificationSettingsError {}

/// Notification setting for a specific room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomNotificationSetting {
    /// The room JID.
    pub room_jid: String,
    /// The notification setting.
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
        if level == NotificationLevel::Always {
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
            .filter(|(_, &level)| level == NotificationLevel::Never)
            .map(|(jid, _)| jid.as_str())
            .collect()
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a XEP-0492 `<notify/>` element.
pub fn is_notify_element(elem: &Element) -> bool {
    elem.ns() == NS_NOTIFICATION_SETTINGS && elem.name() == "notify"
}

/// Check if an element is a XEP-0492 `<notify/>` element.
pub fn is_notification_settings_element(elem: &Element) -> bool {
    is_notify_element(elem)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse the account-wide fallback notification setting from `<notify/>`.
pub fn parse_notify_fallback_setting(
    elem: &Element,
) -> Result<Option<NotificationLevel>, NotificationSettingsError> {
    validate_notify_element(elem)?;

    let mut fallback = None;
    for child in elem.children().filter(|child| is_setting_element(child)) {
        if has_identity_attributes(child) {
            continue;
        }
        let setting = NotificationLevel::from_element_name(child.name())
            .expect("is_setting_element guarantees a known setting name");
        if fallback.replace(setting).is_some() {
            return Err(NotificationSettingsError::MultipleFallbackSettings);
        }
    }

    Ok(fallback)
}

/// Parse the account-wide fallback notification setting from `<notify/>`.
///
/// This compatibility wrapper returns `None` for malformed or non-XEP-0492
/// input. New code that needs diagnostics should call
/// [`parse_notify_fallback_setting`].
pub fn parse_notification_setting(elem: &Element) -> Option<NotificationLevel> {
    parse_notify_fallback_setting(elem).ok().flatten()
}

/// Parse the Waddle rich-payload opt-in from a XEP-0492 `<notify/>`.
///
/// Returns `true` when the account-wide fallback setting carries an
/// `<advanced/>` child holding `<rich-payload xmlns='urn:waddle:push:rich:0'/>`
/// (see [`NS_PUSH_RICH_PAYLOAD`]). Absence — the default — returns
/// `false`, preserving the minimal XEP-0357 summary payload. Malformed
/// or non-XEP-0492 input also returns `false`.
pub fn parse_rich_payload_opt_in(notify: &Element) -> bool {
    if !is_notify_element(notify) {
        return false;
    }
    notify
        .children()
        .filter(|child| is_setting_element(child) && !has_identity_attributes(child))
        .filter_map(|setting| setting.get_child("advanced", NS_NOTIFICATION_SETTINGS))
        .any(|advanced| advanced.has_child("rich-payload", NS_PUSH_RICH_PAYLOAD))
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a XEP-0492 notification child element.
pub fn build_notification_setting_child(level: NotificationLevel) -> Element {
    Element::builder(level.element_name(), NS_NOTIFICATION_SETTINGS).build()
}

/// Build a XEP-0492 `<notify/>` element with an account-wide fallback child.
pub fn build_notify_element(level: NotificationLevel) -> Element {
    Element::builder("notify", NS_NOTIFICATION_SETTINGS)
        .append(build_notification_setting_child(level))
        .build()
}

/// Build a XEP-0492 `<notify/>` element with an account-wide fallback child.
pub fn build_notification_settings_element(level: NotificationLevel) -> Element {
    build_notify_element(level)
}

/// Build the `<advanced/>` child carrying the Waddle rich-payload opt-in.
///
/// Shapes the XEP-0492 §2.3 `<advanced/>` container with a single
/// `<rich-payload xmlns='urn:waddle:push:rich:0'/>` child
/// (see [`NS_PUSH_RICH_PAYLOAD`]). The `<advanced/>` element is kept
/// non-empty per XEP-0492 §2.3.
pub fn build_rich_payload_advanced() -> Element {
    Element::builder("advanced", NS_NOTIFICATION_SETTINGS)
        .append(Element::builder("rich-payload", NS_PUSH_RICH_PAYLOAD).build())
        .build()
}

/// Build a XEP-0492 `<notify/>` whose fallback setting opts in to the
/// Waddle rich XEP-0357 push summary.
pub fn build_notify_element_with_rich_payload(level: NotificationLevel) -> Element {
    let mut fallback = build_notification_setting_child(level);
    fallback.append_child(build_rich_payload_advanced());
    Element::builder("notify", NS_NOTIFICATION_SETTINGS)
        .append(fallback)
        .build()
}

/// Replace the account-wide fallback setting while preserving unknown
/// `<advanced/>` payloads attached to the previous fallback setting.
pub fn replace_fallback_notification_setting(
    notify: &Element,
    level: NotificationLevel,
) -> Result<Element, NotificationSettingsError> {
    validate_notify_element(notify)?;

    let mut next = Element::builder("notify", NS_NOTIFICATION_SETTINGS).build();
    let mut fallback_advanced = Vec::new();
    let mut saw_fallback = false;

    for child in notify.children() {
        if is_setting_element(child) {
            validate_identity_attributes(child)?;
            if !has_identity_attributes(child) {
                if saw_fallback {
                    return Err(NotificationSettingsError::MultipleFallbackSettings);
                }
                saw_fallback = true;
                fallback_advanced.extend(
                    child
                        .children()
                        .filter(|grandchild| grandchild.is("advanced", NS_NOTIFICATION_SETTINGS))
                        .cloned(),
                );
                continue;
            }
        }
        next.append_child(child.clone());
    }

    let mut fallback = build_notification_setting_child(level);
    for advanced in fallback_advanced {
        fallback.append_child(advanced);
    }
    next.append_child(fallback);
    Ok(next)
}

fn is_setting_element(elem: &Element) -> bool {
    elem.ns() == NS_NOTIFICATION_SETTINGS
        && NotificationLevel::from_element_name(elem.name()).is_some()
}

/// Validate the XEP-0492 `<notify/>` wire shape.
///
/// Unknown client-specific settings belong inside `<advanced/>` and therefore
/// use a namespace other than `urn:xmpp:notification-settings:1`. If Waddle
/// accepts the official namespace, it must enforce the official element shape.
pub fn validate_notify_element(elem: &Element) -> Result<(), NotificationSettingsError> {
    if !is_notify_element(elem) {
        return Err(NotificationSettingsError::NotNotifyElement);
    }

    let mut saw_fallback = false;
    let mut seen_identity_settings = HashSet::new();
    for child in elem.children() {
        if !is_setting_element(child) {
            return Err(NotificationSettingsError::InvalidNotifyChild(
                child.name().to_string(),
            ));
        }

        validate_identity_attributes(child)?;
        if !has_identity_attributes(child) {
            if saw_fallback {
                return Err(NotificationSettingsError::MultipleFallbackSettings);
            }
            saw_fallback = true;
        } else if !seen_identity_settings.insert((
            child.name(),
            child.attr("identity-category"),
            child.attr("identity-type"),
        )) {
            return Err(NotificationSettingsError::DuplicateNotificationSetting);
        }
        validate_setting_children(child)?;
    }

    Ok(())
}

fn validate_setting_children(elem: &Element) -> Result<(), NotificationSettingsError> {
    let mut saw_advanced = false;
    for child in elem.children() {
        if !child.is("advanced", NS_NOTIFICATION_SETTINGS) {
            return Err(NotificationSettingsError::InvalidSettingChild(
                child.name().to_string(),
            ));
        }
        if saw_advanced {
            return Err(NotificationSettingsError::MultipleAdvancedElements);
        }
        saw_advanced = true;
        for grandchild in child.children() {
            if grandchild.ns() == NS_NOTIFICATION_SETTINGS {
                return Err(NotificationSettingsError::InvalidSettingChild(
                    grandchild.name().to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn has_identity_attributes(elem: &Element) -> bool {
    elem.attr("identity-category").is_some() || elem.attr("identity-type").is_some()
}

fn validate_identity_attributes(elem: &Element) -> Result<(), NotificationSettingsError> {
    if elem.attr("identity-type").is_some() && elem.attr("identity-category").is_none() {
        return Err(NotificationSettingsError::IdentityTypeWithoutCategory);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_level_from_element_name() {
        assert_eq!(
            NotificationLevel::from_element_name("always"),
            Some(NotificationLevel::Always)
        );
        assert_eq!(
            NotificationLevel::from_element_name("on-mention"),
            Some(NotificationLevel::OnMention)
        );
        assert_eq!(
            NotificationLevel::from_element_name("never"),
            Some(NotificationLevel::Never)
        );
        assert_eq!(NotificationLevel::from_element_name("invalid"), None);
    }

    #[test]
    fn test_notification_level_element_name() {
        assert_eq!(NotificationLevel::Always.element_name(), "always");
        assert_eq!(NotificationLevel::OnMention.element_name(), "on-mention");
        assert_eq!(NotificationLevel::Never.element_name(), "never");
    }

    #[test]
    fn test_notification_level_display() {
        assert_eq!(NotificationLevel::Always.to_string(), "always");
        assert_eq!(NotificationLevel::Never.to_string(), "never");
    }

    #[test]
    fn test_notification_level_default() {
        assert_eq!(NotificationLevel::default(), NotificationLevel::Always);
    }

    #[test]
    fn test_should_notify() {
        // Always: always notify
        assert!(NotificationLevel::Always.should_notify(false));
        assert!(NotificationLevel::Always.should_notify(true));

        // Mentions only: only when mentioned
        assert!(!NotificationLevel::OnMention.should_notify(false));
        assert!(NotificationLevel::OnMention.should_notify(true));

        // Never: never notify
        assert!(!NotificationLevel::Never.should_notify(false));
        assert!(!NotificationLevel::Never.should_notify(true));
    }

    #[test]
    fn test_is_suppressed() {
        assert!(!NotificationLevel::Always.is_suppressed());
        assert!(!NotificationLevel::OnMention.is_suppressed());
        assert!(NotificationLevel::Never.is_suppressed());
    }

    #[test]
    fn test_notification_settings() {
        let mut settings = NotificationSettings::new();

        // Default is Always
        assert_eq!(settings.get("room@muc"), NotificationLevel::Always);
        assert!(settings.should_notify("room@muc", false));

        // Set mentions-only
        settings.set("room@muc", NotificationLevel::OnMention);
        assert_eq!(settings.get("room@muc"), NotificationLevel::OnMention);
        assert!(!settings.should_notify("room@muc", false));
        assert!(settings.should_notify("room@muc", true));

        // Set never
        settings.set("noisy@muc", NotificationLevel::Never);
        assert!(!settings.should_notify("noisy@muc", true));

        // Overrides
        assert_eq!(settings.overrides().len(), 2);
        assert_eq!(settings.muted_rooms(), vec!["noisy@muc"]);

        // Remove override
        settings.remove("room@muc");
        assert_eq!(settings.get("room@muc"), NotificationLevel::Always);
    }

    #[test]
    fn test_set_default_removes_override() {
        let mut settings = NotificationSettings::new();
        settings.set("room@muc", NotificationLevel::Never);
        assert_eq!(settings.overrides().len(), 1);

        settings.set("room@muc", NotificationLevel::Always);
        assert_eq!(settings.overrides().len(), 0);
    }

    #[test]
    fn test_build_and_parse() {
        let elem = build_notification_settings_element(NotificationLevel::OnMention);
        assert_eq!(elem.name(), "notify");
        assert_eq!(elem.ns(), NS_NOTIFICATION_SETTINGS);

        let parsed = parse_notification_setting(&elem);
        assert_eq!(parsed, Some(NotificationLevel::OnMention));
    }

    #[test]
    fn test_is_notification_settings_element() {
        let elem = build_notification_settings_element(NotificationLevel::Always);
        assert!(is_notification_settings_element(&elem));

        let wrong = Element::builder("notify", "jabber:client").build();
        assert!(!is_notification_settings_element(&wrong));
    }

    #[test]
    fn test_room_notification_setting() {
        let s = RoomNotificationSetting::new("room@muc", NotificationLevel::Never);
        assert_eq!(s.room_jid, "room@muc");
        assert_eq!(s.level, NotificationLevel::Never);
    }

    #[test]
    fn parses_xep_0492_fallback_notify_element() {
        let elem: Element = "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>"
            .parse()
            .expect("valid notify");

        assert_eq!(
            parse_notify_fallback_setting(&elem),
            Ok(Some(NotificationLevel::Never))
        );
    }

    #[test]
    fn ignores_identity_specific_settings_when_reading_fallback() {
        let elem: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <never identity-category='client' identity-type='pc' />\
                <on-mention identity-category='client' />\
                <always />\
            </notify>"
            .parse()
            .expect("valid notify");

        assert_eq!(
            parse_notify_fallback_setting(&elem),
            Ok(Some(NotificationLevel::Always))
        );
    }

    #[test]
    fn rejects_identity_type_without_identity_category() {
        let elem: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <never identity-type='pc' />\
            </notify>"
            .parse()
            .expect("valid notify");

        assert_eq!(
            parse_notify_fallback_setting(&elem),
            Err(NotificationSettingsError::IdentityTypeWithoutCategory)
        );
    }

    #[test]
    fn rejects_multiple_fallback_settings() {
        let elem: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <always />\
                <never />\
            </notify>"
            .parse()
            .expect("valid notify");

        assert_eq!(
            parse_notify_fallback_setting(&elem),
            Err(NotificationSettingsError::MultipleFallbackSettings)
        );
    }

    #[test]
    fn parses_rich_payload_opt_in_from_advanced_child() {
        let elem: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <always>\
                    <advanced>\
                        <rich-payload xmlns='urn:waddle:push:rich:0' />\
                    </advanced>\
                </always>\
            </notify>"
            .parse()
            .expect("valid notify");

        assert!(parse_rich_payload_opt_in(&elem));
    }

    #[test]
    fn rich_payload_opt_in_round_trips_through_builder() {
        let notify = build_notify_element_with_rich_payload(NotificationLevel::OnMention);

        // The XEP-0492 wire shape still validates: the rich-payload lives
        // inside `<advanced/>` under a non-XEP-0492 namespace.
        assert_eq!(validate_notify_element(&notify), Ok(()));
        assert_eq!(
            parse_notify_fallback_setting(&notify),
            Ok(Some(NotificationLevel::OnMention))
        );
        assert!(parse_rich_payload_opt_in(&notify));
    }

    #[test]
    fn rich_payload_opt_in_absent_when_no_advanced_extension() {
        let elem: Element = "<notify xmlns='urn:xmpp:notification-settings:1'><always /></notify>"
            .parse()
            .expect("valid notify");

        assert!(!parse_rich_payload_opt_in(&elem));
    }

    #[test]
    fn replace_fallback_preserves_advanced_payload() {
        let elem: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
                <never identity-category='client' identity-type='pc'>\
                    <advanced><custom xmlns='urn:example:custom' /></advanced>\
                </never>\
                <always>\
                    <advanced><preview xmlns='urn:waddle:test' mode='quiet' /></advanced>\
                </always>\
            </notify>"
            .parse()
            .expect("valid notify");

        let updated = replace_fallback_notification_setting(&elem, NotificationLevel::OnMention)
            .expect("updated notify");

        assert_eq!(
            parse_notify_fallback_setting(&updated),
            Ok(Some(NotificationLevel::OnMention))
        );
        let fallback = updated
            .children()
            .find(|child| child.is("on-mention", NS_NOTIFICATION_SETTINGS))
            .expect("fallback on-mention child");
        let advanced = fallback
            .get_child("advanced", NS_NOTIFICATION_SETTINGS)
            .expect("advanced preserved");
        assert!(advanced.get_child("preview", "urn:waddle:test").is_some());
        assert!(updated
            .children()
            .any(|child| child.attr("identity-category") == Some("client")
                && child
                    .get_child("advanced", NS_NOTIFICATION_SETTINGS)
                    .is_some()));
    }
}
