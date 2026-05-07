use super::*;

#[test]
fn test_is_disco_info_query() {
    let query_elem = Element::builder("query", DISCO_INFO_NS).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "test-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(query_elem),
    };

    assert!(is_disco_info_query(&iq));
}

#[test]
fn test_parse_disco_info_query() {
    let query_elem = Element::builder("query", DISCO_INFO_NS)
        .attr("node", "caps#hash")
        .build();
    let iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "test-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(query_elem),
    };

    let query = parse_disco_info_query(&iq).unwrap();
    assert_eq!(query.target.as_deref(), Some("example.com"));
    assert_eq!(query.node.as_deref(), Some("caps#hash"));
}

#[test]
fn test_build_disco_info_response() {
    let query_elem = Element::builder("query", DISCO_INFO_NS).build();
    let iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "disco-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(query_elem),
    };

    let response = build_disco_info_response(
        &iq,
        &[Identity::server(Some("Waddle")).with_lang(Some("en"))],
        &[Feature::disco_info(), Feature::disco_items()],
        None,
    );

    assert_eq!(response.id, "disco-1");

    let xmpp_parsers::iq::IqType::Result(Some(query)) = response.payload else {
        panic!("expected disco#info result payload");
    };
    let identity = query
        .children()
        .find(|child| child.name() == "identity")
        .expect("identity should be present");
    assert_eq!(identity.attr("xml:lang"), Some("en"));
}

#[test]
fn test_build_server_info_abuse_form() {
    let form = build_server_info_abuse_form("example.com");
    assert_eq!(form.name(), "x");
    assert_eq!(form.ns(), DATA_FORMS_NS);
    assert_eq!(form.attr("type"), Some("result"));
    let form_type = form
        .children()
        .find(|child| child.attr("var") == Some("FORM_TYPE"))
        .and_then(|child| child.get_child("value", DATA_FORMS_NS))
        .expect("FORM_TYPE field should be present");
    assert_eq!(form_type.text(), SERVER_INFO_FORM_TYPE);

    let abuse_addresses = form
        .children()
        .find(|child| child.attr("var") == Some("abuse-addresses"))
        .and_then(|child| child.get_child("value", DATA_FORMS_NS))
        .expect("abuse-addresses field should be present");
    assert_eq!(abuse_addresses.text(), "mailto:abuse@example.com");
}

#[test]
fn test_server_features_include_core_features() {
    let features = server_features();
    assert!(features.contains(&Feature::disco_info()));
    assert!(features.contains(&Feature::carbons()));
    assert!(features.contains(&Feature::receipts()));
    assert!(features.contains(&Feature::mam_extended()));
    assert!(!features.contains(&Feature::fulltext_mam()));
    assert!(features.contains(&Feature::server_info()));
}

#[test]
fn test_muc_room_features_forum_room() {
    let features = muc_room_features(true, true, false, true);
    assert!(!features.contains(&Feature::new("urn:xmpp:forums:0")));
    assert!(features.contains(&Feature::muc_persistent()));
    assert!(features.contains(&Feature::muc_membersonly()));
    assert!(features.contains(&Feature::muc_unmoderated()));
    assert!(features.contains(&Feature::muc_self_ping_optimization()));
    assert!(features.contains(&Feature::mam_extended()));
    assert!(features.contains(&Feature::fulltext_mam()));
    assert!(features.contains(&Feature::chat_states()));
    assert_eq!(Feature::chat_states().0, NS_CHATSTATES);
}

#[test]
fn test_muc_service_features_exclude_xep_0410_feature() {
    let features = muc_service_features();
    assert!(!features.contains(&Feature::muc_self_ping_optimization()));
    assert_eq!(
        Feature::muc_self_ping_optimization().0,
        NS_MUC_SELF_PING_OPTIMIZATION
    );
}

#[test]
fn test_spaces_service_features_include_only_supported_pubsub_features() {
    let features = spaces_service_features();
    for feature in [
        "http://jabber.org/protocol/pubsub#subscribe",
        "http://jabber.org/protocol/pubsub#create-nodes",
        "http://jabber.org/protocol/pubsub#config-node",
        "http://jabber.org/protocol/pubsub#meta-data",
        "http://jabber.org/protocol/pubsub#delete-nodes",
        "http://jabber.org/protocol/pubsub#delete-items",
        "http://jabber.org/protocol/pubsub#retract-items",
        "http://jabber.org/protocol/pubsub#multi-items",
        "http://jabber.org/protocol/pubsub#item-ids",
        "http://jabber.org/protocol/pubsub#retrieve-items",
    ] {
        assert!(
            features.contains(&Feature::new(feature)),
            "spaces service missing {feature}"
        );
    }
    assert!(!features.contains(&Feature::spaces()));
    assert!(!features.contains(&Feature::new(
        "http://jabber.org/protocol/pubsub#manage-subscriptions"
    )));
}
