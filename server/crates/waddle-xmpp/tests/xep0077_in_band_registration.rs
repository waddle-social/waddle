//! XEP-0077: In-Band Registration dedicated conformance suite.

use minidom::Element;
use waddle_xmpp::xep::xep0077::{
    build_registration_error, build_registration_feature, build_registration_fields_response,
    build_registration_success, RegistrationError, NS_REGISTER, NS_REGISTER_FEATURE,
};
use xmpp_parsers::{
    iq::Iq,
    stanza_error::{DefinedCondition, ErrorType},
};

fn serialize_element(element: &Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize element");
    String::from_utf8(buf).expect("minidom serializes UTF-8")
}

fn serialize_iq(iq: Iq) -> String {
    serialize_element(&iq.into())
}

fn error_child(iq: &Element) -> &Element {
    iq.get_child("error", "jabber:client").expect("error child")
}

fn submitted_registration_query() -> Element {
    Element::builder("query", NS_REGISTER)
        .append(
            Element::builder("username", NS_REGISTER)
                .append("bill")
                .build(),
        )
        .append(
            Element::builder("password", NS_REGISTER)
                .append("m1cro$oft")
                .build(),
        )
        .append(
            Element::builder("email", NS_REGISTER)
                .append("billg@bigcompany.com")
                .build(),
        )
        .build()
}

fn parse_iq(xml: &str) -> Iq {
    Iq::try_from(xml.parse::<Element>().expect("serialized iq is xml"))
        .expect("serialized iq parses")
}

#[test]
fn xep0077_fields_result_uses_register_query_children() {
    let iq = build_registration_fields_response("reg'&<1", Some("Use <name> & \"password\""), true);

    let xml = serialize_iq(iq);
    assert!(xml.contains("&amp;&lt;1"));
    assert!(xml.contains("Use &lt;name&gt; &amp; \"password\""));

    let Iq::Result {
        id,
        payload: Some(query),
        ..
    } = parse_iq(&xml)
    else {
        panic!("registration fields response is an IQ result with query payload");
    };

    assert_eq!(id, "reg'&<1");
    assert_eq!(query.name(), "query");
    assert_eq!(query.ns(), NS_REGISTER);
    assert_eq!(
        query
            .get_child("instructions", NS_REGISTER)
            .expect("instructions child")
            .text(),
        "Use <name> & \"password\""
    );
    assert_eq!(
        query
            .children()
            .map(|child| (child.name(), child.ns().to_string(), child.text()))
            .collect::<Vec<_>>(),
        vec![
            (
                "instructions",
                NS_REGISTER.to_string(),
                "Use <name> & \"password\"".to_string()
            ),
            ("username", NS_REGISTER.to_string(), String::new()),
            ("password", NS_REGISTER.to_string(), String::new()),
            ("email", NS_REGISTER.to_string(), String::new()),
        ]
    );
}

#[test]
fn xep0077_fields_result_omits_optional_children_when_absent() {
    let Iq::Result {
        payload: Some(query),
        ..
    } = build_registration_fields_response("reg1", None, false)
    else {
        panic!("registration fields response is an IQ result with query payload");
    };

    assert!(query.get_child("instructions", NS_REGISTER).is_none());
    assert!(query.get_child("email", NS_REGISTER).is_none());
    assert!(query.get_child("username", NS_REGISTER).is_some());
    assert!(query.get_child("password", NS_REGISTER).is_some());
}

#[test]
fn xep0077_success_result_has_no_payload() {
    let xml = serialize_iq(build_registration_success("reg2"));

    let Iq::Result { id, payload, .. } = parse_iq(&xml) else {
        panic!("registration success is an IQ result");
    };

    assert_eq!(id, "reg2");
    assert!(payload.is_none());
}

#[test]
fn xep0077_error_maps_conflict_to_cancel_conflict_with_register_query() {
    let submitted_query = submitted_registration_query();
    let element =
        build_registration_error("reg3", Some(&submitted_query), &RegistrationError::Conflict)
            .to_element();
    assert_eq!(error_child(&element).attr("code"), Some("409"));
    let xml = serialize_element(&element);

    let Iq::Error {
        id,
        payload: Some(query),
        error,
        ..
    } = parse_iq(&xml)
    else {
        panic!("registration failure is an IQ error with original query payload");
    };

    assert_eq!(id, "reg3");
    assert_eq!(query.name(), "query");
    assert_eq!(query.ns(), NS_REGISTER);
    assert_eq!(
        query
            .get_child("username", NS_REGISTER)
            .expect("username")
            .text(),
        "bill"
    );
    assert_eq!(
        query
            .get_child("password", NS_REGISTER)
            .expect("password")
            .text(),
        "m1cro$oft"
    );
    assert_eq!(
        query.get_child("email", NS_REGISTER).expect("email").text(),
        "billg@bigcompany.com"
    );
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::Conflict);
    assert!(error.texts.is_empty());
}

#[test]
fn xep0077_error_text_is_serialized_by_the_xml_writer() {
    let message = "Username <taken> & \"invalid\"";
    let element = build_registration_error(
        "reg5",
        None,
        &RegistrationError::NotAcceptable(message.to_string()),
    )
    .to_element();
    assert_eq!(error_child(&element).attr("code"), Some("406"));
    let xml = serialize_element(&element);

    assert!(xml.contains("Username &lt;taken&gt; &amp; \"invalid\""));

    let Iq::Error { error, .. } = parse_iq(&xml) else {
        panic!("registration failure is an IQ error");
    };

    assert_eq!(error.type_, ErrorType::Modify);
    assert_eq!(error.defined_condition, DefinedCondition::NotAcceptable);
    assert_eq!(
        error.texts.values().next().map(String::as_str),
        Some(message)
    );
}

#[test]
fn xep0077_error_variants_use_rfc6120_conditions() {
    let cases = [
        (
            RegistrationError::NotAllowed,
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "503",
        ),
        (
            RegistrationError::BadRequest("bad".to_string()),
            ErrorType::Modify,
            DefinedCondition::BadRequest,
            "400",
        ),
        (
            RegistrationError::InternalError("oops".to_string()),
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "500",
        ),
    ];

    for (registration_error, error_type, condition, legacy_code) in cases {
        let element = build_registration_error("reg", None, &registration_error).to_element();
        assert_eq!(error_child(&element).attr("code"), Some(legacy_code));

        let Iq::Error { error, .. } = parse_iq(&serialize_element(&element)) else {
            panic!("registration failure is an IQ error");
        };

        assert_eq!(error.type_, error_type);
        assert_eq!(error.defined_condition, condition);
    }
}

#[test]
fn xep0077_stream_feature_uses_feature_namespace() {
    let feature = build_registration_feature();
    let xml = serialize_element(&feature);
    let reparsed = xml.parse::<Element>().expect("stream feature is xml");

    assert_eq!(reparsed.name(), "register");
    assert_eq!(reparsed.ns(), NS_REGISTER_FEATURE);
    assert_ne!(reparsed.ns(), NS_REGISTER);
    assert_eq!(reparsed.children().count(), 0);
    assert_eq!(reparsed.text(), "");
}
