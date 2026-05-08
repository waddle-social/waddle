use super::*;

use minidom::Element;

// ---- FormType ----

#[test]
fn test_form_type_round_trip() {
    for (s, expected) in [
        ("form", FormType::Form),
        ("submit", FormType::Submit),
        ("cancel", FormType::Cancel),
        ("result", FormType::Result),
    ] {
        let parsed: FormType = s.parse().expect(s);
        assert_eq!(parsed, expected);
        assert_eq!(parsed.as_str(), s);
        assert_eq!(parsed.to_string(), s);
    }
}

#[test]
fn test_form_type_invalid() {
    assert!("invalid".parse::<FormType>().is_err());
}

// ---- FieldType ----

#[test]
fn test_field_type_round_trip() {
    for s in &[
        "boolean",
        "fixed",
        "hidden",
        "jid-multi",
        "jid-single",
        "list-multi",
        "list-single",
        "text-multi",
        "text-private",
        "text-single",
    ] {
        let parsed: FieldType = s.parse().expect(s);
        assert_eq!(parsed.as_str(), *s);
    }
}

#[test]
fn test_field_type_is_multi() {
    assert!(FieldType::JidMulti.is_multi());
    assert!(FieldType::ListMulti.is_multi());
    assert!(FieldType::TextMulti.is_multi());
    assert!(!FieldType::TextSingle.is_multi());
    assert!(!FieldType::Boolean.is_multi());
    assert!(!FieldType::ListSingle.is_multi());
}

#[test]
fn test_field_type_default() {
    assert_eq!(FieldType::default(), FieldType::TextSingle);
}

// ---- Field constructors ----

#[test]
fn test_field_hidden() {
    let f = Field::hidden("FORM_TYPE", "urn:example");
    assert_eq!(f.var.as_deref(), Some("FORM_TYPE"));
    assert_eq!(f.field_type, FieldType::Hidden);
    assert_eq!(f.value(), Some("urn:example"));
}

#[test]
fn test_field_form_type() {
    let f = Field::form_type("urn:xmpp:mam:2");
    assert_eq!(f.var.as_deref(), Some("FORM_TYPE"));
    assert_eq!(f.value(), Some("urn:xmpp:mam:2"));
}

#[test]
fn test_field_text_single() {
    let f = Field::text_single("name", "Alice");
    assert_eq!(f.field_type, FieldType::TextSingle);
    assert_eq!(f.value(), Some("Alice"));
}

#[test]
fn test_field_boolean() {
    let t = Field::boolean("flag", true);
    assert_eq!(t.value(), Some("1"));
    assert_eq!(t.value_as_bool(), Some(true));

    let f = Field::boolean("flag", false);
    assert_eq!(f.value(), Some("0"));
    assert_eq!(f.value_as_bool(), Some(false));
}

#[test]
fn test_field_fixed() {
    let f = Field::fixed("Please fill in the form.");
    assert!(f.var.is_none());
    assert_eq!(f.field_type, FieldType::Fixed);
    assert_eq!(f.value(), Some("Please fill in the form."));
}

#[test]
fn test_field_builder_methods() {
    let f = Field::new("color", FieldType::ListSingle)
        .with_label("Favorite Color")
        .with_desc("Choose one")
        .with_required()
        .with_value("red")
        .add_option(FieldOption::with_label("Red", "red"))
        .add_option(FieldOption::with_label("Blue", "blue"));

    assert_eq!(f.label.as_deref(), Some("Favorite Color"));
    assert_eq!(f.desc.as_deref(), Some("Choose one"));
    assert!(f.required);
    assert_eq!(f.value(), Some("red"));
    assert_eq!(f.options.len(), 2);
}

#[test]
fn test_field_add_value() {
    let f = Field::new("items", FieldType::ListMulti)
        .add_value("a")
        .add_value("b")
        .add_value("c");

    assert_eq!(f.values, vec!["a", "b", "c"]);
}

// ---- DataForm constructors ----

