use super::*;
use minidom::Element;
use xmpp_parsers::iq::IqType;
use xmpp_parsers::message::MessageType;

const NS_XDATA: &str = "jabber:x:data";
const PUBSUB_PUBLISH_OPTIONS_FORM_TYPE: &str = "http://jabber.org/protocol/pubsub#publish-options";

fn iq_with_payload(id: &str, from: Option<&str>, to: Option<&str>, payload: IqType) -> Iq {
    Iq {
        from: from.map(|jid| jid.parse::<Jid>().expect("valid from jid")),
        to: to.map(|jid| jid.parse::<Jid>().expect("valid to jid")),
        id: id.to_string(),
        payload,
    }
}

fn pubsub_payload(ns: &str, children: impl IntoIterator<Item = Element>) -> Element {
    let mut builder = Element::builder("pubsub", ns);
    for child in children {
        builder = builder.append(child);
    }
    builder.build()
}

fn xdata_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_XDATA)
        .attr("var", var)
        .append(Element::builder("value", NS_XDATA).append(value).build())
        .build()
}

#[test]
fn parse_publish_request() {
    let item = PubSubItem::new(
        Some("test@conference.example.org".to_string()),
        Some(
            Element::builder("conference", "urn:xmpp:bookmarks:1")
                .attr("autojoin", "true")
                .build(),
        ),
    )
    .to_element(NS_PUBSUB);
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr("node", "urn:xmpp:bookmarks:1")
        .append(item)
        .build();
    let iq = iq_with_payload(
        "pub1",
        Some("user@example.com"),
        Some("user@example.com"),
        IqType::Set(pubsub_payload(NS_PUBSUB, [publish])),
    );
    let request = parse_pubsub_iq(&iq).expect("should parse");

    match request {
        PubSubRequest::Publish {
            node,
            item,
            publish_options,
        } => {
            assert_eq!(node, "urn:xmpp:bookmarks:1");
            assert_eq!(item.id.as_deref(), Some("test@conference.example.org"));
            assert!(item.payload.is_some());
            assert!(publish_options.is_none());
        }
        other => panic!("Expected publish request, got {other:?}"),
    }
}

#[test]
fn parse_publish_request_with_publish_options() {
    let item = PubSubItem::new(
        Some("pending-row-1".to_string()),
        Some(Element::builder("notification", "urn:xmpp:push:0").build()),
    )
    .to_element(NS_PUBSUB);
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr("node", "push-node-1")
        .append(item)
        .build();
    let publish_options = Element::builder("publish-options", NS_PUBSUB)
        .append(
            Element::builder("x", NS_XDATA)
                .attr("type", "submit")
                .append(
                    Element::builder("field", NS_XDATA)
                        .attr("var", "FORM_TYPE")
                        .attr("type", "hidden")
                        .append(
                            Element::builder("value", NS_XDATA)
                                .append(PUBSUB_PUBLISH_OPTIONS_FORM_TYPE)
                                .build(),
                        )
                        .build(),
                )
                .append(xdata_field("secret", "server-secret"))
                .build(),
        )
        .build();
    let iq = iq_with_payload(
        "push1",
        Some("example.com"),
        Some("push.example.com"),
        IqType::Set(pubsub_payload(NS_PUBSUB, [publish, publish_options])),
    );
    let request = parse_pubsub_iq(&iq).expect("should parse");

    match request {
        PubSubRequest::Publish {
            node,
            item,
            publish_options,
        } => {
            assert_eq!(node, "push-node-1");
            assert_eq!(item.id.as_deref(), Some("pending-row-1"));
            let publish_options = publish_options.expect("publish-options form");
            assert_eq!(publish_options.name(), "x");
            assert_eq!(publish_options.ns(), "jabber:x:data");
            assert!(String::from(publish_options.as_ref()).contains("server-secret"));
        }
        other => panic!("Expected publish request, got {other:?}"),
    }
}

#[test]
fn parse_subscribe_request_uses_typed_jid() {
    let subscribe = Element::builder("subscribe", NS_PUBSUB)
        .attr("node", "urn:xmpp:nick")
        .attr("jid", "romeo@example.com")
        .build();
    let iq = iq_with_payload(
        "sub1",
        Some("romeo@example.com"),
        None,
        IqType::Set(pubsub_payload(NS_PUBSUB, [subscribe])),
    );
    let request = parse_pubsub_iq(&iq).expect("should parse");

    match request {
        PubSubRequest::Subscribe { node, jid } => {
            assert_eq!(node, "urn:xmpp:nick");
            assert_eq!(jid.to_string(), "romeo@example.com");
        }
        other => panic!("Expected subscribe request, got {other:?}"),
    }
}

#[test]
fn parse_configure_request() {
    let configure = Element::builder("configure", NS_PUBSUB)
        .attr("node", "space")
        .build();
    let iq = iq_with_payload(
        "cfg1",
        Some("user@example.com"),
        None,
        IqType::Set(pubsub_payload(NS_PUBSUB, [configure])),
    );
    let request = parse_pubsub_iq(&iq).expect("should parse");

    match request {
        PubSubRequest::ConfigureNode { node } => assert_eq!(node, "space"),
        other => panic!("Expected configure request, got {other:?}"),
    }
}

