//! XEP-0049: Private XML Storage — dedicated conformance suite.
//!
//! Pins the typed arbitrary-XML boundary for `jabber:iq:private`:
//! GET identifies one requested child namespace, SET carries typed XML
//! payload elements, and storage serialization is parsed back into
//! `minidom::Element` before result construction.

use minidom::{rxml::xml_ncname, Element};
use waddle_xmpp::xep::xep0049::{
    build_private_storage_result, parse_private_storage_get, parse_private_storage_set,
    parse_stored_private_storage_value, PrivateStorageError, PrivateStorageKey,
    PrivateStorageValue, NS_PRIVATE,
};
use xmpp_parsers::iq::Iq;

const NS_PREFS: &str = "urn:waddle:test:prefs";

fn query(children: Vec<Element>) -> Element {
    let mut builder = Element::builder("query", NS_PRIVATE);
    for child in children {
        builder = builder.append(child);
    }
    builder.build()
}

fn get_iq(payload: Element) -> Iq {
    Iq::Get {
        from: None,
        to: None,
        id: "get-1".into(),
        payload,
    }
}

fn set_iq(payload: Element) -> Iq {
    Iq::Set {
        from: None,
        to: None,
        id: "set-1".into(),
        payload,
    }
}

fn prefs_key() -> PrivateStorageKey {
    PrivateStorageKey {
        element_name: "prefs".into(),
        namespace: NS_PREFS.into(),
    }
}

#[test]
fn xep0049_get_parses_single_child_key() {
    let child = Element::builder("prefs", NS_PREFS).build();
    let key = parse_private_storage_get(&get_iq(query(vec![child]))).expect("valid get");

    assert_eq!(key.element_name, "prefs");
    assert_eq!(key.namespace, NS_PREFS);
}

#[test]
fn xep0049_get_rejects_missing_child() {
    let error = parse_private_storage_get(&get_iq(query(vec![]))).expect_err("missing child");

    assert_eq!(error, PrivateStorageError::MissingPayload);
}

#[test]
fn xep0049_get_rejects_duplicate_children() {
    let first = Element::builder("prefs", NS_PREFS).build();
    let second = Element::builder("other", NS_PREFS).build();
    let error =
        parse_private_storage_get(&get_iq(query(vec![first, second]))).expect_err("duplicates");

    assert_eq!(error, PrivateStorageError::MultiplePayloads);
}

#[test]
fn xep0049_get_rejects_child_without_namespace() {
    let child = Element::builder("prefs", "").build();
    let error = parse_private_storage_get(&get_iq(query(vec![child]))).expect_err("missing ns");

    assert_eq!(error, PrivateStorageError::MissingNamespace);
}

#[test]
fn xep0049_set_preserves_nested_arbitrary_xml_as_typed_payload() {
    let theme = Element::builder("theme", NS_PREFS).append("dark").build();
    let nested = Element::builder("settings", NS_PREFS)
        .append(Element::builder("compact", NS_PREFS).append("true").build())
        .build();
    let prefs = Element::builder("prefs", NS_PREFS)
        .append(theme)
        .append(nested)
        .build();

    let value = parse_private_storage_set(&set_iq(query(vec![prefs]))).expect("valid set");

    assert_eq!(value.key, prefs_key());
    assert_eq!(value.elements.len(), 1);
    let children: Vec<&Element> = value.elements[0].children().collect();
    assert_eq!(children[0].name(), "theme");
    assert_eq!(children[0].text(), "dark");
    assert_eq!(children[1].name(), "settings");
}

#[test]
fn xep0049_set_accepts_multiple_children_in_one_namespace() {
    let first = Element::builder("prefs", NS_PREFS).build();
    let second = Element::builder("profile", NS_PREFS).build();

    let value = parse_private_storage_set(&set_iq(query(vec![first, second]))).expect("valid set");

    assert_eq!(value.key.namespace, NS_PREFS);
    assert_eq!(
        value.elements.iter().map(Element::name).collect::<Vec<_>>(),
        vec!["prefs", "profile"]
    );
}