#[test]
fn test_data_form_builder() {
    let form = DataForm::new(FormType::Form)
        .with_title("Registration")
        .add_instructions("Fill in the fields below.")
        .add_field(Field::form_type("jabber:iq:register"))
        .add_field(
            Field::new("username", FieldType::TextSingle)
                .with_label("Username")
                .with_required(),
        )
        .add_field(
            Field::new("password", FieldType::TextPrivate)
                .with_label("Password")
                .with_required(),
        );

    assert_eq!(form.form_type, FormType::Form);
    assert_eq!(form.title.as_deref(), Some("Registration"));
    assert_eq!(form.instructions, vec!["Fill in the fields below."]);
    assert_eq!(form.fields.len(), 3);
    assert_eq!(form.get_form_type_value(), Some("jabber:iq:register"));
    assert_eq!(form.get_value("username"), None); // no value set
}

#[test]
fn test_data_form_get_bool() {
    let form = DataForm::new(FormType::Submit)
        .add_field(Field::boolean("persistent", true))
        .add_field(Field::boolean("moderated", false));

    assert_eq!(form.get_bool("persistent"), Some(true));
    assert_eq!(form.get_bool("moderated"), Some(false));
    assert_eq!(form.get_bool("nonexistent"), None);
}

#[test]
fn test_data_form_field_mut() {
    let mut form = DataForm::new(FormType::Form).add_field(Field::text_single("name", "old"));

    if let Some(f) = form.field_mut("name") {
        f.values = vec!["new".to_string()];
    }
    assert_eq!(form.get_value("name"), Some("new"));
}

// ---- Tabular (result) form ----

#[test]
fn test_data_form_tabular() {
    let form = DataForm::new(FormType::Result)
        .add_reported(Field::new("name", FieldType::TextSingle).with_label("Name"))
        .add_reported(Field::new("url", FieldType::TextSingle).with_label("URL"))
        .add_item(vec![
            Field::text_single("name", "Waddle"),
            Field::text_single("url", "https://waddle.social"),
        ])
        .add_item(vec![
            Field::text_single("name", "XMPP"),
            Field::text_single("url", "https://xmpp.org"),
        ]);

    assert_eq!(form.reported.len(), 2);
    assert_eq!(form.items.len(), 2);
    assert_eq!(form.items[0][0].value(), Some("Waddle"));
}

// ---- XML round-trip ----

#[test]
fn test_simple_form_round_trip() {
    let original = DataForm::new(FormType::Form)
        .with_title("Bot Configuration")
        .add_instructions("Fill out this form to configure your bot.")
        .add_field(Field::form_type("urn:example:bot"))
        .add_field(
            Field::new("botname", FieldType::TextSingle)
                .with_label("Bot Name")
                .with_required(),
        )
        .add_field(Field::boolean("public", true).with_label("Make public?"));

    let elem = original.into_element();
    let parsed = DataForm::from_element(&elem).expect("parse");

    assert_eq!(parsed.form_type, FormType::Form);
    assert_eq!(parsed.title, original.title);
    assert_eq!(parsed.instructions, original.instructions);
    assert_eq!(parsed.fields.len(), 3);
    assert_eq!(parsed.get_form_type_value(), Some("urn:example:bot"));
    assert_eq!(parsed.get_bool("public"), Some(true));
}

#[test]
fn test_submit_form_round_trip() {
    let original = DataForm::new(FormType::Submit)
        .add_field(Field::form_type("urn:example"))
        .add_field(Field::text_single("name", "Alice"))
        .add_field(Field::boolean("agree", true));

    let elem = original.into_element();
    let parsed = DataForm::from_element(&elem).expect("parse");

    assert_eq!(parsed.form_type, FormType::Submit);
    assert_eq!(parsed.get_value("name"), Some("Alice"));
    assert_eq!(parsed.get_bool("agree"), Some(true));
}