#[test]
fn parse_owner_subscriptions_as_unsupported_manage_subscriptions() {
    let subscriptions = Element::builder("subscriptions", NS_PUBSUB_OWNER)
        .attr("node", "space")
        .build();
    let iq = iq_with_payload(
        "subs1",
        Some("owner@example.com"),
        None,
        IqType::Get(pubsub_payload(NS_PUBSUB_OWNER, [subscriptions])),
    );
    let request = parse_pubsub_iq(&iq).expect("should parse unsupported feature");

    match request {
        PubSubRequest::Unsupported { feature } => {
            assert_eq!(feature, PubSubUnsupportedFeature::ManageSubscriptions);
        }
        other => panic!("Expected unsupported feature request, got {other:?}"),
    }
}

#[test]
fn unsupported_feature_error_includes_pubsub_condition() {
    let subscriptions = Element::builder("subscriptions", NS_PUBSUB_OWNER)
        .attr("node", "space")
        .build();
    let iq = iq_with_payload(
        "subs1",
        Some("owner@example.com"),
        None,
        IqType::Get(pubsub_payload(NS_PUBSUB_OWNER, [subscriptions])),
    );
    let response = build_pubsub_error(
        &iq,
        PubSubError::UnsupportedFeature(PubSubUnsupportedFeature::ManageSubscriptions),
    );
    let IqType::Error(error) = response.payload else {
        panic!("expected error response");
    };

    assert_eq!(
        error.defined_condition,
        xmpp_parsers::stanza_error::DefinedCondition::FeatureNotImplemented
    );
    let unsupported = error.other.expect("pubsub unsupported condition");
    assert_eq!(unsupported.name(), "unsupported");
    assert_eq!(unsupported.ns(), NS_PUBSUB_ERRORS);
    assert_eq!(unsupported.attr("feature"), Some("manage-subscriptions"));
}

#[test]
fn build_and_parse_pubsub_event_message() {
    let from: Jid = "juliet@example.com".parse().expect("valid jid");
    let to: Jid = "romeo@example.com/balcony".parse().expect("valid jid");
    let payload = Element::builder("nick", "http://jabber.org/protocol/nick")
        .append("Juliet")
        .build();
    let event = PubSubEvent::new(
        "http://jabber.org/protocol/nick",
        vec![PubSubItem::new(Some("latest".to_string()), Some(payload))],
    );

    let message = build_pubsub_event(&from, &to, &event);

    assert!(is_pubsub_event(&message));
    assert_eq!(
        message.type_,
        MessageType::Headline,
        "XEP-0060 §12.18 default and XEP-0163 §4.3 require headline"
    );

    let parsed = parse_pubsub_event(&message).expect("event should parse");
    assert_eq!(parsed.node, "http://jabber.org/protocol/nick");
    assert_eq!(parsed.items.len(), 1);
    assert_eq!(parsed.items[0].id.as_deref(), Some("latest"));
}

#[test]
fn pubsub_item_round_trips() {
    let payload = Element::builder("test", "test:ns")
        .attr("foo", "bar")
        .build();
    let item = PubSubItem::new(Some("item-1".to_string()), Some(payload));
    let elem = item.to_element(NS_PUBSUB);
    let parsed = PubSubItem::from_element(&elem);

    assert_eq!(parsed.id.as_deref(), Some("item-1"));
    assert!(parsed.payload.is_some());
    assert!(parsed.publisher.is_none());
}

#[test]
fn pubsub_item_publisher_attribute_round_trips() {
    let publisher: BareJid = "alice@example.com".parse().expect("valid bare");
    let item = PubSubItem::new(Some("e1".to_string()), None).with_publisher(Some(publisher));

    let elem = item.to_element(NS_PUBSUB_EVENT);
    assert_eq!(elem.attr("publisher"), Some("alice@example.com"));

    let parsed = PubSubItem::from_element(&elem);
    assert_eq!(
        parsed.publisher.as_ref().map(BareJid::to_string).as_deref(),
        Some("alice@example.com")
    );
}

#[test]
fn pubsub_item_omits_publisher_when_unset() {
    let item = PubSubItem::new(Some("e2".to_string()), None);
    let elem = item.to_element(NS_PUBSUB_EVENT);
    assert!(elem.attr("publisher").is_none());
}

#[test]
fn is_pubsub_iq_detects_pubsub_requests() {
    let items = Element::builder("items", NS_PUBSUB)
        .attr("node", "test")
        .build();
    let iq = iq_with_payload(
        "test1",
        None,
        None,
        IqType::Get(pubsub_payload(NS_PUBSUB, [items])),
    );

    assert!(is_pubsub_iq(&iq));
}

#[test]
fn build_pubsub_success_preserves_iq_routing() {
    let iq = Iq {
        from: Some("romeo@example.com".parse().expect("valid jid")),
        to: Some("juliet@example.com".parse().expect("valid jid")),
        id: "ok-1".to_string(),
        payload: IqType::Get(Element::builder("ping", "urn:xmpp:ping").build()),
    };

    let response = build_pubsub_success(&iq);
    assert_eq!(response.id, "ok-1");
    assert_eq!(response.from, iq.to);
    assert_eq!(response.to, iq.from);
}
