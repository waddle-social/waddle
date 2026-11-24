//! XEP-0357 (0.4.1) Push Services of other servers, such as the app server
//! of a third-party client (§2): an account enables the service with its JID
//! and node and optional publish options (§4); the server publishes the
//! notification to it over S2S (§5); and a publish answered with an IQ error
//! disables that JID and node (§5.1). Through the production `/ws` router,
//! the real S2S router and a loopback peer serving `home-b.test` and
//! `push.home-b.test`.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use std::time::Duration;

use minidom::Element;
use support::{federated_with, message, receive, send, Adjust, Federated, Pki, BOB_PHONE};
use waddle_xmpp::push::PushSubscription;
use waddle_xmpp::xep::xep0357::NS_PUSH;
use xmpp_parsers::message::MessageType;

const ALICE: &str = "alice@example.com";
const SERVICE: &str = "push.home-b.test";
const NODE: &str = "yxs32uqsflafdk3iuqo";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_DATA: &str = "jabber:x:data";
const NS_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
const PUBLISH_OPTIONS_FORM: &str = "http://jabber.org/protocol/pubsub#publish-options";

async fn deployment() -> Federated {
    let pki = Pki::new();
    let federated = federated_with(
        &pki,
        Adjust {
            peer_services: &[SERVICE],
            ..Adjust::default()
        },
    )
    .await;
    waddle_server::test_support::host_remote_push_services(
        &federated.loopback.state,
        &federated.router,
    );
    federated
}

fn form(secret: &str) -> Element {
    let field = |var: &str, value: &str| {
        Element::builder("field", NS_DATA)
            .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
            .append(Element::builder("value", NS_DATA).append(value).build())
            .build()
    };
    Element::builder("x", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(field("FORM_TYPE", PUBLISH_OPTIONS_FORM))
        .append(field("secret", secret))
        .build()
}

fn enable(id: &str, service: &str, node: Option<&str>, options: Option<Element>) -> Element {
    let mut enable = Element::builder("enable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), service);
    if let Some(node) = node {
        enable = enable.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    if let Some(options) = options {
        enable = enable.append(options);
    }
    Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(enable.build())
        .build()
}

fn disable(id: &str, service: &str, node: Option<&str>) -> Element {
    let mut disable = Element::builder("disable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), service);
    if let Some(node) = node {
        disable = disable.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(disable.build())
        .build()
}

/// Whether `element` or anything below it has the text `text`.
fn has_text(element: &Element, text: &str) -> bool {
    element.text() == text || element.children().any(|child| has_text(child, text))
}

fn error_condition(stanza: &Element) -> Option<String> {
    stanza
        .get_child("error", "jabber:client")
        .and_then(|error| error.children().find(|child| child.ns() == NS_STANZAS))
        .map(|condition| condition.name().to_owned())
}

async fn registrations(federated: &Federated) -> Vec<PushSubscription> {
    federated
        .loopback
        .state
        .deps
        .protocol
        .push_store
        .get_for_user(ALICE)
        .await
        .expect("the registrations are readable")
}

async fn register(federated: &Federated, node: &str, options: Option<Element>) {
    federated
        .loopback
        .state
        .deps
        .protocol
        .push_store
        .register(PushSubscription {
            user_jid: ALICE.to_owned(),
            service_jid: SERVICE.to_owned(),
            node: Some(node.to_owned()),
            publish_options: options,
            endpoint: None,
            p256dh: None,
            auth_key: None,
        })
        .await
        .expect("the registration is stored");
}

/// Alice has no resource; a message from the peer raises a notification,
/// and the publish it causes is the stanza the peer receives for `id`.
async fn publish_for(federated: &Federated, id: &str) -> Element {
    federated
        .peer_sends(message(BOB_PHONE, ALICE, MessageType::Chat, id, "hello"))
        .await;
    federated
        .peer_receives_matching(|stanza| {
            stanza.name() == "iq"
                && stanza.attr("type") == Some("set")
                && stanza.get_child("pubsub", NS_PUBSUB).is_some()
        })
        .await
}

fn answer(publish: &Element, type_: &str, error: Option<(&str, &str)>) -> Element {
    let mut iq = Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), type_)
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), SERVICE)
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            publish.attr("from").expect("the publish names its sender"),
        )
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            publish.attr("id").expect("the publish has an id"),
        );
    if let Some((error_type, condition)) = error {
        iq = iq.append(
            Element::builder("error", "jabber:client")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), error_type)
                .append(Element::builder(condition, NS_STANZAS).build())
                .build(),
        );
    }
    iq.build()
}