#[test]
fn test_list_field_round_trip() {
    let original = DataForm::new(FormType::Form).add_field(
        Field::new("color", FieldType::ListSingle)
            .with_label("Favorite Color")
            .with_value("red")
            .add_option(FieldOption::with_label("Red", "red"))
            .add_option(FieldOption::with_label("Green", "green"))
            .add_option(FieldOption::with_label("Blue", "blue")),
    );

    let elem = original.into_element();
    let parsed = DataForm::from_element(&elem).expect("parse");

    let field = parsed.field("color").expect("color field");
    assert_eq!(field.field_type, FieldType::ListSingle);
    assert_eq!(field.value(), Some("red"));
    assert_eq!(field.options.len(), 3);
    assert_eq!(field.options[0].label.as_deref(), Some("Red"));
    assert_eq!(field.options[0].value, "red");
}

#[test]
fn test_multi_value_field_round_trip() {
    let original = DataForm::new(FormType::Submit).add_field(
        Field::new("features", FieldType::ListMulti)
            .add_value("chat")
            .add_value("video")
            .add_value("voice"),
    );

    let elem = original.into_element();
    let parsed = DataForm::from_element(&elem).expect("parse");

    let values = parsed.get_values("features").expect("features values");
    assert_eq!(values, &["chat", "video", "voice"]);
}

#[test]
fn test_tabular_form_round_trip() {
    let original = DataForm::new(FormType::Result)
        .add_field(Field::form_type("jabber:iq:search"))
        .add_reported(Field::new("jid", FieldType::JidSingle).with_label("JID"))
        .add_reported(Field::new("name", FieldType::TextSingle).with_label("Name"))
        .add_item(vec![
            Field::text_single("jid", "alice@example.com"),
            Field::text_single("name", "Alice"),
        ])
        .add_item(vec![
            Field::text_single("jid", "bob@example.com"),
            Field::text_single("name", "Bob"),
        ]);

    let elem = original.into_element();
    let parsed = DataForm::from_element(&elem).expect("parse");

    assert_eq!(parsed.form_type, FormType::Result);
    assert_eq!(parsed.reported.len(), 2);
    assert_eq!(parsed.items.len(), 2);
    assert_eq!(parsed.items[0][0].value(), Some("alice@example.com"));
    assert_eq!(parsed.items[1][1].value(), Some("Bob"));
}

#[test]
fn test_required_and_desc_round_trip() {
    let original = DataForm::new(FormType::Form).add_field(
        Field::new("email", FieldType::TextSingle)
            .with_label("Email")
            .with_desc("Your email address")
            .with_required(),
    );

    let elem = original.into_element();
    let parsed = DataForm::from_element(&elem).expect("parse");

    let field = parsed.field("email").expect("email field");
    assert!(field.required);
    assert_eq!(field.desc.as_deref(), Some("Your email address"));
    assert_eq!(field.label.as_deref(), Some("Email"));
}

#[test]
fn test_cancel_form_round_trip() {
    let original = DataForm::new(FormType::Cancel);
    let elem = original.into_element();
    let parsed = DataForm::from_element(&elem).expect("parse");

    assert_eq!(parsed.form_type, FormType::Cancel);
    assert!(parsed.fields.is_empty());
}

// ---- Error cases ----

#[test]
fn test_not_a_data_form() {
    let elem = Element::builder("query", "jabber:iq:roster").build();
    assert!(matches!(
        DataForm::from_element(&elem),
        Err(DataFormError::NotADataForm)
    ));
}

#[test]
fn test_missing_type_attribute() {
    let elem = Element::builder("x", NS_DATA_FORMS).build();
    assert!(matches!(
        DataForm::from_element(&elem),
        Err(DataFormError::MissingElement(_))
    ));
}

#[test]
fn test_invalid_type_attribute() {
    let elem = Element::builder("x", NS_DATA_FORMS)
        .attr("type", "bogus")
        .build();
    assert!(matches!(
        DataForm::from_element(&elem),
        Err(DataFormError::InvalidFormType(_))
    ));
}

// ---- Utility functions ----

