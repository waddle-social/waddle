use super::{
    build_pubsub_configure_form_result, build_pubsub_error, build_pubsub_event,
    build_pubsub_success, is_pubsub_event, is_pubsub_iq, parse_pubsub_event, parse_pubsub_iq,
    BareJid, Iq, Jid, PubSubError, PubSubEvent, PubSubItem, PubSubRequest,
    PubSubUnsupportedFeature, SubscriptionState, NS_PUBSUB, NS_PUBSUB_ERRORS, NS_PUBSUB_EVENT,
    NS_PUBSUB_OWNER,
};
use minidom::Element;
use xmpp_parsers::message::MessageType;

const NS_XDATA: &str = "jabber:x:data";
const PUBSUB_PUBLISH_OPTIONS_FORM_TYPE: &str = "http://jabber.org/protocol/pubsub#publish-options";

/// Test-local payload kind so the iq_with_payload helper can stay shaped
/// the same way regardless of which Iq variant it is producing.
enum TestIqPayload {
    Get(Element),
    Set(Element),
}

fn iq_with_payload(id: &str, from: Option<&str>, to: Option<&str>, payload: TestIqPayload) -> Iq {
    let from = from.map(|jid| jid.parse::<Jid>().expect("valid from jid"));
    let to = to.map(|jid| jid.parse::<Jid>().expect("valid to jid"));
    let id = id.to_string();
    match payload {
        TestIqPayload::Get(payload) => Iq::Get {
            from,
            to,
            id,
            payload,
        },
        TestIqPayload::Set(payload) => Iq::Set {
            from,
            to,
            id,
            payload,
        },
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
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(Element::builder("value", NS_XDATA).append(value).build())
        .build()
}

fn xdata_field_value(form: &Element, var: &str) -> Option<String> {
    form.children()
        .find(|field| field.is("field", NS_XDATA) && field.attr("var") == Some(var))
        .and_then(|field| field.get_child("value", NS_XDATA))
        .map(|value| value.text())
}

#[test]
fn parse_publish_request() {
    let item = PubSubItem::new(
        Some("test@conference.example.org".to_string()),
        Some(
            Element::builder("conference", "urn:xmpp:bookmarks:1")
                .attr(minidom::rxml::xml_ncname!("autojoin").to_owned(), "true")
                .build(),
        ),
    )
    .to_element(NS_PUBSUB);
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "urn:xmpp:bookmarks:1",
        )
        .append(item)
        .build();
    let iq = iq_with_payload(
        "pub1",
        Some("user@example.com"),
        Some("user@example.com"),
        TestIqPayload::Set(pubsub_payload(NS_PUBSUB, [publish])),
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
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "push-node-1")
        .append(item)
        .build();
    let publish_options = Element::builder("publish-options", NS_PUBSUB)
        .append(
            Element::builder("x", NS_XDATA)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                .append(
                    Element::builder("field", NS_XDATA)
                        .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
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
        TestIqPayload::Set(pubsub_payload(NS_PUBSUB, [publish, publish_options])),
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
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "urn:xmpp:nick",
        )
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "romeo@example.com",
        )
        .build();
    let iq = iq_with_payload(
        "sub1",
        Some("romeo@example.com"),
        None,
        TestIqPayload::Set(pubsub_payload(NS_PUBSUB, [subscribe])),
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
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "space")
        .build();
    let iq = iq_with_payload(
        "cfg1",
        Some("user@example.com"),
        None,
        TestIqPayload::Set(pubsub_payload(NS_PUBSUB, [configure])),
    );
    let request = parse_pubsub_iq(&iq).expect("should parse");

    match request {
        PubSubRequest::ConfigureNode { node } => assert_eq!(node, "space"),
        other => panic!("Expected configure request, got {other:?}"),
    }
}

