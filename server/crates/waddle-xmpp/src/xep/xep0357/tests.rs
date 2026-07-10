use super::*;

fn make_enable_iq(jid_attr: &str, node_attr: Option<&str>, with_form: bool) -> Iq {
    let mut enable = Element::builder("enable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid_attr);

    if let Some(node) = node_attr {
        enable = enable.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }

    let mut enable_elem = enable.build();

    if with_form {
        let form_type_value = Element::builder("value", NS_DATA_FORMS)
            .append(NS_PUBSUB_PUBLISH_OPTIONS)
            .build();
        let form_type_field = Element::builder("field", NS_DATA_FORMS)
            .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
            .append(form_type_value)
            .build();

        let secret_value = Element::builder("value", NS_DATA_FORMS)
            .append("opaque-secret")
            .build();
        let secret_field = Element::builder("field", NS_DATA_FORMS)
            .attr(minidom::rxml::xml_ncname!("var").to_owned(), "secret")
            .append(secret_value)
            .build();

        let form = Element::builder("x", NS_DATA_FORMS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
            .append(form_type_field)
            .append(secret_field)
            .build();

        enable_elem.append_child(form);
    }

    Iq::Set {
        from: Some("alice@example.com".parse().expect("valid jid")),
        to: Some("example.com".parse().expect("valid jid")),
        id: "push1".to_string(),
        payload: enable_elem,
    }
}

fn make_disable_iq(jid_attr: &str, node_attr: Option<&str>) -> Iq {
    let mut disable = Element::builder("disable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid_attr);

    if let Some(node) = node_attr {
        disable = disable.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }

    Iq::Set {
        from: Some("alice@example.com".parse().expect("valid jid")),
        to: Some("example.com".parse().expect("valid jid")),
        id: "push2".to_string(),
        payload: disable.build(),
    }
}

fn bare_jid(value: &str) -> jid::BareJid {
    value.parse().expect("valid bare jid")
}

fn submit_form_with_form_type(form_type: Option<&str>) -> Element {
    let mut form = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit");
    if let Some(form_type) = form_type {
        form = form.append(
            Element::builder("field", NS_DATA_FORMS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .append(
                    Element::builder("value", NS_DATA_FORMS)
                        .append(form_type)
                        .build(),
                )
                .build(),
        );
    }
    form.build()
}

#[test]
fn test_ns_push_constant() {
    assert_eq!(NS_PUSH, "urn:xmpp:push:0");
}

#[test]
fn test_is_push_enable() {
    let iq = make_enable_iq("push-service.example.com", Some("web-push"), true);
    assert!(is_push_enable(&iq));
    assert!(!is_push_disable(&iq));
}

#[test]
fn test_is_push_enable_false_for_get() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(!is_push_enable(&iq));
}

#[test]
fn test_is_push_enable_false_for_wrong_ns() {
    let elem = Element::builder("enable", "wrong:ns")
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(!is_push_enable(&iq));
}

#[test]
fn test_is_push_enable_false_for_result() {
    let iq = Iq::Result {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: None,
    };
    assert!(!is_push_enable(&iq));
}

#[test]
fn test_is_push_disable() {
    let iq = make_disable_iq("push-service.example.com", Some("web-push"));
    assert!(is_push_disable(&iq));
    assert!(!is_push_enable(&iq));
}

#[test]
fn test_is_push_disable_false_for_get() {
    let elem = Element::builder("disable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(!is_push_disable(&iq));
}

#[test]
fn test_is_push_disable_false_for_wrong_element() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(!is_push_disable(&iq));
}

#[test]
fn test_parse_push_enable_with_options() {
    let iq = make_enable_iq("push-service.example.com", Some("web-push"), true);
    let enable = parse_push_enable(&iq).expect("should parse");

    assert_eq!(enable.jid, bare_jid("push-service.example.com"));
    assert_eq!(enable.node.as_deref(), Some("web-push"));
    assert_eq!(enable.options.len(), 2);
    assert!(matches!(
        enable.publish_options,
        PublishOptionsParse::Valid(_)
    ));

    assert!(enable
        .options
        .iter()
        .any(|(k, v)| k == "FORM_TYPE" && v == NS_PUBSUB_PUBLISH_OPTIONS));
    assert!(enable
        .options
        .iter()
        .any(|(k, v)| k == "secret" && v == "opaque-secret"));
}

#[test]
fn test_parse_push_enable_without_options() {
    let iq = make_enable_iq("push-service.example.com", Some("web-push"), false);
    let enable = parse_push_enable(&iq).expect("should parse");

    assert_eq!(enable.jid, bare_jid("push-service.example.com"));
    assert_eq!(enable.node.as_deref(), Some("web-push"));
    assert!(enable.options.is_empty());
    assert_eq!(enable.publish_options, PublishOptionsParse::Absent);
}

#[test]
fn test_parse_push_enable_ignores_provider_attribute_options() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push-service.example.com",
        )
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "web-push")
        .attr(
            minidom::rxml::xml_ncname!("endpoint").to_owned(),
            "https://push.example.com/abc",
        )
        .attr(minidom::rxml::xml_ncname!("p256dh").to_owned(), "BASE64KEY")
        .attr(minidom::rxml::xml_ncname!("auth").to_owned(), "BASE64AUTH")
        .build();
    let iq = Iq::Set {
        from: Some("alice@example.com".parse().expect("valid jid")),
        to: Some("example.com".parse().expect("valid jid")),
        id: "push1".to_string(),
        payload: elem,
    };
    let enable = parse_push_enable(&iq).expect("should parse");

    assert_eq!(enable.jid, bare_jid("push-service.example.com"));
    assert_eq!(enable.node.as_deref(), Some("web-push"));
    assert!(enable.options.is_empty());
    assert_eq!(enable.publish_options, PublishOptionsParse::Absent);
}