#[tokio::test]
async fn enable_accepts_the_push_service_of_another_server() {
    let federated = deployment().await;
    federated.account("alice").await;
    let mut alice = federated.online("alice", "desk", 0).await;

    // §4: a 'jid', a 'node' and a publish-options form.
    send(
        &mut alice,
        enable("e1", SERVICE, Some(NODE), Some(form("one"))),
    )
    .await;
    let result = receive(&mut alice, "iq", "e1").await;
    assert_eq!(result.attr("type"), Some("result"), "{result:?}");
    let held = registrations(&federated).await;
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0].service_jid, SERVICE);
    assert_eq!(held[0].node.as_deref(), Some(NODE));
    assert!(held[0].publish_options.is_some());

    // "If the combination of JID and node has already been enabled, then
    // the server SHOULD use the last received request for any publish
    // options."
    send(
        &mut alice,
        enable("e2", SERVICE, Some(NODE), Some(form("two"))),
    )
    .await;
    assert_eq!(
        receive(&mut alice, "iq", "e2").await.attr("type"),
        Some("result")
    );
    let held = registrations(&federated).await;
    assert_eq!(held.len(), 1, "{held:?}");
    let options = held[0].publish_options.as_ref().expect("publish options");
    assert!(has_text(options, "two"), "{options:?}");

    // "The JID for a Push Service MAY be enabled multiple times for a user
    // only if different node values are provided."
    send(&mut alice, enable("e3", SERVICE, Some("other"), None)).await;
    assert_eq!(
        receive(&mut alice, "iq", "e3").await.attr("type"),
        Some("result")
    );
    assert_eq!(registrations(&federated).await.len(), 2);

    // §3: a 'node' removes that combination only; without one, every entry
    // of the service.
    send(&mut alice, disable("d1", SERVICE, Some(NODE))).await;
    assert_eq!(
        receive(&mut alice, "iq", "d1").await.attr("type"),
        Some("result")
    );
    let held = registrations(&federated).await;
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0].node.as_deref(), Some("other"));
    send(&mut alice, disable("d2", SERVICE, None)).await;
    assert_eq!(
        receive(&mut alice, "iq", "d2").await.attr("type"),
        Some("result")
    );
    assert!(registrations(&federated).await.is_empty());
}

#[tokio::test]
async fn enable_refuses_what_cannot_be_published_to() {
    let federated = deployment().await;
    federated.account("alice").await;
    let mut alice = federated.online("alice", "desk", 0).await;

    // A publish needs a node (XEP-0060 §7.1.1).
    send(&mut alice, enable("n1", SERVICE, None, None)).await;
    let refused = receive(&mut alice, "iq", "n1").await;
    assert_eq!(refused.attr("type"), Some("error"));
    assert_eq!(error_condition(&refused).as_deref(), Some("bad-request"));

    // A service of this deployment that is not its Push Service is not one
    // to publish to over S2S.
    send(
        &mut alice,
        enable("l1", "muc.example.com", Some(NODE), None),
    )
    .await;
    let refused = receive(&mut alice, "iq", "l1").await;
    assert_eq!(refused.attr("type"), Some("error"));
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("service-unavailable")
    );
    assert!(registrations(&federated).await.is_empty());
}