#[test]
fn parse_configure_set_keeps_partial_fields_partial() {
    let configure = Element::builder("configure", NS_PUBSUB_OWNER)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "space")
        .append(
            Element::builder("x", NS_XDATA)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                .append(xdata_field("pubsub#max_items", "200"))
                .build(),
        )
        .build();
    let iq = iq_with_payload(
        "cfg-set1",
        Some("owner@example.com"),
        None,
        TestIqPayload::Set(pubsub_payload(NS_PUBSUB_OWNER, [configure])),
    );
    let request = parse_pubsub_iq(&iq).expect("should parse");

    match request {
        PubSubRequest::ConfigureNodeSet { node, config } => {
            assert_eq!(node, "space");
            assert_eq!(config.max_items, Some(200));
            assert_eq!(config.access_model, None);
        }
        other => panic!("Expected configure-set request, got {other:?}"),
    }
}

#[test]
fn parse_configure_set_accepts_typed_pubsub_type() {
    let configure = Element::builder("configure", NS_PUBSUB_OWNER)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "stories")
        .append(
            Element::builder("x", NS_XDATA)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                .append(xdata_field("pubsub#type", crate::xep0501::NS_STORIES))
                .build(),
        )
        .build();
    let iq = iq_with_payload(
        "cfg-set-type",
        Some("owner@example.com"),
        None,
        TestIqPayload::Set(pubsub_payload(NS_PUBSUB_OWNER, [configure])),
    );
    let request = parse_pubsub_iq(&iq).expect("should parse");

    match request {
        PubSubRequest::ConfigureNodeSet { config, .. } => {
            assert_eq!(
                config.node_type,
                Some(crate::pubsub::PubSubNodeType::PubsubSocialFeedStories)
            );
        }
        other => panic!("Expected configure-set request, got {other:?}"),
    }
}

#[test]
fn configure_form_result_emits_pubsub_type_for_typed_node() {
    let iq = iq_with_payload(
        "cfg-get-type",
        Some("owner@example.com"),
        None,
        TestIqPayload::Get(Element::builder("query", "jabber:iq:version").build()),
    );
    let result = build_pubsub_configure_form_result(
        &iq,
        "stories",
        &crate::pubsub::NodeConfig::community_stories(),
    );
    let Iq::Result {
        payload: Some(pubsub),
        ..
    } = result
    else {
        panic!("expected result payload");
    };
    let form = pubsub
        .get_child("configure", NS_PUBSUB_OWNER)
        .and_then(|configure| configure.get_child("x", NS_XDATA))
        .expect("configure data form");

    assert_eq!(
        xdata_field_value(form, "pubsub#type").as_deref(),
        Some(crate::xep0501::NS_STORIES)
    );
}

#[test]
fn space_presets_carry_the_spaces_pubsub_type_and_the_calendar_does_not() {
    use crate::pubsub::{NodeConfig, PubSubNodeType};
    assert_eq!(
        NodeConfig::spaces_public().node_type,
        Some(PubSubNodeType::Space)
    );
    assert_eq!(
        NodeConfig::spaces_private().node_type,
        Some(PubSubNodeType::Space)
    );
    assert_eq!(NodeConfig::community_calendar().node_type, None);
}

#[test]
fn configure_form_reports_purge_offline_false_and_refuses_true() {
    let iq = iq_with_payload(
        "cfg-purge",
        Some("owner@example.com"),
        None,
        TestIqPayload::Get(Element::builder("query", "jabber:iq:version").build()),
    );
    let Iq::Result {
        payload: Some(pubsub),
        ..
    } = build_pubsub_configure_form_result(
        &iq,
        "space",
        &crate::pubsub::NodeConfig::spaces_public(),
    )
    else {
        panic!("expected result payload");
    };
    let form = pubsub
        .get_child("configure", NS_PUBSUB_OWNER)
        .and_then(|configure| configure.get_child("x", NS_XDATA))
        .expect("configure data form");
    assert_eq!(
        xdata_field_value(form, "pubsub#purge_offline").as_deref(),
        Some("0")
    );
    assert_eq!(
        xdata_field_value(form, "pubsub#type").as_deref(),
        Some("urn:xmpp:spaces:0")
    );

    for (value, accepted) in [("0", true), ("false", true), ("1", false), ("true", false)] {
        let configure = Element::builder("configure", NS_PUBSUB_OWNER)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), "space")
            .append(
                Element::builder("x", NS_XDATA)
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                    .append(xdata_field("pubsub#purge_offline", value))
                    .build(),
            )
            .build();
        let iq = iq_with_payload(
            "cfg-purge-set",
            Some("owner@example.com"),
            None,
            TestIqPayload::Set(pubsub_payload(NS_PUBSUB_OWNER, [configure])),
        );
        assert_eq!(
            parse_pubsub_iq(&iq).is_ok(),
            accepted,
            "purge_offline={value}"
        );
    }
}

