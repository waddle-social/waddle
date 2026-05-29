//! XEP-0492: Chat Notification Settings dedicated suite.

use minidom::Element;
use waddle_xmpp::xep::{
    build_notification_settings_element, build_notify_element_with_rich_payload,
    build_rich_payload_advanced, is_notify_element, parse_notify_fallback_setting,
    parse_rich_payload_opt_in, replace_fallback_notification_setting, validate_notify_element,
    NotificationLevel, NotificationSettingsError, NS_NOTIFICATION_SETTINGS, NS_PUSH_RICH_PAYLOAD,
};

#[test]
fn xep0492_notify_uses_registered_namespace_and_child_shape() {
    let notify = build_notification_settings_element(NotificationLevel::OnMention);

    assert_eq!(notify.name(), "notify");
    assert_eq!(notify.ns(), NS_NOTIFICATION_SETTINGS);
    assert_eq!(notify.attr("level"), None);
    assert!(is_notify_element(&notify));
    assert!(notify
        .children()
        .any(|child| child.is("on-mention", NS_NOTIFICATION_SETTINGS)));
}

#[test]
fn xep0492_fallback_parse_ignores_identity_specific_settings() {
    let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
            <never identity-category='client' identity-type='pc' />\
            <on-mention identity-category='client' />\
            <always />\
        </notify>"
        .parse()
        .expect("valid XEP-0492 notify element");

    assert_eq!(
        parse_notify_fallback_setting(&notify),
        Ok(Some(NotificationLevel::Always))
    );
}

#[test]
fn xep0492_identity_type_requires_identity_category() {
    let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
            <never identity-type='pc' />\
        </notify>"
        .parse()
        .expect("valid XML");

    assert_eq!(
        parse_notify_fallback_setting(&notify),
        Err(NotificationSettingsError::IdentityTypeWithoutCategory)
    );
}

#[test]
fn xep0492_rejects_duplicate_identity_specific_settings() {
    let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
            <never identity-category='client' identity-type='pc' />\
            <never identity-category='client' identity-type='pc' />\
        </notify>"
        .parse()
        .expect("valid XML");

    assert_eq!(
        validate_notify_element(&notify),
        Err(NotificationSettingsError::DuplicateNotificationSetting)
    );
}

#[test]
fn xep0492_rejects_unknown_official_notify_children() {
    let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
            <sometimes />\
        </notify>"
        .parse()
        .expect("valid XML");

    assert_eq!(
        validate_notify_element(&notify),
        Err(NotificationSettingsError::InvalidNotifyChild(
            "sometimes".to_string()
        ))
    );
}

#[test]
fn xep0492_update_preserves_unknown_advanced_payloads() {
    let notify: Element = "<notify xmlns='urn:xmpp:notification-settings:1'>\
            <never identity-category='client' identity-type='pc'>\
                <advanced><device-rule xmlns='urn:example:device' /></advanced>\
            </never>\
            <always>\
                <advanced><quiet-hours xmlns='urn:example:quiet' /></advanced>\
            </always>\
        </notify>"
        .parse()
        .expect("valid XEP-0492 notify element");

    let updated = replace_fallback_notification_setting(&notify, NotificationLevel::OnMention)
        .expect("fallback update succeeds");

    let fallback = updated
        .children()
        .find(|child| child.is("on-mention", NS_NOTIFICATION_SETTINGS))
        .expect("updated fallback setting");
    let fallback_advanced = fallback
        .get_child("advanced", NS_NOTIFICATION_SETTINGS)
        .expect("fallback advanced payload preserved");
    assert!(fallback_advanced
        .get_child("quiet-hours", "urn:example:quiet")
        .is_some());

    let identity_specific = updated
        .children()
        .find(|child| child.attr("identity-category") == Some("client"))
        .expect("identity-specific setting preserved");
    assert!(identity_specific
        .get_child("advanced", NS_NOTIFICATION_SETTINGS)
        .and_then(|advanced| advanced.get_child("device-rule", "urn:example:device"))
        .is_some());
}

// ── Waddle rich-payload <advanced/> extension (#719) ────────────────

#[test]
fn waddle_rich_payload_opt_in_lives_in_xep0492_advanced_under_waddle_ns() {
    let notify = build_notify_element_with_rich_payload(NotificationLevel::Always);

    // The opt-in is housed in the XEP-0492 §2.3 <advanced/> container,
    // and the Waddle semantics use a urn:waddle:* namespace — never the
    // official XEP-0492 namespace.
    assert_eq!(validate_notify_element(&notify), Ok(()));
    let fallback = notify
        .children()
        .find(|child| child.is("always", NS_NOTIFICATION_SETTINGS))
        .expect("fallback setting");
    let advanced = fallback
        .get_child("advanced", NS_NOTIFICATION_SETTINGS)
        .expect("advanced container in XEP-0492 namespace");
    let rich = advanced
        .get_child("rich-payload", NS_PUSH_RICH_PAYLOAD)
        .expect("rich-payload child under urn:waddle namespace");
    assert_eq!(rich.ns(), "urn:waddle:push:rich:0");
    assert!(parse_rich_payload_opt_in(&notify));
}

#[test]
fn waddle_rich_payload_advanced_is_non_empty_per_xep0492() {
    // XEP-0492 §2.3: the <advanced/> element SHOULD NOT be empty.
    let advanced = build_rich_payload_advanced();
    assert_eq!(advanced.name(), "advanced");
    assert_eq!(advanced.ns(), NS_NOTIFICATION_SETTINGS);
    assert!(advanced.children().next().is_some());
}

#[test]
fn absent_rich_payload_advanced_parses_as_opt_out() {
    let notify = build_notification_settings_element(NotificationLevel::Always);
    assert!(!parse_rich_payload_opt_in(&notify));
}
