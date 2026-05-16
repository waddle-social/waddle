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
fn test_server_features_include_core_features() {
    let features = server_features();
    assert!(features.contains(&Feature::disco_info()));
    assert!(features.contains(&Feature::carbons()));
    assert!(features.contains(&Feature::receipts()));
    assert!(!features.contains(&Feature::mam()));
    assert!(!features.contains(&Feature::mam_extended()));
    assert!(!features.contains(&Feature::fulltext_mam()));
    assert!(!features.contains(&Feature::pep()));
    assert!(!features.contains(&Feature::avatar_metadata_notify()));
    assert!(!features.contains(&Feature::vcard4_notify()));
}

#[test]
fn test_vcard4_notify_feature_var_pins_xep0292_namespace() {
    // Pin the `+notify` filter contract so a future rename trips a test
    // instead of silently breaking XEP-0292 / XEP-0163 §3 fan-out. The
    // var must equal the XEP-0292 PEP node `urn:xmpp:vcard4` plus the
    // standard `+notify` suffix; that is the string the runtime
    // fan-out filter constructs per XEP-0163 §3.
    assert_eq!(Feature::vcard4_notify().0, "urn:xmpp:vcard4+notify");
    assert_ne!(Feature::vcard4_notify(), Feature::avatar_metadata_notify());
}

#[test]
fn test_server_features_exclude_unimplemented_advertisements() {
    let features = server_features();
    assert!(!features.contains(&Feature::socks5_bytestreams()));
    assert!(!features.contains(&Feature::server_info()));
    assert!(!features.contains(&Feature::csi()));
    assert!(!features.contains(&Feature::new("urn:xmpp:pep-vcard-conversion:0")));
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
    // Feed + stories live on the community service, not spaces — the
    // spaces service MUST NOT advertise these or clients enumerate
    // them as space members.
    assert!(!features.contains(&Feature::social_feed()));
    assert!(!features.contains(&Feature::stories()));
}

#[test]
fn test_community_service_features_advertise_feed_and_stories() {
    let features = community_service_features();
    assert!(features.contains(&Feature::social_feed()));
    assert!(features.contains(&Feature::stories()));
    assert!(features.contains(&Feature::pubsub()));
    assert!(features.contains(&Feature::disco_info()));
}

#[test]
fn test_push_service_features_match_xep0357_pubsub_boundary() {
    let identity = Identity::pubsub_push(Some("Push Service"));
    assert_eq!(identity.category, "pubsub");
    assert_eq!(identity.type_, "push");

    let features = push_service_features();
    assert!(features.contains(&Feature::disco_info()));
    assert!(features.contains(&Feature::disco_items()));
    assert!(features.contains(&Feature::pubsub()));
    assert!(features.contains(&Feature::pubsub_publish()));
    assert!(features.contains(&Feature::pubsub_access_whitelist()));
    assert!(features.contains(&Feature::pubsub_publish_only_affiliation()));
    assert!(features.contains(&Feature::push()));
    assert!(!features.contains(&Feature::pubsub_auto_create()));
    assert!(!features.contains(&Feature::pubsub_subscribe()));
}