#[test]
fn test_parse_push_enable_without_node() {
    let iq = make_enable_iq("push-service.example.com", None, false);
    let enable = parse_push_enable(&iq).expect("should parse");

    assert_eq!(enable.jid, bare_jid("push-service.example.com"));
    assert!(enable.node.is_none());
    assert_eq!(enable.publish_options, PublishOptionsParse::Absent);
}

#[test]
fn test_parse_push_enable_wrong_publish_options_form_type_is_invalid() {
    let mut enable_elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    enable_elem.append_child(submit_form_with_form_type(Some("jabber:x:oob")));
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: enable_elem,
    };

    let enable = parse_push_enable(&iq).expect("enable");
    assert!(enable.options.is_empty());
    assert_eq!(enable.publish_options, PublishOptionsParse::Invalid);
}

#[test]
fn test_parse_push_enable_missing_publish_options_form_type_is_invalid() {
    let mut enable_elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    enable_elem.append_child(submit_form_with_form_type(None));
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: enable_elem,
    };

    let enable = parse_push_enable(&iq).expect("enable");
    assert!(enable.options.is_empty());
    assert_eq!(enable.publish_options, PublishOptionsParse::Invalid);
}

// Greptile review: a VALID publish-options form followed by a second
// submit form (wrong FORM_TYPE or even another valid one) must not
// slip through on first-match — multiple submit forms are ambiguous
// and therefore Invalid.
#[test]
fn test_parse_push_enable_multiple_submit_forms_are_invalid() {
    let mut enable_elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    enable_elem.append_child(submit_form_with_form_type(Some(
        "http://jabber.org/protocol/pubsub#publish-options",
    )));
    enable_elem.append_child(submit_form_with_form_type(Some("jabber:x:oob")));
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: enable_elem,
    };

    let enable = parse_push_enable(&iq).expect("enable");
    assert_eq!(enable.publish_options, PublishOptionsParse::Invalid);
}

#[test]
fn test_parse_push_enable_missing_jid() {
    let elem = Element::builder("enable", NS_PUSH).build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(parse_push_enable(&iq).is_none());
}

#[test]
fn test_parse_push_enable_empty_jid() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), "")
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(parse_push_enable(&iq).is_none());
}

#[test]
fn test_parse_push_enable_invalid_jid() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), "not a jid")
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(parse_push_enable(&iq).is_none());
}

#[test]
fn test_parse_push_enable_rejects_full_jid() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com/device",
        )
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(parse_push_enable(&iq).is_none());
}

#[test]
fn test_parse_push_enable_wrong_payload_type() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(parse_push_enable(&iq).is_none());
}

#[test]
fn test_parse_push_disable() {
    let iq = make_disable_iq("push-service.example.com", Some("web-push"));
    let disable = parse_push_disable(&iq).expect("should parse");

    assert_eq!(disable.jid, bare_jid("push-service.example.com"));
    assert_eq!(disable.node.as_deref(), Some("web-push"));
}

#[test]
fn test_parse_push_disable_without_node() {
    let iq = make_disable_iq("push-service.example.com", None);
    let disable = parse_push_disable(&iq).expect("should parse");

    assert_eq!(disable.jid, bare_jid("push-service.example.com"));
    assert!(disable.node.is_none());
}

