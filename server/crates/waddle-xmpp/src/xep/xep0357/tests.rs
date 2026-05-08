use super::*;

fn make_enable_iq(jid_attr: &str, node_attr: Option<&str>, with_form: bool) -> Iq {
    let mut enable = Element::builder("enable", NS_PUSH).attr("jid", jid_attr);

    if let Some(node) = node_attr {
        enable = enable.attr("node", node);
    }

    let mut enable_elem = enable.build();

    if with_form {
        let endpoint_value = Element::builder("value", NS_DATA_FORMS)
            .append("https://push.example.com/abc")
            .build();
        let endpoint_field = Element::builder("field", NS_DATA_FORMS)
            .attr("var", "endpoint")
            .append(endpoint_value)
            .build();

        let p256dh_value = Element::builder("value", NS_DATA_FORMS)
            .append("BASE64KEY")
            .build();
        let p256dh_field = Element::builder("field", NS_DATA_FORMS)
            .attr("var", "p256dh")
            .append(p256dh_value)
            .build();

        let auth_value = Element::builder("value", NS_DATA_FORMS)
            .append("BASE64AUTH")
            .build();
        let auth_field = Element::builder("field", NS_DATA_FORMS)
            .attr("var", "auth")
            .append(auth_value)
            .build();

        let form = Element::builder("x", NS_DATA_FORMS)
            .attr("type", "submit")
            .append(endpoint_field)
            .append(p256dh_field)
            .append(auth_field)
            .build();

        enable_elem.append_child(form);
    }

    Iq {
        from: Some("alice@example.com".parse().expect("valid jid")),
        to: Some("example.com".parse().expect("valid jid")),
        id: "push1".to_string(),
        payload: IqType::Set(enable_elem),
    }
}

fn make_disable_iq(jid_attr: &str, node_attr: Option<&str>) -> Iq {
    let mut disable = Element::builder("disable", NS_PUSH).attr("jid", jid_attr);

    if let Some(node) = node_attr {
        disable = disable.attr("node", node);
    }

    Iq {
        from: Some("alice@example.com".parse().expect("valid jid")),
        to: Some("example.com".parse().expect("valid jid")),
        id: "push2".to_string(),
        payload: IqType::Set(disable.build()),
    }
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
        .attr("jid", "push.example.com")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: IqType::Get(elem),
    };
    assert!(!is_push_enable(&iq));
}

#[test]
fn test_is_push_enable_false_for_wrong_ns() {
    let elem = Element::builder("enable", "wrong:ns")
        .attr("jid", "push.example.com")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: IqType::Set(elem),
    };
    assert!(!is_push_enable(&iq));
}

#[test]
fn test_is_push_enable_false_for_result() {
    let iq = Iq {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: IqType::Result(None),
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
        .attr("jid", "push.example.com")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: IqType::Get(elem),
    };
    assert!(!is_push_disable(&iq));
}

#[test]
fn test_is_push_disable_false_for_wrong_element() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr("jid", "push.example.com")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: IqType::Set(elem),
    };
    assert!(!is_push_disable(&iq));
}

#[test]
fn test_parse_push_enable_with_options() {
    let iq = make_enable_iq("push-service.example.com", Some("web-push"), true);
    let enable = parse_push_enable(&iq).expect("should parse");

    assert_eq!(enable.jid, "push-service.example.com");
    assert_eq!(enable.node.as_deref(), Some("web-push"));
    assert_eq!(enable.options.len(), 3);

    assert!(enable
        .options
        .iter()
        .any(|(k, v)| k == "endpoint" && v == "https://push.example.com/abc"));
    assert!(enable
        .options
        .iter()
        .any(|(k, v)| k == "p256dh" && v == "BASE64KEY"));
    assert!(enable
        .options
        .iter()
        .any(|(k, v)| k == "auth" && v == "BASE64AUTH"));
}

#[test]
fn test_parse_push_enable_without_options() {
    let iq = make_enable_iq("push-service.example.com", Some("web-push"), false);
    let enable = parse_push_enable(&iq).expect("should parse");

    assert_eq!(enable.jid, "push-service.example.com");
    assert_eq!(enable.node.as_deref(), Some("web-push"));
    assert!(enable.options.is_empty());
}

#[test]
fn test_parse_push_enable_with_attribute_options() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr("jid", "push-service.example.com")
        .attr("node", "web-push")
        .attr("endpoint", "https://push.example.com/abc")
        .attr("p256dh", "BASE64KEY")
        .attr("auth", "BASE64AUTH")
        .build();
    let iq = Iq {
        from: Some("alice@example.com".parse().expect("valid jid")),
        to: Some("example.com".parse().expect("valid jid")),
        id: "push1".to_string(),
        payload: IqType::Set(elem),
    };
    let enable = parse_push_enable(&iq).expect("should parse");

    assert_eq!(enable.jid, "push-service.example.com");
    assert_eq!(enable.node.as_deref(), Some("web-push"));
    assert_eq!(enable.options.len(), 3);
    assert!(enable
        .options
        .iter()
        .any(|(k, v)| k == "endpoint" && v == "https://push.example.com/abc"));
    assert!(enable
        .options
        .iter()
        .any(|(k, v)| k == "p256dh" && v == "BASE64KEY"));
    assert!(enable
        .options
        .iter()
        .any(|(k, v)| k == "auth" && v == "BASE64AUTH"));
}

