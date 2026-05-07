use super::*;
use jid::Jid;
use xmpp_parsers::presence::Type as PresenceType;

fn make_item(jid: &str, subscription: Subscription, ask: Option<AskType>) -> RosterItem {
    let mut item = RosterItem::new(jid.parse().unwrap());
    item.subscription = subscription;
    item.ask = ask;
    item
}

#[test]
fn test_subscription_type_from_presence_type() {
    assert_eq!(
        SubscriptionType::from_presence_type(&PresenceType::Subscribe),
        Some(SubscriptionType::Subscribe)
    );
    assert_eq!(
        SubscriptionType::from_presence_type(&PresenceType::Subscribed),
        Some(SubscriptionType::Subscribed)
    );
    assert_eq!(
        SubscriptionType::from_presence_type(&PresenceType::Unsubscribe),
        Some(SubscriptionType::Unsubscribe)
    );
    assert_eq!(
        SubscriptionType::from_presence_type(&PresenceType::Unsubscribed),
        Some(SubscriptionType::Unsubscribed)
    );
    assert_eq!(
        SubscriptionType::from_presence_type(&PresenceType::Unavailable),
        None
    );
}

#[test]
fn test_outbound_subscribe_sets_ask() {
    let mut item = make_item("contact@example.com", Subscription::None, None);
    SubscriptionStateMachine::apply_outbound_subscribe(&mut item);
    assert_eq!(item.ask, Some(AskType::Subscribe));
    assert_eq!(item.subscription, Subscription::None);
}

#[test]
fn test_inbound_subscribed_none_to_to() {
    let mut item = make_item(
        "contact@example.com",
        Subscription::None,
        Some(AskType::Subscribe),
    );
    SubscriptionStateMachine::apply_inbound_subscribed(&mut item);
    assert_eq!(item.subscription, Subscription::To);
    assert_eq!(item.ask, None);
}

#[test]
fn test_inbound_subscribed_from_to_both() {
    let mut item = make_item(
        "contact@example.com",
        Subscription::From,
        Some(AskType::Subscribe),
    );
    SubscriptionStateMachine::apply_inbound_subscribed(&mut item);
    assert_eq!(item.subscription, Subscription::Both);
    assert_eq!(item.ask, None);
}

#[test]
fn test_outbound_subscribed_none_to_from() {
    let mut item = make_item("contact@example.com", Subscription::None, None);
    SubscriptionStateMachine::apply_outbound_subscribed(&mut item);
    assert_eq!(item.subscription, Subscription::From);
}

#[test]
fn test_outbound_subscribed_to_to_both() {
    let mut item = make_item("contact@example.com", Subscription::To, None);
    SubscriptionStateMachine::apply_outbound_subscribed(&mut item);
    assert_eq!(item.subscription, Subscription::Both);
}

#[test]
fn test_inbound_unsubscribed_to_to_none() {
    let mut item = make_item("contact@example.com", Subscription::To, None);
    SubscriptionStateMachine::apply_inbound_unsubscribed(&mut item);
    assert_eq!(item.subscription, Subscription::None);
}

#[test]
fn test_inbound_unsubscribed_both_to_from() {
    let mut item = make_item("contact@example.com", Subscription::Both, None);
    SubscriptionStateMachine::apply_inbound_unsubscribed(&mut item);
    assert_eq!(item.subscription, Subscription::From);
}

#[test]
fn test_outbound_unsubscribed_from_to_none() {
    let mut item = make_item("contact@example.com", Subscription::From, None);
    SubscriptionStateMachine::apply_outbound_unsubscribed(&mut item);
    assert_eq!(item.subscription, Subscription::None);
}

#[test]
fn test_outbound_unsubscribed_both_to_to() {
    let mut item = make_item("contact@example.com", Subscription::Both, None);
    SubscriptionStateMachine::apply_outbound_unsubscribed(&mut item);
    assert_eq!(item.subscription, Subscription::To);
}

#[test]
fn test_outbound_unsubscribe_to_to_none() {
    let mut item = make_item("contact@example.com", Subscription::To, None);
    SubscriptionStateMachine::apply_outbound_unsubscribe(&mut item);
    assert_eq!(item.subscription, Subscription::None);
}

#[test]
fn test_outbound_unsubscribe_both_to_from() {
    let mut item = make_item("contact@example.com", Subscription::Both, None);
    SubscriptionStateMachine::apply_outbound_unsubscribe(&mut item);
    assert_eq!(item.subscription, Subscription::From);
}