#[test]
fn test_parse_push_disable_missing_jid() {
    let elem = Element::builder("disable", NS_PUSH).build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(parse_push_disable(&iq).is_none());
}

#[test]
fn test_parse_push_disable_invalid_jid() {
    let elem = Element::builder("disable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), "not a jid")
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(parse_push_disable(&iq).is_none());
}

#[test]
fn test_parse_push_disable_rejects_full_jid() {
    let elem = Element::builder("disable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com/device",
        )
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(parse_push_disable(&iq).is_none());
}

#[test]
fn test_parse_push_disable_wrong_payload_type() {
    let elem = Element::builder("disable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: elem,
    };
    assert!(parse_push_disable(&iq).is_none());
}

#[test]
fn test_build_push_enable_result() {
    let iq = make_enable_iq("push-service.example.com", Some("web-push"), true);
    let result = build_push_enable_result(&iq);

    assert_eq!(result.id(), "push1");
    assert_eq!(result.from(), iq.to());
    assert_eq!(result.to(), iq.from());
    assert!(matches!(result, Iq::Result { payload: None, .. }));
}

#[test]
fn test_build_push_disable_result() {
    let iq = make_disable_iq("push-service.example.com", Some("web-push"));
    let result = build_push_disable_result(&iq);

    assert_eq!(result.id(), "push2");
    assert_eq!(result.from(), iq.to());
    assert_eq!(result.to(), iq.from());
    assert!(matches!(result, Iq::Result { payload: None, .. }));
}

#[test]
fn test_build_result_with_none_addresses() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test-none".to_string(),
        payload: elem,
    };
    let result = build_push_enable_result(&iq);
    assert!(result.from().is_none());
    assert!(result.to().is_none());
    assert_eq!(result.id(), "test-none");
}

#[test]
fn test_parse_data_form_with_empty_value() {
    let empty_value = Element::builder("value", NS_DATA_FORMS).build();
    let field = Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), "endpoint")
        .append(empty_value)
        .build();
    let form = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(field)
        .build();
    let mut enable_elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    enable_elem.append_child(form);

    let options = parse_data_form_options(
        enable_elem
            .get_child("x", NS_DATA_FORMS)
            .expect("data form"),
    );
    assert!(options.is_empty());
}

#[test]
fn test_parse_data_form_with_missing_var() {
    let value = Element::builder("value", NS_DATA_FORMS)
        .append("some-value")
        .build();
    let field = Element::builder("field", NS_DATA_FORMS)
        .append(value)
        .build();
    let form = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(field)
        .build();
    let mut enable_elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    enable_elem.append_child(form);

    let options = parse_data_form_options(
        enable_elem
            .get_child("x", NS_DATA_FORMS)
            .expect("data form"),
    );
    assert!(options.is_empty());
}

#[test]
fn test_non_publish_options_form_is_not_registration_publish_options() {
    let mut enable_elem = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    enable_elem.append_child(submit_form_with_form_type(Some("not-publish-options")));
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: enable_elem,
    };

    let enable = parse_push_enable(&iq).expect("enable");
    assert!(enable.options.is_empty());
    assert_eq!(enable.publish_options, PublishOptionsParse::Invalid);
}

#[test]
fn test_push_enable_struct_debug() {
    let enable = PushEnable {
        jid: bare_jid("push.example.com"),
        node: Some("web-push".to_string()),
        options: vec![("key".to_string(), "val".to_string())],
        publish_options: PublishOptionsParse::Absent,
    };
    let debug = format!("{:?}", enable);
    assert!(debug.contains("push.example.com"));
    assert!(debug.contains("web-push"));
}

#[test]
fn test_push_disable_struct_debug() {
    let disable = PushDisable {
        jid: bare_jid("push.example.com"),
        node: None,
    };
    let debug = format!("{:?}", disable);
    assert!(debug.contains("push.example.com"));
}

#[test]
fn test_push_enable_clone_eq() {
    let enable = PushEnable {
        jid: bare_jid("push.example.com"),
        node: Some("node1".to_string()),
        options: vec![("k".to_string(), "v".to_string())],
        publish_options: PublishOptionsParse::Absent,
    };
    let cloned = enable.clone();
    assert_eq!(enable, cloned);
}

#[test]
fn test_push_disable_clone_eq() {
    let disable = PushDisable {
        jid: bare_jid("push.example.com"),
        node: Some("node1".to_string()),
    };
    let cloned = disable.clone();
    assert_eq!(disable, cloned);
}