#[test]
fn parse_owner_subscriptions_get() {
    let subscriptions = Element::builder("subscriptions", NS_PUBSUB_OWNER)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "space")
        .build();
    let iq = iq_with_payload(
        "subs1",
        Some("owner@example.com"),
        None,
        TestIqPayload::Get(pubsub_payload(NS_PUBSUB_OWNER, [subscriptions])),
    );
    let request = parse_pubsub_iq(&iq).expect("should parse unsupported feature");

    match request {
        PubSubRequest::OwnerSubscriptionsGet { node } => {
            assert_eq!(node, "space");
        }
        other => panic!("Expected owner subscriptions get request, got {other:?}"),
    }
}

#[test]
fn parse_owner_subscriptions_set() {
    let subscriptions = Element::builder("subscriptions", NS_PUBSUB_OWNER)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "space")
        .append(
            Element::builder("subscription", NS_PUBSUB_OWNER)
                .attr(
                    minidom::rxml::xml_ncname!("jid").to_owned(),
                    "alice@example.com",
                )
                .attr(
                    minidom::rxml::xml_ncname!("subscription").to_owned(),
                    "subscribed",
                )
                .build(),
        )
        .append(
            Element::builder("subscription", NS_PUBSUB_OWNER)
                .attr(
                    minidom::rxml::xml_ncname!("jid").to_owned(),
                    "bob@example.com",
                )
                .attr(
                    minidom::rxml::xml_ncname!("subscription").to_owned(),
                    "none",
                )
                .attr(minidom::rxml::xml_ncname!("subid").to_owned(), "sub-1")
                .build(),
        )
        .build();
    let iq = iq_with_payload(
        "subs-set",
        Some("owner@example.com"),
        None,
        TestIqPayload::Set(pubsub_payload(NS_PUBSUB_OWNER, [subscriptions])),
    );
    let request = parse_pubsub_iq(&iq).expect("should parse owner subscriptions set");

    match request {
        PubSubRequest::OwnerSubscriptionsSet { node, changes } => {
            assert_eq!(node, "space");
            assert_eq!(changes.len(), 2);
            assert_eq!(changes[0].1, SubscriptionState::Subscribed);
            assert_eq!(changes[1].1, SubscriptionState::None);
            assert_eq!(changes[1].2.as_deref(), Some("sub-1"));
        }
        other => panic!("Expected owner subscriptions set request, got {other:?}"),
    }
}

#[test]
fn unsupported_feature_error_includes_pubsub_condition() {
    let subscriptions = Element::builder("subscriptions", NS_PUBSUB_OWNER)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "space")
        .build();
    let iq = iq_with_payload(
        "subs1",
        Some("owner@example.com"),
        None,
        TestIqPayload::Get(pubsub_payload(NS_PUBSUB_OWNER, [subscriptions])),
    );
    let response = build_pubsub_error(
        &iq,
        PubSubError::UnsupportedFeature(PubSubUnsupportedFeature::ManageSubscriptions),
    );
    let Iq::Error { error, .. } = response else {
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
        .attr(minidom::rxml::xml_ncname!("foo").to_owned(), "bar")
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
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "test")
        .build();
    let iq = iq_with_payload(
        "test1",
        None,
        None,
        TestIqPayload::Get(pubsub_payload(NS_PUBSUB, [items])),
    );

    assert!(is_pubsub_iq(&iq));
}

