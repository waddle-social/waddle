use super::*;

#[test]
fn test_roster_item_new() {
    let jid: BareJid = "contact@example.com".parse().unwrap();
    let item = RosterItem::new(jid.clone());

    assert_eq!(item.jid, jid);
    assert_eq!(item.name, None);
    assert_eq!(item.subscription, Subscription::None);
    assert_eq!(item.ask, None);
    assert!(item.groups.is_empty());
}

#[test]
fn test_roster_item_with_name() {
    let jid: BareJid = "contact@example.com".parse().unwrap();
    let item = RosterItem::with_name(jid.clone(), "My Contact");

    assert_eq!(item.jid, jid);
    assert_eq!(item.name, Some("My Contact".to_string()));
}

#[test]
fn test_roster_item_builder() {
    let jid: BareJid = "contact@example.com".parse().unwrap();
    let item = RosterItem::new(jid.clone())
        .set_subscription(Subscription::Both)
        .set_ask(Some(AskType::Subscribe))
        .add_group("Friends")
        .add_group("Work");

    assert_eq!(item.subscription, Subscription::Both);
    assert_eq!(item.ask, Some(AskType::Subscribe));
    assert_eq!(item.groups, vec!["Friends", "Work"]);
}

#[test]
fn test_subscription_from_str() {
    assert_eq!("none".parse::<Subscription>().unwrap(), Subscription::None);
    assert_eq!("to".parse::<Subscription>().unwrap(), Subscription::To);
    assert_eq!("from".parse::<Subscription>().unwrap(), Subscription::From);
    assert_eq!("both".parse::<Subscription>().unwrap(), Subscription::Both);
    assert_eq!(
        "remove".parse::<Subscription>().unwrap(),
        Subscription::Remove
    );
    assert!("invalid".parse::<Subscription>().is_err());
}

#[test]
fn test_subscription_as_str() {
    assert_eq!(Subscription::None.as_str(), "none");
    assert_eq!(Subscription::To.as_str(), "to");
    assert_eq!(Subscription::From.as_str(), "from");
    assert_eq!(Subscription::Both.as_str(), "both");
    assert_eq!(Subscription::Remove.as_str(), "remove");
}

#[test]
fn test_ask_type_from_str() {
    assert_eq!("subscribe".parse::<AskType>().unwrap(), AskType::Subscribe);
    assert!("invalid".parse::<AskType>().is_err());
}

#[test]
fn test_roster_item_to_element() {
    let jid: BareJid = "contact@example.com".parse().unwrap();
    let item = RosterItem::with_name(jid, "Alice")
        .set_subscription(Subscription::Both)
        .add_group("Friends");

    let elem = item.to_element();

    assert_eq!(elem.name(), "item");
    assert_eq!(elem.ns(), ROSTER_NS);
    assert_eq!(elem.attr("jid"), Some("contact@example.com"));
    assert_eq!(elem.attr("name"), Some("Alice"));
    assert_eq!(elem.attr("subscription"), Some("both"));

    let groups: Vec<_> = elem.children().filter(|c| c.name() == "group").collect();
    assert_eq!(groups.len(), 1);
}

#[test]
fn test_roster_item_from_element() {
    let elem = Element::builder("item", ROSTER_NS)
        .attr("jid", "contact@example.com")
        .attr("name", "Alice")
        .attr("subscription", "both")
        .attr("ask", "subscribe")
        .append(
            Element::builder("group", ROSTER_NS)
                .append("Friends")
                .build(),
        )
        .build();

    let item = RosterItem::from_element(&elem).unwrap();

    assert_eq!(item.jid.to_string(), "contact@example.com");
    assert_eq!(item.name, Some("Alice".to_string()));
    assert_eq!(item.subscription, Subscription::Both);
    assert_eq!(item.ask, Some(AskType::Subscribe));
    assert_eq!(item.groups, vec!["Friends".to_string()]);
}

#[test]
fn test_roster_item_from_element_minimal() {
    let elem = Element::builder("item", ROSTER_NS)
        .attr("jid", "contact@example.com")
        .build();

    let item = RosterItem::from_element(&elem).unwrap();

    assert_eq!(item.jid.to_string(), "contact@example.com");
    assert_eq!(item.name, None);
    assert_eq!(item.subscription, Subscription::None);
    assert_eq!(item.ask, None);
    assert!(item.groups.is_empty());
}

