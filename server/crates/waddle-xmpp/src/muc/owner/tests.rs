use super::{
    build_config_form, MucRoom, RoomConfig, DATA_FORMS_NS, FIELD_FEDERATED, FIELD_FORUM_MODE,
    FIELD_PIN_PERMISSION,
};

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

/// The value the owner-config form offers for the `federated` field of a
/// room configured as `config`.
fn offered_federated(config: RoomConfig) -> String {
    let room = MucRoom::new(
        "room@muc.example.com".parse().unwrap(),
        "waddle-123".to_string(),
        "channel-456".to_string(),
        config,
    );
    let form = build_config_form(&room);
    let field = form
        .children()
        .find(|c| c.attr("var") == Some(FIELD_FEDERATED))
        .expect("the federated field should exist");
    assert_eq!(field.attr("type"), Some("boolean"));
    field
        .get_child("value", DATA_FORMS_NS)
        .expect("the federated field should contain a value")
        .text()
}

/// XEP-0045 §10.1.2: the form offers the `federated` field with the
/// room's current setting, which is off unless it was turned on.
#[test]
fn the_config_form_offers_the_federated_setting() {
    assert_eq!(offered_federated(RoomConfig::default()), "0");
    assert_eq!(
        offered_federated(RoomConfig {
            federated: true,
            ..Default::default()
        }),
        "1"
    );
}

/// A configuration stored before the `federated` field existed reads as
/// not federated, and a group DM is never federated.
#[test]
fn a_stored_room_config_without_the_federated_field_is_not_federated() {
    let mut value =
        serde_json::to_value(RoomConfig::default()).expect("default RoomConfig serializes");
    let obj = value.as_object_mut().expect("object");
    assert!(obj.remove("federated").is_some());
    let config: RoomConfig =
        serde_json::from_value(value).expect("RoomConfig without federated must deserialize");
    assert!(!config.federated);

    let group_dm = RoomConfig {
        group_dm: true,
        federated: true,
        ..Default::default()
    }
    .normalized();
    assert!(!group_dm.federated);
    assert!(group_dm.members_only);
}