#[test]
fn build_pubsub_success_preserves_iq_routing() {
    let from: Option<Jid> = Some("romeo@example.com".parse().expect("valid jid"));
    let to: Option<Jid> = Some("juliet@example.com".parse().expect("valid jid"));
    let iq = Iq::Get {
        from: from.clone(),
        to: to.clone(),
        id: "ok-1".to_string(),
        payload: Element::builder("ping", "urn:xmpp:ping").build(),
    };

    let response = build_pubsub_success(&iq);
    assert_eq!(response.id(), "ok-1");
    assert_eq!(response.from(), to.as_ref());
    assert_eq!(response.to(), from.as_ref());
}

#[test]
fn node_deletion_event_names_the_node_and_subscriber() {
    let service = "spaces.example.test".parse().expect("Space service");
    let recipient = "alice@example.test/phone"
        .parse()
        .expect("bound subscriber");
    let event = crate::pubsub::build_pubsub_delete_event(&service, &recipient, "engineering");
    assert_eq!(event.from, Some(service));
    assert_eq!(event.to, Some(recipient));
    assert_eq!(event.type_, xmpp_parsers::message::MessageType::Headline);
    assert_eq!(event.payloads.len(), 1);
    let ns = crate::pubsub::NS_PUBSUB_EVENT;
    assert!(event.payloads[0].is("event", ns));
    let deleted = event.payloads[0]
        .get_child("delete", ns)
        .expect("node deletion");
    assert_eq!(deleted.attr("node"), Some("engineering"));
    assert!(deleted.children().next().is_none());
}

fn publish_options_form(form_type: &str, fields: &[(&str, &str)]) -> Element {
    let mut form = Element::builder("x", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", NS_XDATA)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .append(
                    Element::builder("value", NS_XDATA)
                        .append(form_type)
                        .build(),
                )
                .build(),
        );
    for (var, value) in fields {
        form = form.append(
            Element::builder("field", NS_XDATA)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), *var)
                .append(Element::builder("value", NS_XDATA).append(*value).build())
                .build(),
        );
    }
    form.build()
}

#[test]
fn publish_options_form_parses_into_node_config_preconditions() {
    use crate::pubsub::{AccessModel, NodeConfig, SendLastPublishedItem};

    let form = publish_options_form(
        PUBSUB_PUBLISH_OPTIONS_FORM_TYPE,
        &[
            ("pubsub#persist_items", "true"),
            ("pubsub#access_model", "whitelist"),
            ("pubsub#max_items", "max"),
            ("pubsub#send_last_published_item", "never"),
        ],
    );
    let patch = super::parse_publish_options_form(&form).expect("valid publish-options");
    assert_eq!(patch.access_model, Some(AccessModel::Whitelist));
    assert_eq!(patch.persist_items, Some(true));
    assert_eq!(patch.max_items, Some(u32::MAX));
    assert_eq!(
        patch.send_last_published_item,
        Some(SendLastPublishedItem::Never)
    );

    // `max` is met by the largest count the node allows, not by `1`.
    let mds = NodeConfig::mds_displayed();
    assert!(patch.met_by("urn:xmpp:mds:displayed:0", &mds));
    let bookmarks = NodeConfig::pep_for_node("urn:xmpp:bookmarks:1");
    assert!(patch.met_by("urn:xmpp:bookmarks:1", &bookmarks));
    assert!(!patch.met_by("urn:example:node", &NodeConfig::pep_default()));
}

#[test]
fn publish_options_form_rejects_unknown_field_and_wrong_form_type() {
    let unknown = publish_options_form(
        PUBSUB_PUBLISH_OPTIONS_FORM_TYPE,
        &[("pubsub#no_such_option", "x")],
    );
    assert!(super::parse_publish_options_form(&unknown).is_err());

    let wrong = publish_options_form(
        "http://jabber.org/protocol/pubsub#node_config",
        &[("pubsub#access_model", "whitelist")],
    );
    assert!(super::parse_publish_options_form(&wrong).is_err());
}