#[test]
fn test_roster_item_from_element_missing_jid() {
    let elem = Element::builder("item", ROSTER_NS)
        .attr("name", "Alice")
        .build();

    let result = RosterItem::from_element(&elem);
    assert!(result.is_err());
}

#[test]
fn test_is_roster_get() {
    let query_elem = Element::builder("query", ROSTER_NS).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(query_elem),
    };

    assert!(is_roster_get(&iq));
}

#[test]
fn test_is_not_roster_get_wrong_ns() {
    let query_elem = Element::builder("query", "wrong:ns").build();
    let iq = Iq {
        from: None,
        to: None,
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(query_elem),
    };

    assert!(!is_roster_get(&iq));
}

#[test]
fn test_is_not_roster_get_wrong_type() {
    let query_elem = Element::builder("query", ROSTER_NS).build();
    let iq = Iq {
        from: None,
        to: None,
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query_elem),
    };

    assert!(!is_roster_get(&iq));
}

#[test]
fn test_is_roster_set() {
    let query_elem = Element::builder("query", ROSTER_NS)
        .append(
            Element::builder("item", ROSTER_NS)
                .attr("jid", "contact@example.com")
                .build(),
        )
        .build();
    let iq = Iq {
        from: None,
        to: None,
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query_elem),
    };

    assert!(is_roster_set(&iq));
}

#[test]
fn test_parse_roster_get() {
    let query_elem = Element::builder("query", ROSTER_NS)
        .attr("ver", "abc123")
        .build();
    let iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(query_elem),
    };

    let query = parse_roster_get(&iq).unwrap();
    match query.ver {
        RosterVersionRequest::Cached(v) => assert_eq!(v.as_str(), "abc123"),
        other => panic!("expected Cached, got {:?}", other),
    }
    assert!(query.items.is_empty());
}

#[test]
fn test_parse_roster_set() {
    let query_elem = Element::builder("query", ROSTER_NS)
        .append(
            Element::builder("item", ROSTER_NS)
                .attr("jid", "contact@example.com")
                .attr("name", "Alice")
                .attr("subscription", "both")
                .build(),
        )
        .build();
    let iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query_elem),
    };

    let query = parse_roster_set(&iq).unwrap();
    assert_eq!(query.items.len(), 1);
    assert_eq!(query.items[0].jid.to_string(), "contact@example.com");
    assert_eq!(query.items[0].name, Some("Alice".to_string()));
    assert_eq!(query.items[0].subscription, Subscription::None);
}

#[test]
fn test_parse_roster_set_remove() {
    let query_elem = Element::builder("query", ROSTER_NS)
        .append(
            Element::builder("item", ROSTER_NS)
                .attr("jid", "contact@example.com")
                .attr("subscription", "remove")
                .build(),
        )
        .build();
    let iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query_elem),
    };

    let query = parse_roster_set(&iq).unwrap();
    assert_eq!(query.items.len(), 1);
    assert!(query.items[0].subscription.is_remove());
}

#[test]
fn test_parse_roster_set_invalid_subscription_ignored() {
    let query_elem = Element::builder("query", ROSTER_NS)
        .append(
            Element::builder("item", ROSTER_NS)
                .attr("jid", "contact@example.com")
                .attr("subscription", "foobar")
                .build(),
        )
        .build();
    let iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query_elem),
    };

    let query = parse_roster_set(&iq).unwrap();
    assert_eq!(query.items.len(), 1);
    assert_eq!(query.items[0].subscription, Subscription::None);
}

#[test]
fn test_parse_roster_set_empty_items() {
    let query_elem = Element::builder("query", ROSTER_NS).build();
    let iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query_elem),
    };

    let result = parse_roster_set(&iq);
    assert!(result.is_err());
}

#[test]
fn test_parse_roster_set_duplicate_groups_rejected() {
    let query_elem = Element::builder("query", ROSTER_NS)
        .append(
            Element::builder("item", ROSTER_NS)
                .attr("jid", "contact@example.com")
                .append(
                    Element::builder("group", ROSTER_NS)
                        .append("Friends")
                        .build(),
                )
                .append(
                    Element::builder("group", ROSTER_NS)
                        .append("Friends")
                        .build(),
                )
                .build(),
        )
        .build();
    let iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query_elem),
    };

    let result = parse_roster_set(&iq);
    assert!(result.is_err());
}