#[test]
fn test_parse_push_enable_without_node() {
    let iq = make_enable_iq("push-service.example.com", None, false);
    let enable = parse_push_enable(&iq).expect("should parse");

    assert_eq!(enable.jid, "push-service.example.com");
    assert!(enable.node.is_none());
}

#[test]
fn test_parse_push_enable_missing_jid() {
    let elem = Element::builder("enable", NS_PUSH).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: IqType::Set(elem),
    };
    assert!(parse_push_enable(&iq).is_none());
}

#[test]
fn test_parse_push_enable_empty_jid() {
    let elem = Element::builder("enable", NS_PUSH).attr("jid", "").build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: IqType::Set(elem),
    };
    assert!(parse_push_enable(&iq).is_none());
}

#[test]
fn test_parse_push_enable_wrong_payload_type() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr("jid", "push.example.com")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: IqType::Get(elem),
    };
    assert!(parse_push_enable(&iq).is_none());
}

#[test]
fn test_parse_push_disable() {
    let iq = make_disable_iq("push-service.example.com", Some("web-push"));
    let disable = parse_push_disable(&iq).expect("should parse");

    assert_eq!(disable.jid, "push-service.example.com");
    assert_eq!(disable.node.as_deref(), Some("web-push"));
}

#[test]
fn test_parse_push_disable_without_node() {
    let iq = make_disable_iq("push-service.example.com", None);
    let disable = parse_push_disable(&iq).expect("should parse");

    assert_eq!(disable.jid, "push-service.example.com");
    assert!(disable.node.is_none());
}

#[test]
fn test_parse_push_disable_missing_jid() {
    let elem = Element::builder("disable", NS_PUSH).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: IqType::Set(elem),
    };
    assert!(parse_push_disable(&iq).is_none());
}

#[test]
fn test_parse_push_disable_wrong_payload_type() {
    let elem = Element::builder("disable", NS_PUSH)
        .attr("jid", "push.example.com")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test".to_string(),
        payload: IqType::Get(elem),
    };
    assert!(parse_push_disable(&iq).is_none());
}

#[test]
fn test_build_push_enable_result() {
    let iq = make_enable_iq("push-service.example.com", Some("web-push"), true);
    let result = build_push_enable_result(&iq);

    assert_eq!(result.id, "push1");
    assert_eq!(result.from, iq.to);
    assert_eq!(result.to, iq.from);
    assert!(matches!(result.payload, IqType::Result(None)));
}

#[test]
fn test_build_push_disable_result() {
    let iq = make_disable_iq("push-service.example.com", Some("web-push"));
    let result = build_push_disable_result(&iq);

    assert_eq!(result.id, "push2");
    assert_eq!(result.from, iq.to);
    assert_eq!(result.to, iq.from);
    assert!(matches!(result.payload, IqType::Result(None)));
}

#[test]
fn test_build_result_with_none_addresses() {
    let elem = Element::builder("enable", NS_PUSH)
        .attr("jid", "push.example.com")
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test-none".to_string(),
        payload: IqType::Set(elem),
    };
    let result = build_push_enable_result(&iq);
    assert!(result.from.is_none());
    assert!(result.to.is_none());
    assert_eq!(result.id, "test-none");
}

#[test]
fn test_parse_data_form_with_empty_value() {
    let empty_value = Element::builder("value", NS_DATA_FORMS).build();
    let field = Element::builder("field", NS_DATA_FORMS)
        .attr("var", "endpoint")
        .append(empty_value)
        .build();
    let form = Element::builder("x", NS_DATA_FORMS)
        .attr("type", "submit")
        .append(field)
        .build();
    let mut enable_elem = Element::builder("enable", NS_PUSH)
        .attr("jid", "push.example.com")
        .build();
    enable_elem.append_child(form);

    let options = parse_data_form_options(&enable_elem);
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
        .attr("type", "submit")
        .append(field)
        .build();
    let mut enable_elem = Element::builder("enable", NS_PUSH)
        .attr("jid", "push.example.com")
        .build();
    enable_elem.append_child(form);

    let options = parse_data_form_options(&enable_elem);
    assert!(options.is_empty());
}

#[test]
fn test_push_enable_struct_debug() {
    let enable = PushEnable {
        jid: "push.example.com".to_string(),
        node: Some("web-push".to_string()),
        options: vec![("key".to_string(), "val".to_string())],
    };
    let debug = format!("{:?}", enable);
    assert!(debug.contains("push.example.com"));
    assert!(debug.contains("web-push"));
}

#[test]
fn test_push_disable_struct_debug() {
    let disable = PushDisable {
        jid: "push.example.com".to_string(),
        node: None,
    };
    let debug = format!("{:?}", disable);
    assert!(debug.contains("push.example.com"));
}

#[test]
fn test_push_enable_clone_eq() {
    let enable = PushEnable {
        jid: "push.example.com".to_string(),
        node: Some("node1".to_string()),
        options: vec![("k".to_string(), "v".to_string())],
    };
    let cloned = enable.clone();
    assert_eq!(enable, cloned);
}

#[test]
fn test_push_disable_clone_eq() {
    let disable = PushDisable {
        jid: "push.example.com".to_string(),
        node: Some("node1".to_string()),
    };
    let cloned = disable.clone();
    assert_eq!(disable, cloned);
}