#[test]
fn xep0049_set_rejects_multiple_child_namespaces() {
    let first = Element::builder("prefs", NS_PREFS).build();
    let second = Element::builder("private", "urn:waddle:test:other").build();

    let error =
        parse_private_storage_set(&set_iq(query(vec![first, second]))).expect_err("mixed ns");

    assert_eq!(error, PrivateStorageError::MultipleNamespaces);
}

#[test]
fn xep0049_set_rejects_child_without_namespace() {
    let child = Element::builder("prefs", "").build();
    let error = parse_private_storage_set(&set_iq(query(vec![child]))).expect_err("missing ns");

    assert_eq!(error, PrivateStorageError::MissingNamespace);
}

#[test]
fn xep0049_value_constructor_rejects_element_without_namespace() {
    let child = Element::builder("prefs", "").build();
    let error = PrivateStorageValue::from_element(child).expect_err("missing namespace");

    assert_eq!(error, PrivateStorageError::MissingNamespace);
}

#[test]
fn xep0049_stored_xml_must_parse_to_typed_value() {
    let error = parse_stored_private_storage_value("<prefs xmlns='urn:waddle:test:prefs'>")
        .expect_err("malformed stored XML");

    assert_eq!(error, PrivateStorageError::MalformedStoredXml);
}

#[test]
fn xep0049_stored_xml_without_namespace_is_rejected() {
    let error = parse_stored_private_storage_value("<prefs/>").expect_err("missing namespace");

    assert!(matches!(
        error,
        PrivateStorageError::MalformedStoredXml | PrivateStorageError::MissingNamespace
    ));
}

#[test]
fn xep0049_absent_value_returns_empty_requested_element() {
    let iq = get_iq(query(vec![Element::builder("prefs", NS_PREFS).build()]));
    let result = build_private_storage_result(&iq, None, &prefs_key());

    let Iq::Result {
        payload: Some(payload),
        ..
    } = result
    else {
        panic!("expected result payload");
    };

    assert_eq!(payload.name(), "query");
    assert_eq!(payload.ns(), NS_PRIVATE);
    let children: Vec<&Element> = payload.children().collect();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].name(), "prefs");
    assert_eq!(children[0].ns(), NS_PREFS);
    assert_eq!(children[0].children().count(), 0);
}

#[test]
fn xep0049_result_preserves_stored_typed_payloads() {
    let first = Element::builder("prefs", NS_PREFS)
        .append(Element::builder("theme", NS_PREFS).append("dark").build())
        .build();
    let second = Element::builder("profile", NS_PREFS)
        .append(
            Element::builder("display", NS_PREFS)
                .append("Waddle")
                .build(),
        )
        .build();
    let value = PrivateStorageValue::from_elements(vec![first, second]).expect("same ns");
    let iq = get_iq(query(vec![Element::builder("prefs", NS_PREFS).build()]));

    let result = build_private_storage_result(&iq, Some(&value), &prefs_key());

    let Iq::Result {
        payload: Some(payload),
        ..
    } = result
    else {
        panic!("expected result payload");
    };
    let children: Vec<&Element> = payload.children().collect();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].name(), "prefs");
    assert_eq!(children[1].name(), "profile");
}

#[test]
fn xep0049_storage_serializer_uses_xml_escaping() {
    let note = Element::builder("note", NS_PREFS)
        .attr(xml_ncname!("label").to_owned(), "5 > 3 & \"quoted\"")
        .append("Tom & <Jerry>")
        .build();
    let value = PrivateStorageValue::from_element(note).expect("namespaced element");

    let stored = value.to_xml_string();
    assert!(!stored.contains("Tom & <Jerry>"));

    let parsed = parse_stored_private_storage_value(&stored).expect("serialized XML parses");
    assert_eq!(parsed.elements[0].attr("label"), Some("5 > 3 & \"quoted\""));
    assert_eq!(parsed.elements[0].text(), "Tom & <Jerry>");
}
