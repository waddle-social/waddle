//! XEP-0004: Data Forms dedicated suite.
//!
//! Exercises the public `DataForm` API from outside the crate:
//! build → serialize → reparse round trips, parsing of raw
//! `jabber:x:data` wire XML, typed error variants for malformed
//! forms, and namespace exactness.

use minidom::Element;
use waddle_xmpp::xep::{
    find_data_form, is_data_form, DataForm, DataFormError, Field, FieldOption, FieldType, FormType,
    FromElement, ToElement, NS_DATA_FORMS,
};

fn reparse(elem: &Element) -> Element {
    String::from(elem)
        .parse::<Element>()
        .expect("serialized form is well-formed XML")
}

#[test]
fn xep0004_namespace_is_exact() {
    assert_eq!(NS_DATA_FORMS, "jabber:x:data");
}

#[test]
fn xep0004_full_form_survives_serialize_reparse_round_trip() {
    let form = DataForm::new(FormType::Form)
        .with_title("Bot Configuration")
        .add_instructions("Fill out this form to configure your new bot!")
        .add_field(Field::form_type("jabber:bot"))
        .add_field(
            Field::text_single("botname", "The Jabber Bot")
                .with_label("The name of your bot")
                .with_required(),
        )
        .add_field(Field::boolean("public", false))
        .add_field(
            Field::new("features", FieldType::ListMulti)
                .add_option(FieldOption::with_label("News", "news"))
                .add_option(FieldOption::new("search"))
                .add_value("news"),
        )
        .add_field(Field::new("password", FieldType::TextPrivate).with_desc("Bot password"));

    let elem = form.to_element();
    assert!(is_data_form(&elem));

    let parsed = DataForm::from_element(&reparse(&elem)).expect("round-trip parse");
    assert_eq!(parsed, form);
}

#[test]
fn xep0004_parses_wire_submit_form() {
    let xml = "<x xmlns='jabber:x:data' type='submit'>\
               <field var='FORM_TYPE' type='hidden'><value>jabber:bot</value></field>\
               <field var='botname'><value>The Jabber Bot</value></field>\
               <field var='public' type='boolean'><value>1</value></field>\
               </x>";
    let elem: Element = xml.parse().expect("valid xml");
    let form = DataForm::from_element(&elem).expect("valid submit form");

    assert_eq!(form.form_type, FormType::Submit);
    assert_eq!(form.get_form_type_value(), Some("jabber:bot"));
    assert_eq!(form.get_value("botname"), Some("The Jabber Bot"));
    assert_eq!(form.get_bool("public"), Some(true));
}

#[test]
fn xep0004_field_without_type_defaults_to_text_single() {
    // XEP-0004 §3.2: if no 'type' is specified, the default is text-single.
    let xml = "<x xmlns='jabber:x:data' type='submit'>\
               <field var='untyped'><value>v</value></field>\
               </x>";
    let elem: Element = xml.parse().expect("valid xml");
    let form = DataForm::from_element(&elem).expect("valid form");
    let field = form.field("untyped").expect("field present");
    assert_eq!(field.field_type, FieldType::TextSingle);
}

#[test]
fn xep0004_tabular_result_form_round_trips_reported_and_items() {
    let form = DataForm::new(FormType::Result)
        .add_reported(Field::new("jid", FieldType::JidSingle).with_label("JID"))
        .add_reported(Field::new("name", FieldType::TextSingle).with_label("Name"))
        .add_item(vec![
            Field::text_single("jid", "juliet@capulet.com"),
            Field::text_single("name", "Juliet"),
        ])
        .add_item(vec![
            Field::text_single("jid", "romeo@montague.net"),
            Field::text_single("name", "Romeo"),
        ]);

    let parsed = DataForm::from_element(&reparse(&form.to_element())).expect("round trip");
    assert_eq!(parsed.reported.len(), 2);
    assert_eq!(parsed.items.len(), 2);
    assert_eq!(parsed, form);
}

