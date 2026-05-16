use super::*;
use minidom::Element;
use xmpp_parsers::iq::IqType;
use xmpp_parsers::message::MessageType;

#[test]
fn parse_publish_request() {
    let xml = r#"<iq xmlns='jabber:client' type='set' from='user@example.com' to='user@example.com' id='pub1'>
        <pubsub xmlns='http://jabber.org/protocol/pubsub'>
            <publish node='urn:xmpp:bookmarks:1'>
                <item id='test@conference.example.org'>
                    <conference xmlns='urn:xmpp:bookmarks:1' autojoin='true'/>
                </item>
            </publish>
        </pubsub>
    </iq>"#;

    let elem: Element = xml.parse().expect("valid XML");
    let iq = Iq::try_from(elem).expect("valid IQ");
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
    let xml = r#"<iq xmlns='jabber:client' type='set' from='example.com' to='push.example.com' id='push1'>
        <pubsub xmlns='http://jabber.org/protocol/pubsub'>
            <publish node='push-node-1'>
                <item id='pending-row-1'>
                    <notification xmlns='urn:xmpp:push:0'/>
                </item>
            </publish>
            <publish-options>
                <x xmlns='jabber:x:data' type='submit'>
                    <field var='FORM_TYPE' type='hidden'>
                        <value>http://jabber.org/protocol/pubsub#publish-options</value>
                    </field>
                    <field var='secret'>
                        <value>server-secret</value>
                    </field>
                </x>
            </publish-options>
        </pubsub>
    </iq>"#;

    let elem: Element = xml.parse().expect("valid XML");
    let iq = Iq::try_from(elem).expect("valid IQ");
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
    let xml = r#"<iq xmlns='jabber:client' type='set' from='romeo@example.com' id='sub1'>
        <pubsub xmlns='http://jabber.org/protocol/pubsub'>
            <subscribe node='urn:xmpp:nick' jid='romeo@example.com'/>
        </pubsub>
    </iq>"#;

    let iq = Iq::try_from(xml.parse::<Element>().expect("valid XML")).expect("valid IQ");
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
    let xml = r#"<iq xmlns='jabber:client' type='set' from='user@example.com' id='cfg1'>
        <pubsub xmlns='http://jabber.org/protocol/pubsub'>
            <configure node='space'/>
        </pubsub>
    </iq>"#;

    let iq = Iq::try_from(xml.parse::<Element>().expect("valid XML")).expect("valid IQ");
    let request = parse_pubsub_iq(&iq).expect("should parse");

    match request {
        PubSubRequest::ConfigureNode { node } => assert_eq!(node, "space"),
        other => panic!("Expected configure request, got {other:?}"),
    }
}

#[test]
fn parse_owner_subscriptions_as_unsupported_manage_subscriptions() {
    let xml = r#"<iq xmlns='jabber:client' type='get' from='owner@example.com' id='subs1'>
        <pubsub xmlns='http://jabber.org/protocol/pubsub#owner'>
            <subscriptions node='space'/>
        </pubsub>
    </iq>"#;

    let iq = Iq::try_from(xml.parse::<Element>().expect("valid XML")).expect("valid IQ");
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
    let xml = r#"<iq xmlns='jabber:client' type='get' from='owner@example.com' id='subs1'>
        <pubsub xmlns='http://jabber.org/protocol/pubsub#owner'>
            <subscriptions node='space'/>
        </pubsub>
    </iq>"#;
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid XML")).expect("valid IQ");
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
    let xml = r#"<iq xmlns='jabber:client' type='get' id='test1'>
        <pubsub xmlns='http://jabber.org/protocol/pubsub'>
            <items node='test'/>
        </pubsub>
    </iq>"#;

    let elem: Element = xml.parse().expect("valid XML");
    let iq = Iq::try_from(elem).expect("valid IQ");

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
