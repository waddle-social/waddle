use super::*;

#[test]
fn test_roster_item_from_element_happy_path() {
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
fn test_roster_item_from_element_missing_jid() {
    let elem = Element::builder("item", ROSTER_NS)
        .attr("name", "Alice")
        .build();

    let result = RosterItem::from_element(&elem);
    assert!(result.is_err());
    match result.unwrap_err() {
        CoreError::BadRequest(Some(msg)) => assert!(msg.contains("missing 'jid'")),
        e => panic!("unexpected error: {:?}", e),
    }
}

#[test]
fn test_roster_item_from_element_invalid_subscription() {
    let elem = Element::builder("item", ROSTER_NS)
        .attr("jid", "contact@example.com")
        .attr("subscription", "bogus")
        .build();

    let result = RosterItem::from_element(&elem);
    assert!(result.is_err());
}

#[test]
fn test_roster_item_from_element_invalid_ask() {
    let elem = Element::builder("item", ROSTER_NS)
        .attr("jid", "contact@example.com")
        .attr("ask", "bogus")
        .build();

    let result = RosterItem::from_element(&elem);
    assert!(result.is_err());
}

#[test]
fn test_roster_item_to_element_roundtrip() {
    let jid: BareJid = "contact@example.com".parse().unwrap();
    let original = RosterItem::with_name(jid, "Bob")
        .set_subscription(Subscription::To)
        .set_ask(Some(AskType::Subscribe))
        .add_group("Work");

    let elem = original.to_element();
    let parsed = RosterItem::from_element(&elem).unwrap();

    assert_eq!(parsed.jid, original.jid);
    assert_eq!(parsed.name, original.name);
    assert_eq!(parsed.subscription, original.subscription);
    assert_eq!(parsed.ask, original.ask);
    assert_eq!(parsed.groups, original.groups);
}

#[test]
fn test_subscription_from_str_all_variants() {
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
fn test_ask_type_from_str() {
    assert_eq!("subscribe".parse::<AskType>().unwrap(), AskType::Subscribe);
    assert!("invalid".parse::<AskType>().is_err());
}

#[test]
fn test_build_roster_get_iq() {
    let iq = build_roster_get_iq("r1");
    assert_eq!(iq.name(), "iq");
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("id"), Some("r1"));
    let query = iq.children().next().unwrap();
    assert_eq!(query.name(), "query");
    assert_eq!(query.ns(), ROSTER_NS);
}

#[test]
fn test_roster_version_generate_is_non_empty_and_unique() {
    let a = RosterVersion::generate();
    let b = RosterVersion::generate();
    assert!(!a.as_str().is_empty());
    assert_ne!(a, b);
}

#[test]
fn test_roster_version_from_str_rejects_empty() {
    assert!("".parse::<RosterVersion>().is_err());
    assert_eq!("ver14".parse::<RosterVersion>().unwrap().as_str(), "ver14");
}

#[test]
fn test_roster_version_request_from_attr() {
    assert_eq!(
        RosterVersionRequest::from_attr(None),
        RosterVersionRequest::Absent
    );
    assert_eq!(
        RosterVersionRequest::from_attr(Some("")),
        RosterVersionRequest::Bootstrap
    );
    match RosterVersionRequest::from_attr(Some("ver42")) {
        RosterVersionRequest::Cached(v) => assert_eq!(v.as_str(), "ver42"),
        other => panic!("expected Cached, got {:?}", other),
    }
}

#[test]
fn test_roster_version_request_signals_support() {
    assert!(!RosterVersionRequest::Absent.signals_support());
    assert!(RosterVersionRequest::Bootstrap.signals_support());
    assert!(RosterVersionRequest::Cached(RosterVersion::generate()).signals_support());
}

#[test]
fn test_build_roster_set_iq() {
    let jid: BareJid = "friend@example.com".parse().unwrap();
    let item = RosterItem::with_name(jid, "Friend");
    let iq = build_roster_set_iq("s1", &item);
    assert_eq!(iq.name(), "iq");
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("id"), Some("s1"));
    let query = iq.children().next().unwrap();
    assert_eq!(query.name(), "query");
    assert_eq!(query.ns(), ROSTER_NS);
    let item_elem = query.children().next().unwrap();
    assert_eq!(item_elem.attr("jid"), Some("friend@example.com"));
}