#[test]
fn publish_without_a_data_form_in_publish_options_is_bad_request() {
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "n")
        .append(Element::builder("item", NS_PUBSUB).build())
        .build();
    let options = Element::builder("publish-options", NS_PUBSUB).build();
    let iq = iq_with_payload(
        "p",
        None,
        None,
        TestIqPayload::Set(pubsub_payload(NS_PUBSUB, [publish, options])),
    );
    assert!(parse_pubsub_iq(&iq).is_err());
}

#[test]
fn precondition_not_met_is_conflict_with_the_pubsub_error_condition() {
    let iq = Iq::Set {
        from: Some("alice@example.test/a".parse().expect("jid")),
        to: None,
        id: "p".to_string(),
        payload: Element::builder("pubsub", NS_PUBSUB).build(),
    };
    let Iq::Error { error, .. } = build_pubsub_error(&iq, PubSubError::PreconditionNotMet) else {
        panic!("expected an error IQ");
    };
    assert_eq!(
        error.defined_condition,
        xmpp_parsers::stanza_error::DefinedCondition::Conflict
    );
    let other = error.other.expect("pubsub-specific condition");
    assert!(other.is("precondition-not-met", NS_PUBSUB_ERRORS));
}

fn roster_groups_field(groups: &[&str]) -> Element {
    let mut field = Element::builder("field", NS_XDATA)
        .attr(
            minidom::rxml::xml_ncname!("var").to_owned(),
            "pubsub#roster_groups_allowed",
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "list-multi");
    for group in groups {
        field = field.append(Element::builder("value", NS_XDATA).append(*group).build());
    }
    field.build()
}

fn configure_set_with(field: Element) -> Iq {
    let configure = Element::builder("configure", NS_PUBSUB_OWNER)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "n")
        .append(
            Element::builder("x", NS_XDATA)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                .append(field)
                .build(),
        )
        .build();
    iq_with_payload(
        "cfg-groups",
        Some("owner@example.com"),
        None,
        TestIqPayload::Set(pubsub_payload(NS_PUBSUB_OWNER, [configure])),
    )
}

/// XEP-0060 §16.4.4: `pubsub#roster_groups_allowed` is a list-multi field,
/// one `<value/>` per group.
#[test]
fn parse_configure_set_reads_every_roster_group_value() {
    let iq = configure_set_with(roster_groups_field(&["Friends", "Family", "Friends"]));
    match parse_pubsub_iq(&iq).expect("should parse") {
        PubSubRequest::ConfigureNodeSet { config, .. } => {
            assert_eq!(
                config.roster_groups_allowed,
                Some(vec!["Friends".to_owned(), "Family".to_owned()]),
                "every value is kept, duplicates collapse"
            );
        }
        other => panic!("Expected configure-set request, got {other:?}"),
    }
    // A field with no value clears the list.
    match parse_pubsub_iq(&configure_set_with(roster_groups_field(&[]))).expect("should parse") {
        PubSubRequest::ConfigureNodeSet { config, .. } => {
            assert_eq!(config.roster_groups_allowed, Some(Vec::new()));
        }
        other => panic!("Expected configure-set request, got {other:?}"),
    }
}

#[test]
fn configure_form_lists_roster_groups_for_the_roster_model() {
    let iq = iq_with_payload(
        "cfg-get",
        Some("owner@example.com"),
        None,
        TestIqPayload::Get(Element::builder("query", "jabber:iq:version").build()),
    );
    let mut config = crate::pubsub::NodeConfig::pep_default();
    config.access_model = crate::pubsub::AccessModel::Roster;
    config.roster_groups_allowed = vec!["Friends".to_owned(), "Family".to_owned()];
    let Iq::Result {
        payload: Some(pubsub),
        ..
    } = build_pubsub_configure_form_result(&iq, "n", &config)
    else {
        panic!("expected result payload");
    };
    let form = pubsub
        .get_child("configure", NS_PUBSUB_OWNER)
        .and_then(|configure| configure.get_child("x", NS_XDATA))
        .expect("configure data form");
    let field = form
        .children()
        .find(|field| field.attr("var") == Some("pubsub#roster_groups_allowed"))
        .expect("roster_groups_allowed field");
    assert_eq!(field.attr("type"), Some("list-multi"));
    let values: Vec<String> = field.children().map(Element::text).collect();
    assert_eq!(values, ["Friends", "Family"]);

    // Not offered on a node whose model cannot use it.
    let open = crate::pubsub::NodeConfig::public();
    let Iq::Result {
        payload: Some(pubsub),
        ..
    } = build_pubsub_configure_form_result(&iq, "n", &open)
    else {
        panic!("expected result payload");
    };
    let form = pubsub
        .get_child("configure", NS_PUBSUB_OWNER)
        .and_then(|configure| configure.get_child("x", NS_XDATA))
        .expect("configure data form");
    assert!(form
        .children()
        .all(|field| field.attr("var") != Some("pubsub#roster_groups_allowed")));
}