#[tokio::test]
async fn a_notification_is_published_to_the_remote_service_with_its_publish_options() {
    let federated = deployment().await;
    federated.account("alice").await;
    register(&federated, NODE, Some(form("eruio234vzxc2kla-91"))).await;
    let loops = waddle_server::test_support::spawn_notification_loops(
        &federated.loopback.state,
        Duration::from_millis(100),
    );

    let publish = publish_for(&federated, "m1").await;
    for task in loops {
        task.abort();
    }

    // §5: from the server, to the service, a publish of the node whose item
    // is a <notification/> in 'urn:xmpp:push:0'.
    assert_eq!(publish.attr("to"), Some(SERVICE));
    assert_eq!(publish.attr("from"), Some(support::LOCAL));
    let pubsub = publish.get_child("pubsub", NS_PUBSUB).expect("pubsub");
    let node = pubsub.get_child("publish", NS_PUBSUB).expect("publish");
    assert_eq!(node.attr("node"), Some(NODE));
    let item = node.get_child("item", NS_PUBSUB).expect("item");
    let notification = item
        .get_child("notification", NS_PUSH)
        .expect("notification");
    let summary = notification.get_child("x", NS_DATA).expect("summary form");
    assert!(summary.children().any(|field| {
        field.attr("var") == Some("FORM_TYPE")
            && field
                .children()
                .any(|value| value.text() == "urn:xmpp:push:summary")
    }));
    // "If additional data was provided when enabling the service, the
    // publish request SHOULD include the data as publish options."
    let options = pubsub
        .get_child("publish-options", NS_PUBSUB)
        .expect("publish-options")
        .get_child("x", NS_DATA)
        .expect("options form");
    assert!(has_text(options, "eruio234vzxc2kla-91"));
    // An answer of result keeps the registration.
    federated
        .peer_service_sends(SERVICE, answer(&publish, "result", None))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(registrations(&federated).await.len(), 1);
}

#[tokio::test]
async fn a_publish_answered_with_an_iq_error_disables_the_registration() {
    let federated = deployment().await;
    federated.account("alice").await;
    register(&federated, NODE, None).await;
    register(&federated, "kept", None).await;
    let loops = waddle_server::test_support::spawn_notification_loops(
        &federated.loopback.state,
        Duration::from_millis(100),
    );

    federated
        .peer_sends(message(BOB_PHONE, ALICE, MessageType::Chat, "m2", "hello"))
        .await;
    let first = federated
        .peer_receives_matching(|stanza| stanza.get_child("pubsub", NS_PUBSUB).is_some())
        .await;
    let node = |stanza: &Element| {
        stanza
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
            .and_then(|publish| publish.attr("node").map(str::to_owned))
    };
    let refused = node(&first).expect("a node");
    // "If a publish request is returned with an IQ-error, then the server
    // SHOULD consider the particular JID and node combination to be
    // disabled."
    federated
        .peer_service_sends(
            SERVICE,
            answer(&first, "error", Some(("cancel", "item-not-found"))),
        )
        .await;
    let remaining = loop {
        let held = registrations(&federated).await;
        if held.len() == 1 {
            break held;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_ne!(remaining[0].node.as_deref(), Some(refused.as_str()));
    for task in loops {
        task.abort();
    }
}

#[tokio::test]
async fn a_transient_iq_error_keeps_the_registration_enabled() {
    let federated = deployment().await;
    federated.account("alice").await;
    register(&federated, NODE, None).await;
    let loops = waddle_server::test_support::spawn_notification_loops(
        &federated.loopback.state,
        Duration::from_millis(100),
    );

    let publish = publish_for(&federated, "m3").await;
    // "A server MAY choose to keep a service enabled if the error is deemed
    // recoverable or transient": an error of type 'wait'.
    federated
        .peer_service_sends(
            SERVICE,
            answer(&publish, "error", Some(("wait", "resource-constraint"))),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    for task in loops {
        task.abort();
    }
    assert_eq!(registrations(&federated).await.len(), 1);
}
