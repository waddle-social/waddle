use super::*;

#[test]
fn test_build_config_form() {
    let room = MucRoom::new(
        "room@muc.example.com".parse().unwrap(),
        "waddle-123".to_string(),
        "channel-456".to_string(),
        RoomConfig {
            name: "My Room".to_string(),
            description: Some("A great room".to_string()),
            persistent: true,
            members_only: true,
            moderated: false,
            max_occupants: 25,
            enable_logging: true,
            forum: true,
            ..Default::default()
        },
    );

    let form = build_config_form(&room);

    assert_eq!(form.name(), "x");
    assert_eq!(form.ns(), DATA_FORMS_NS);
    assert_eq!(form.attr("type"), Some("form"));

    // Verify FORM_TYPE field exists
    let form_type = form
        .children()
        .find(|c| c.attr("var") == Some("FORM_TYPE"))
        .expect("FORM_TYPE field should exist");
    assert_eq!(form_type.attr("type"), Some("hidden"));

    let forum_field = form
        .children()
        .find(|c| c.attr("var") == Some(FIELD_FORUM_MODE))
        .expect("forum field should exist");
    let forum_value = forum_field
        .get_child("value", DATA_FORMS_NS)
        .expect("forum field should contain a value");
    assert_eq!(forum_value.text(), "1");

    // #415: pin permission field is a list-single with both options
    // enumerated, defaulting to admins-only for fresh rooms.
    let pin_field = form
        .children()
        .find(|c| c.attr("var") == Some(FIELD_PIN_PERMISSION))
        .expect("pin permission field should exist");
    assert_eq!(pin_field.attr("type"), Some("list-single"));
    let pin_value = pin_field
        .get_child("value", DATA_FORMS_NS)
        .expect("pin permission field should contain a value");
    assert_eq!(pin_value.text(), "admins-only");
    let option_values: Vec<String> = pin_field
        .children()
        .filter(|c| c.is("option", DATA_FORMS_NS))
        .filter_map(|opt| opt.get_child("value", DATA_FORMS_NS).map(|v| v.text()))
        .collect();
    assert_eq!(option_values, vec!["admins-only", "anyone"]);

    let mention_count = form
        .children()
        .find(|c| c.attr("var") == Some(crate::xep::FIELD_MENTIONS_COUNT))
        .expect("mention count field should exist");
    assert_eq!(mention_count.attr("type"), Some("text-single"));
    assert_eq!(
        mention_count
            .get_child("value", DATA_FORMS_NS)
            .map(|value| value.text()),
        Some("5".to_string())
    );
    let mention_channel = form
        .children()
        .find(|c| c.attr("var") == Some(crate::xep::FIELD_MENTIONS_CHANNEL))
        .expect("mention channel permission field should exist");
    assert_eq!(mention_channel.attr("type"), Some("list-single"));
    assert_eq!(
        mention_channel
            .get_child("value", DATA_FORMS_NS)
            .map(|value| value.text()),
        Some("participants".to_string())
    );
}

/// #415: rooms persisted before the pin_permission field was added
/// must deserialize to the `AdminsOnly` default — locks the
/// `#[serde(default)]` contract on `RoomConfig::pin_permission`.
#[test]
fn legacy_room_config_deserializes_to_admins_only() {
    let mut value =
        serde_json::to_value(RoomConfig::default()).expect("default RoomConfig serializes");
    let obj = value.as_object_mut().expect("object");
    assert!(
        obj.remove("pin_permission").is_some(),
        "fixture must drop pin_permission to simulate pre-#415 on-disk shape"
    );
    let config: RoomConfig =
        serde_json::from_value(value).expect("RoomConfig sans pin_permission must deserialize");
    assert_eq!(
        config.pin_permission,
        super::super::pin::PinPermission::AdminsOnly
    );
}

/// Rooms persisted before mention permissions were added must deserialize to
/// the XEP-0513 default permission set.
#[test]
fn legacy_room_config_deserializes_to_default_mention_permissions() {
    let mut value =
        serde_json::to_value(RoomConfig::default()).expect("default RoomConfig serializes");
    let obj = value.as_object_mut().expect("object");
    assert!(
        obj.remove("mention_permissions").is_some(),
        "fixture must drop mention_permissions to simulate pre-XEP-0513 on-disk shape"
    );
    let config: RoomConfig = serde_json::from_value(value)
        .expect("RoomConfig sans mention_permissions must deserialize");
    assert_eq!(
        config.mention_permissions,
        crate::xep::MentionPermissions::default()
    );
}