/// XEP-0060 §8.1.3: unsupported access model is `not-acceptable` with the
/// pubsub-specific `<unsupported-access-model/>`.
#[test]
fn unsupported_access_model_error_shape() {
    let iq = iq_with_payload(
        "cfg-auth",
        Some("owner@example.com"),
        None,
        TestIqPayload::Set(pubsub_payload(NS_PUBSUB_OWNER, [])),
    );
    let Iq::Error { error, .. } = build_pubsub_error(&iq, PubSubError::UnsupportedAccessModel)
    else {
        panic!("expected error iq");
    };
    assert_eq!(
        error.defined_condition,
        xmpp_parsers::stanza_error::DefinedCondition::NotAcceptable
    );
    assert!(error
        .other
        .as_ref()
        .is_some_and(|other| other.is("unsupported-access-model", NS_PUBSUB_ERRORS)));
}

#[test]
fn publish_options_roster_groups_precondition_compares_the_group_set() {
    use crate::pubsub::NodeConfig;

    let form = Element::builder("x", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", NS_XDATA)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .append(
                    Element::builder("value", NS_XDATA)
                        .append(PUBSUB_PUBLISH_OPTIONS_FORM_TYPE)
                        .build(),
                )
                .build(),
        )
        .append(roster_groups_field(&["friends", "family"]))
        .build();
    let patch = super::parse_publish_options_form(&form).expect("valid publish-options");
    let mut config = NodeConfig::pep_default();
    assert!(!patch.met_by("urn:example:node", &config));
    config.roster_groups_allowed = vec!["family".to_owned(), "friends".to_owned()];
    assert!(patch.met_by("urn:example:node", &config));
    config.roster_groups_allowed.push("work".to_owned());
    assert!(!patch.met_by("urn:example:node", &config));
}

#[test]
fn purge_event_is_a_single_empty_purge_element() {
    let service: jid::Jid = "pubsub.example.test".parse().expect("jid");
    let subscriber: jid::Jid = "bob@example.test/a".parse().expect("jid");
    let message = super::build_pubsub_purge_event(&service, &subscriber, "n");
    let event = message.payloads.first().expect("event");
    assert!(event.is("event", NS_PUBSUB_EVENT));
    let purge = event.get_child("purge", NS_PUBSUB_EVENT).expect("purge");
    assert_eq!(purge.attr("node"), Some("n"));
    assert_eq!(purge.children().count(), 0);
}

#[test]
fn request_without_a_node_or_item_maps_to_the_pubsub_condition() {
    for (child, expected) in [
        (
            Element::builder("publish", NS_PUBSUB),
            PubSubError::NodeIdRequired,
        ),
        (
            Element::builder("create", NS_PUBSUB),
            PubSubError::InstantNodesUnsupported,
        ),
        (
            Element::builder("publish", NS_PUBSUB)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), "n"),
            PubSubError::ItemRequired,
        ),
    ] {
        let iq = iq_with_payload(
            "e",
            None,
            None,
            TestIqPayload::Set(pubsub_payload(NS_PUBSUB, [child.build()])),
        );
        assert_eq!(parse_pubsub_iq(&iq).expect_err("rejected"), expected);
    }
}
