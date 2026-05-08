use super::*;

fn make_owner_get_iq(room_jid: &str) -> Iq {
    let query = Element::builder("query", NS_MUC_OWNER).build();

    Iq {
        from: Some("owner@example.com/res".parse().unwrap()),
        to: Some(room_jid.parse().unwrap()),
        id: "config-get-1".to_string(),
        payload: IqType::Get(query),
    }
}

fn make_owner_set_iq(room_jid: &str, form: Element) -> Iq {
    let query = Element::builder("query", NS_MUC_OWNER).append(form).build();

    Iq {
        from: Some("owner@example.com/res".parse().unwrap()),
        to: Some(room_jid.parse().unwrap()),
        id: "config-set-1".to_string(),
        payload: IqType::Set(query),
    }
}

fn make_destroy_iq(room_jid: &str, reason: Option<&str>, alternate: Option<&str>) -> Iq {
    let mut destroy = Element::builder("destroy", NS_MUC_OWNER);

    if let Some(alt) = alternate {
        destroy = destroy.attr("jid", alt);
    }

    if let Some(r) = reason {
        destroy = destroy.append(Element::builder("reason", NS_MUC_OWNER).append(r).build());
    }

    let query = Element::builder("query", NS_MUC_OWNER)
        .append(destroy.build())
        .build();

    Iq {
        from: Some("owner@example.com/res".parse().unwrap()),
        to: Some(room_jid.parse().unwrap()),
        id: "destroy-1".to_string(),
        payload: IqType::Set(query),
    }
}

fn make_config_form() -> Element {
    Element::builder("x", DATA_FORMS_NS)
        .attr("type", "submit")
        .append(build_field_hidden("FORM_TYPE", MUC_ROOMCONFIG_NS))
        .append(build_field_text_single(
            "muc#roomconfig_roomname",
            "Room Name",
            "Test Room",
        ))
        .append(build_field_text_single(
            "muc#roomconfig_roomdesc",
            "Description",
            "A test room",
        ))
        .append(build_field_boolean(
            "muc#roomconfig_persistentroom",
            "Persistent",
            true,
        ))
        .append(build_field_boolean(
            "muc#roomconfig_membersonly",
            "Members Only",
            false,
        ))
        .append(build_field_boolean(
            "muc#roomconfig_moderatedroom",
            "Moderated",
            true,
        ))
        .append(build_field_text_single(
            "muc#roomconfig_maxusers",
            "Max Users",
            "50",
        ))
        .append(build_field_boolean(
            "muc#roomconfig_enablelogging",
            "Logging",
            true,
        ))
        .append(build_field_boolean(FIELD_FORUM_MODE, "Forum Mode", true))
        .build()
}

#[test]
fn test_parse_owner_get() {
    let iq = make_owner_get_iq("room@muc.example.com");
    let query = parse_owner_query(&iq, "muc.example.com").unwrap();

    assert_eq!(query.room_jid.to_string(), "room@muc.example.com");
    assert!(matches!(query.action, OwnerAction::GetConfig));
}

#[test]
fn test_parse_owner_set_config() {
    let form = make_config_form();
    let iq = make_owner_set_iq("room@muc.example.com", form);
    let query = parse_owner_query(&iq, "muc.example.com").unwrap();

    assert_eq!(query.room_jid.to_string(), "room@muc.example.com");

    match query.action {
        OwnerAction::SetConfig(config) => {
            assert_eq!(config.name.as_deref(), Some("Test Room"));
            assert_eq!(config.description.as_deref(), Some("A test room"));
            assert_eq!(config.persistent, Some(true));
            assert_eq!(config.members_only, Some(false));
            assert_eq!(config.moderated, Some(true));
            assert_eq!(config.max_occupants, Some(50));
            assert_eq!(config.enable_logging, Some(true));
            assert_eq!(config.forum, Some(true));
        }
        _ => panic!("Expected SetConfig action"),
    }
}