#[test]
fn xep0004_cancel_form_round_trips() {
    let form = DataForm::new(FormType::Cancel);
    let parsed = DataForm::from_element(&reparse(&form.to_element())).expect("round trip");
    assert_eq!(parsed.form_type, FormType::Cancel);
    assert!(parsed.fields.is_empty());
}

#[test]
fn xep0004_missing_type_attribute_is_typed_error() {
    let xml = "<x xmlns='jabber:x:data'/>";
    let elem: Element = xml.parse().expect("valid xml");
    let err = DataForm::from_element(&elem).expect_err("must reject missing type");
    assert!(matches!(err, DataFormError::MissingElement(_)));
}

#[test]
fn xep0004_invalid_form_type_is_typed_error() {
    let xml = "<x xmlns='jabber:x:data' type='bogus'/>";
    let elem: Element = xml.parse().expect("valid xml");
    let err = DataForm::from_element(&elem).expect_err("must reject bogus type");
    assert!(matches!(err, DataFormError::InvalidFormType(t) if t == "bogus"));
}

#[test]
fn xep0004_wrong_namespace_is_not_a_data_form() {
    let elem = Element::builder("x", "jabber:x:oob").build();
    assert!(!is_data_form(&elem));
    let err = DataForm::from_element(&elem).expect_err("wrong namespace");
    assert!(matches!(err, DataFormError::NotADataForm));
}

#[test]
fn xep0004_find_data_form_locates_nested_form() {
    let form_elem = DataForm::new(FormType::Result)
        .add_field(Field::form_type("urn:example:info"))
        .to_element();
    let parent = Element::builder("query", "http://jabber.org/protocol/disco#info")
        .append(form_elem)
        .build();

    let found = find_data_form(&parent)
        .expect("form child present")
        .expect("form parses");
    assert_eq!(found.get_form_type_value(), Some("urn:example:info"));

    let empty = Element::builder("query", "http://jabber.org/protocol/disco#info").build();
    assert!(find_data_form(&empty).is_none());
}

#[test]
fn xep0004_boolean_wire_values_accept_one_and_true() {
    for (wire, expected) in [("1", true), ("true", true), ("0", false), ("false", false)] {
        let xml = format!(
            "<x xmlns='jabber:x:data' type='submit'>\
             <field var='flag' type='boolean'><value>{wire}</value></field>\
             </x>"
        );
        let elem: Element = xml.parse().expect("valid xml");
        let form = DataForm::from_element(&elem).expect("valid form");
        assert_eq!(form.get_bool("flag"), Some(expected), "wire value {wire}");
    }
}

#[test]
fn xep0004_multi_value_field_preserves_order_and_count() {
    let form = DataForm::new(FormType::Submit).add_field(
        Field::new("features", FieldType::TextMulti)
            .add_value("first")
            .add_value("second")
            .add_value("third"),
    );

    let parsed = DataForm::from_element(&reparse(&form.to_element())).expect("round trip");
    assert_eq!(
        parsed.get_values("features").expect("values"),
        &[
            "first".to_string(),
            "second".to_string(),
            "third".to_string()
        ]
    );
    assert!(FieldType::TextMulti.is_multi());
    assert!(!FieldType::TextSingle.is_multi());
}

#[test]
fn xep0004_required_and_desc_survive_round_trip() {
    let form = DataForm::new(FormType::Form).add_field(
        Field::new("email", FieldType::TextSingle)
            .with_desc("Your email address")
            .with_required(),
    );

    let parsed = DataForm::from_element(&reparse(&form.to_element())).expect("round trip");
    let field = parsed.field("email").expect("field");
    assert!(field.required);
    assert_eq!(field.desc.as_deref(), Some("Your email address"));
}

#[test]
fn xep0004_fixed_field_needs_no_var() {
    let form = DataForm::new(FormType::Form).add_field(Field::fixed("Section header"));
    let parsed = DataForm::from_element(&reparse(&form.to_element())).expect("round trip");
    assert_eq!(parsed.fields.len(), 1);
    assert_eq!(parsed.fields[0].field_type, FieldType::Fixed);
    assert!(parsed.fields[0].var.is_none());
    assert_eq!(parsed.fields[0].value(), Some("Section header"));
}