#[test]
fn test_is_data_form() {
    let good = Element::builder("x", NS_DATA_FORMS)
        .attr("type", "form")
        .build();
    assert!(is_data_form(&good));

    let bad_name = Element::builder("query", NS_DATA_FORMS).build();
    assert!(!is_data_form(&bad_name));

    let bad_ns = Element::builder("x", "jabber:iq:roster").build();
    assert!(!is_data_form(&bad_ns));
}

#[test]
fn test_find_data_form() {
    let parent = Element::builder("query", "urn:example")
        .append(
            Element::builder("x", NS_DATA_FORMS)
                .attr("type", "submit")
                .append(Field::text_single("name", "test").into_element())
                .build(),
        )
        .build();

    let form = find_data_form(&parent)
        .expect("should find form")
        .expect("should parse");

    assert_eq!(form.form_type, FormType::Submit);
    assert_eq!(form.get_value("name"), Some("test"));
}

#[test]
fn test_find_data_form_none() {
    let parent = Element::builder("query", "urn:example").build();
    assert!(find_data_form(&parent).is_none());
}

// ---- Compatibility with existing inline forms ----

#[test]
fn test_parse_hand_built_server_info_form() {
    // Simulate the kind of form built in disco/info.rs
    let elem = Element::builder("x", NS_DATA_FORMS)
        .attr("type", "result")
        .append(
            Element::builder("field", NS_DATA_FORMS)
                .attr("var", "FORM_TYPE")
                .attr("type", "hidden")
                .append(
                    Element::builder("value", NS_DATA_FORMS)
                        .append("urn:xmpp:serverinfo:0")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", NS_DATA_FORMS)
                .attr("var", "abuse-addresses")
                .append(
                    Element::builder("value", NS_DATA_FORMS)
                        .append("mailto:abuse@example.com")
                        .build(),
                )
                .build(),
        )
        .build();

    let form = DataForm::from_element(&elem).expect("parse");
    assert_eq!(form.form_type, FormType::Result);
    assert_eq!(form.get_form_type_value(), Some("urn:xmpp:serverinfo:0"));
    assert_eq!(
        form.get_value("abuse-addresses"),
        Some("mailto:abuse@example.com")
    );
}

#[test]
fn test_parse_muc_config_form() {
    // Simulate the kind of form used in muc/owner.rs
    let elem = Element::builder("x", NS_DATA_FORMS)
        .attr("type", "submit")
        .append(
            Element::builder("field", NS_DATA_FORMS)
                .attr("var", "FORM_TYPE")
                .attr("type", "hidden")
                .append(
                    Element::builder("value", NS_DATA_FORMS)
                        .append("http://jabber.org/protocol/muc#roomconfig")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", NS_DATA_FORMS)
                .attr("var", "muc#roomconfig_roomname")
                .append(
                    Element::builder("value", NS_DATA_FORMS)
                        .append("Test Room")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", NS_DATA_FORMS)
                .attr("var", "muc#roomconfig_persistentroom")
                .append(Element::builder("value", NS_DATA_FORMS).append("1").build())
                .build(),
        )
        .build();

    let form = DataForm::from_element(&elem).expect("parse");
    assert_eq!(form.form_type, FormType::Submit);
    assert_eq!(form.get_value("muc#roomconfig_roomname"), Some("Test Room"));
    assert_eq!(form.get_bool("muc#roomconfig_persistentroom"), Some(true));
}

// ---- Boolean value parsing edge cases ----

#[test]
fn test_boolean_value_variants() {
    // "true" and "1" should both be true
    for v in &["1", "true"] {
        let f = Field::new("test", FieldType::Boolean).with_value(*v);
        assert_eq!(f.value_as_bool(), Some(true), "expected true for {v}");
    }
    // "0", "false", and anything else should be false
    for v in &["0", "false", "maybe", ""] {
        let f = Field::new("test", FieldType::Boolean).with_value(*v);
        assert_eq!(f.value_as_bool(), Some(false), "expected false for {v}");
    }
}

#[test]
fn test_empty_field_value_as_bool() {
    let f = Field::new("test", FieldType::Boolean);
    assert_eq!(f.value_as_bool(), None);
}