#[test]
fn test_parse_roster_set_empty_group_rejected() {
    let query_elem = Element::builder("query", ROSTER_NS)
        .append(
            Element::builder("item", ROSTER_NS)
                .attr("jid", "contact@example.com")
                .append(Element::builder("group", ROSTER_NS).append("").build())
                .build(),
        )
        .build();
    let iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query_elem),
    };

    let result = parse_roster_set(&iq);
    assert!(matches!(
        result,
        Err(crate::XmppError::Stanza {
            condition: crate::error::StanzaErrorCondition::NotAcceptable,
            ..
        })
    ));
}

#[test]
fn test_build_roster_result() {
    let query_elem = Element::builder("query", ROSTER_NS).build();
    let original_iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Get(query_elem),
    };

    let items = vec![
        RosterItem::with_name("contact1@example.com".parse().unwrap(), "Alice")
            .set_subscription(Subscription::Both),
        RosterItem::with_name("contact2@example.com".parse().unwrap(), "Bob")
            .set_subscription(Subscription::To),
    ];

    let ver: RosterVersion = "ver123".parse().unwrap();
    let response = build_roster_result(&original_iq, &items, Some(&ver));

    assert_eq!(response.id, "roster-1");
    assert!(matches!(
        response.payload,
        xmpp_parsers::iq::IqType::Result(Some(_))
    ));

    if let xmpp_parsers::iq::IqType::Result(Some(elem)) = response.payload {
        assert_eq!(elem.attr("ver"), Some("ver123"));
        let item_count = elem.children().filter(|c| c.name() == "item").count();
        assert_eq!(item_count, 2);
    }
}

#[test]
fn test_build_roster_result_empty() {
    let query_elem = Element::builder("query", ROSTER_NS).build();
    let original_iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query_elem),
    };

    let response = build_roster_result_empty(&original_iq);

    assert_eq!(response.id, "roster-1");
    assert!(matches!(
        response.payload,
        xmpp_parsers::iq::IqType::Result(None)
    ));
}

#[test]
fn test_build_roster_push() {
    let item = RosterItem::with_name("contact@example.com".parse().unwrap(), "Alice")
        .set_subscription(Subscription::Both);

    let from_jid: BareJid = "user@example.com".parse().unwrap();
    let to_jid: FullJid = "user@example.com/resource".parse().unwrap();
    let ver: RosterVersion = "ver456".parse().unwrap();
    let push = build_roster_push("push-1", &from_jid, &to_jid, &item, Some(&ver));

    assert_eq!(push.id, "push-1");
    assert_eq!(
        push.to.as_ref().unwrap().to_string(),
        "user@example.com/resource"
    );
    assert_eq!(push.from.as_ref().unwrap().to_string(), "user@example.com");

    if let xmpp_parsers::iq::IqType::Set(elem) = push.payload {
        assert_eq!(elem.name(), "query");
        assert_eq!(elem.ns(), ROSTER_NS);
        assert_eq!(elem.attr("ver"), Some("ver456"));

        let items: Vec<_> = elem.children().filter(|c| c.name() == "item").collect();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].attr("jid"), Some("contact@example.com"));
    } else {
        panic!("Expected Set payload");
    }
}

#[test]
fn test_roster_set_result_to_push_item() {
    let jid: BareJid = "contact@example.com".parse().unwrap();

    // Added
    let added = RosterSetResult::Added(RosterItem::with_name(jid.clone(), "Alice"));
    let push_item = added.to_push_item();
    assert_eq!(push_item.name, Some("Alice".to_string()));

    // Removed
    let removed = RosterSetResult::Removed(jid.clone());
    let push_item = removed.to_push_item();
    assert_eq!(push_item.subscription, Subscription::Remove);
}

#[test]
fn test_parse_roster_set_multiple_items_rejected() {
    let query_elem = Element::builder("query", ROSTER_NS)
        .append(
            Element::builder("item", ROSTER_NS)
                .attr("jid", "contact1@example.com")
                .build(),
        )
        .append(
            Element::builder("item", ROSTER_NS)
                .attr("jid", "contact2@example.com")
                .build(),
        )
        .build();
    let iq = Iq {
        from: Some("user@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "roster-1".to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query_elem),
    };

    let result = parse_roster_set(&iq);
    assert!(result.is_err());
}