#[test]
fn test_should_receive_presence() {
    assert!(!SubscriptionStateMachine::should_receive_presence(
        Subscription::None
    ));
    assert!(SubscriptionStateMachine::should_receive_presence(
        Subscription::To
    ));
    assert!(!SubscriptionStateMachine::should_receive_presence(
        Subscription::From
    ));
    assert!(SubscriptionStateMachine::should_receive_presence(
        Subscription::Both
    ));
}

#[test]
fn test_should_send_presence() {
    assert!(!SubscriptionStateMachine::should_send_presence(
        Subscription::None
    ));
    assert!(!SubscriptionStateMachine::should_send_presence(
        Subscription::To
    ));
    assert!(SubscriptionStateMachine::should_send_presence(
        Subscription::From
    ));
    assert!(SubscriptionStateMachine::should_send_presence(
        Subscription::Both
    ));
}

#[test]
fn test_build_subscription_presence() {
    let from: BareJid = "user@example.com".parse().unwrap();
    let to: BareJid = "contact@example.com".parse().unwrap();

    let pres = build_subscription_presence(
        SubscriptionType::Subscribe,
        &from,
        &to,
        Some("Please add me"),
        &[],
    );

    assert_eq!(pres.type_, PresenceType::Subscribe);
    assert_eq!(pres.from, Some(Jid::from(from)));
    assert_eq!(pres.to, Some(Jid::from(to)));
    assert_eq!(
        pres.statuses.values().next(),
        Some(&"Please add me".to_string())
    );
}

#[test]
fn test_build_available_presence_includes_waddle_caps() {
    let from: jid::FullJid = "user@example.com/resource".parse().unwrap();
    let to: BareJid = "contact@example.com".parse().unwrap();

    let pres = build_available_presence(&from, &to, Some("chat"), Some("Ready"), 5);

    let caps = pres
        .payloads
        .iter()
        .find(|payload| payload.name() == "c" && payload.ns() == crate::xep::NS_CAPS)
        .expect("available presence must include caps");

    assert_eq!(caps.attr("node"), Some(crate::xep::WADDLE_CAPS_NODE));
}

#[test]
fn test_parse_subscription_presence() {
    let sender: BareJid = "user@example.com".parse().unwrap();
    let target: BareJid = "contact@example.com".parse().unwrap();

    let mut pres = Presence::new(PresenceType::Subscribe);
    pres.to = Some(Jid::from(target.clone()));
    pres.statuses.insert(String::new(), "Hello".to_string());

    let action = parse_subscription_presence(&pres, &sender).unwrap();

    match action {
        PresenceAction::Subscription(req) => {
            assert_eq!(req.subscription_type, SubscriptionType::Subscribe);
            assert_eq!(req.from, sender);
            assert_eq!(req.to, target);
            assert_eq!(req.status, Some("Hello".to_string()));
        }
        _ => panic!("Expected Subscription action"),
    }
}

#[test]
fn test_parse_probe_presence() {
    let sender: BareJid = "user@example.com".parse().unwrap();
    let target: BareJid = "contact@example.com".parse().unwrap();

    let mut pres = Presence::new(PresenceType::Probe);
    pres.to = Some(Jid::from(target.clone()));

    let action = parse_subscription_presence(&pres, &sender).unwrap();

    match action {
        PresenceAction::Probe {
            from,
            to,
            to_was_full,
        } => {
            assert_eq!(from, sender);
            assert_eq!(to, target);
            assert!(!to_was_full);
        }
        _ => panic!("Expected Probe action"),
    }
}

#[test]
fn test_parse_probe_presence_full_jid() {
    let sender: BareJid = "user@example.com".parse().unwrap();
    let target: jid::FullJid = "contact@example.com/resource".parse().unwrap();

    let mut pres = Presence::new(PresenceType::Probe);
    pres.to = Some(Jid::from(target.clone()));

    let action = parse_subscription_presence(&pres, &sender).unwrap();

    match action {
        PresenceAction::Probe {
            from,
            to,
            to_was_full,
        } => {
            assert_eq!(from, sender);
            assert_eq!(to, target.to_bare());
            assert!(to_was_full);
        }
        _ => panic!("Expected Probe action"),
    }
}

#[test]
fn test_parse_regular_presence() {
    let sender: BareJid = "user@example.com".parse().unwrap();

    // Available presence (no type)
    let pres = Presence::new(PresenceType::None);
    let action = parse_subscription_presence(&pres, &sender).unwrap();

    match action {
        PresenceAction::PresenceUpdate(_) => {}
        _ => panic!("Expected PresenceUpdate action"),
    }
}

#[test]
fn test_pending_subscription() {
    let from: BareJid = "contact@example.com".parse().unwrap();
    let pending = PendingSubscription::new(from.clone());

    assert_eq!(pending.from, from);
    assert!(pending.status.is_none());
}