#[test]
fn test_parse_owner_destroy() {
    let iq = make_destroy_iq(
        "room@muc.example.com",
        Some("Room no longer needed"),
        Some("newroom@muc.example.com"),
    );
    let query = parse_owner_query(&iq, "muc.example.com").unwrap();

    match query.action {
        OwnerAction::Destroy(request) => {
            assert_eq!(request.reason.as_deref(), Some("Room no longer needed"));
            assert_eq!(
                request.alternate_venue.as_ref().map(|j| j.to_string()),
                Some("newroom@muc.example.com".to_string())
            );
        }
        _ => panic!("Expected Destroy action"),
    }
}

#[test]
fn test_parse_boolean_via_field() {
    use crate::xep::xep0004::Field;

    // "1" and "true" should be true
    assert_eq!(Field::boolean("t", true).value_as_bool(), Some(true));
    let f = Field::new("t", crate::xep::xep0004::FieldType::Boolean).with_value("true");
    assert_eq!(f.value_as_bool(), Some(true));

    // "0" and "false" should be false
    assert_eq!(Field::boolean("t", false).value_as_bool(), Some(false));
    let f = Field::new("t", crate::xep::xep0004::FieldType::Boolean).with_value("false");
    assert_eq!(f.value_as_bool(), Some(false));

    // Empty string should be false
    let f = Field::new("t", crate::xep::xep0004::FieldType::Boolean).with_value("");
    assert_eq!(f.value_as_bool(), Some(false));
}

#[test]
fn test_apply_config_form() {
    let mut config = RoomConfig::default();
    let form_data = ConfigFormData {
        name: Some("Updated Room".to_string()),
        description: Some("New description".to_string()),
        persistent: Some(false),
        members_only: Some(true),
        moderated: Some(true),
        max_occupants: Some(100),
        enable_logging: Some(false),
        forum: Some(true),
    };

    apply_config_form(&mut config, &form_data);

    assert_eq!(config.name, "Updated Room");
    assert_eq!(config.description.as_deref(), Some("New description"));
    assert!(!config.persistent);
    assert!(config.members_only);
    assert!(config.moderated);
    assert_eq!(config.max_occupants, 100);
    assert!(!config.enable_logging);
    assert!(config.forum);
}

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

#[test]
fn test_build_config_result() {
    let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
    let to_jid: Jid = "owner@example.com/res".parse().unwrap();

    let form = Element::builder("x", DATA_FORMS_NS).build();
    let result = build_config_result("test-1", &room_jid, &to_jid, form);

    assert_eq!(result.id, "test-1");
    assert!(matches!(result.payload, IqType::Result(Some(_))));
}

#[test]
fn test_build_owner_set_result() {
    let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
    let to_jid: Jid = "owner@example.com/res".parse().unwrap();

    let result = build_owner_set_result("test-2", &room_jid, &to_jid);

    assert_eq!(result.id, "test-2");
    assert!(matches!(result.payload, IqType::Result(None)));
}

#[test]
fn test_build_destroy_notification() {
    let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
    let occupant_jid: jid::FullJid = "user@example.com/res".parse().unwrap();

    let request = DestroyRequest {
        reason: Some("Room closed".to_string()),
        alternate_venue: Some("newroom@muc.example.com".parse().unwrap()),
        password: None,
    };

    let presence = build_destroy_notification(&room_jid, "user", &occupant_jid, &request, true);

    assert!(matches!(
        presence.type_,
        xmpp_parsers::presence::Type::Unavailable
    ));
    assert!(presence.from.is_some());
    assert!(presence.to.is_some());

    // Verify the x element contains destroy
    let x_elem = presence
        .payloads
        .iter()
        .find(|p| p.name() == "x" && p.ns() == "http://jabber.org/protocol/muc#user")
        .expect("Should have muc#user x element");

    let destroy = x_elem
        .get_child("destroy", "http://jabber.org/protocol/muc#user")
        .expect("Should have destroy element");
    assert_eq!(destroy.attr("jid"), Some("newroom@muc.example.com"));

    let reason = destroy
        .get_child("reason", "http://jabber.org/protocol/muc#user")
        .expect("Should have reason element");
    assert_eq!(reason.text(), "Room closed");
}
