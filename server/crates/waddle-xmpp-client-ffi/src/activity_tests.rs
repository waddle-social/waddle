//! XEP-0108 activity notifications carried by XEP-0163 PEP.

use std::sync::Mutex;

use jid::BareJid;
use minidom::rxml::xml_ncname;
use minidom::Element;
use url::Url;
use waddle_xmpp_client::pep::{
    build_activity_clear_element, build_activity_element_with_specific, NS_ACTIVITY,
    NS_PUBSUB_EVENT,
};
use waddle_xmpp_client::{
    AccessToken, ClientConfig, ClientResource, ConnectionConfig, OAuthBearerConfig,
    WebSocketConfig, XmppRuntime, NS_CLIENT,
};

use crate::convert::dispatch_event;
use crate::{WaddleActivity, WaddleClientEvent, WaddleEventListener};

const ACCOUNT: &str = "alice@waddle.test";
const PUBLISHER: &str = "bob@waddle.test";

#[derive(Default)]
struct ActivityListener(Mutex<Vec<(BareJid, Option<WaddleActivity>)>>);

impl WaddleEventListener for ActivityListener {
    fn on_event(&self, event: WaddleClientEvent) {
        if let WaddleClientEvent::ActivityChanged { jid, activity } = event {
            self.0.lock().unwrap().push((jid, activity));
        }
    }
}

fn received_activity(notification: &Element) -> Vec<(BareJid, Option<WaddleActivity>)> {
    let config = ClientConfig::try_new(
        ConnectionConfig::new("waddle.test".parse().unwrap()),
        WebSocketConfig::try_new(Url::parse("wss://waddle.test/ws").unwrap()).unwrap(),
        OAuthBearerConfig::try_new(
            ACCOUNT.parse().unwrap(),
            ClientResource::try_new("test").unwrap(),
            AccessToken::new("token"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut runtime = XmppRuntime::try_new(config).unwrap();
    let listener = ActivityListener::default();
    for event in runtime.handle_app_stanza(notification) {
        dispatch_event(event, ACCOUNT, &listener);
    }
    listener.0.into_inner().unwrap()
}

fn notification(from: &str, item: Element) -> Element {
    Element::builder("message", NS_CLIENT)
        .attr(xml_ncname!("from").to_owned(), from)
        .attr(xml_ncname!("type").to_owned(), "headline")
        .append(
            Element::builder("event", NS_PUBSUB_EVENT)
                .append(
                    Element::builder("items", NS_PUBSUB_EVENT)
                        .attr(xml_ncname!("node").to_owned(), NS_ACTIVITY)
                        .append(item)
                        .build(),
                )
                .build(),
        )
        .build()
}

fn publication(payload: Element) -> Element {
    Element::builder("item", NS_PUBSUB_EVENT)
        .attr(xml_ncname!("id").to_owned(), "current")
        .append(payload)
        .build()
}

fn call_activity() -> Element {
    build_activity_element_with_specific("talking", Some("on_the_phone"), Some("In a call"))
}

#[test]
fn peer_activity_publication_reaches_the_listener() {
    let events = received_activity(&notification(PUBLISHER, publication(call_activity())));
    let [(jid, Some(activity))] = events.as_slice() else {
        panic!("expected one published activity, got {events:?}");
    };
    assert_eq!(jid.as_str(), PUBLISHER);
    assert_eq!(activity.general, "talking");
    assert_eq!(activity.specific.as_deref(), Some("on_the_phone"));
    assert_eq!(activity.text.as_deref(), Some("In a call"));
}

#[test]
fn activity_from_a_bare_domain_pep_service_reaches_the_listener() {
    let publisher = "peer.example";
    let events = received_activity(&notification(publisher, publication(call_activity())));
    assert!(matches!(events.as_slice(), [(jid, Some(_))] if jid.as_str() == publisher));
}

#[test]
fn peer_activity_retraction_reaches_the_listener_without_activity() {
    let retract = Element::builder("retract", NS_PUBSUB_EVENT)
        .attr(xml_ncname!("id").to_owned(), "current")
        .build();
    let events = received_activity(&notification(PUBLISHER, retract));
    assert!(matches!(events.as_slice(), [(jid, None)] if jid.as_str() == PUBLISHER));
}

#[test]
fn empty_activity_publication_clears_the_peer_activity() {
    let events = received_activity(&notification(
        PUBLISHER,
        publication(build_activity_clear_element()),
    ));
    assert!(matches!(events.as_slice(), [(jid, None)] if jid.as_str() == PUBLISHER));
}

#[test]
fn own_activity_notifications_are_ignored() {
    assert!(received_activity(&notification(ACCOUNT, publication(call_activity()))).is_empty());
}

#[test]
fn activity_notifications_from_full_jids_are_ignored() {
    for from in ["bob@waddle.test/phone", "room@muc.waddle.test/bob"] {
        assert!(received_activity(&notification(from, publication(call_activity()))).is_empty());
    }
}

#[test]
fn activity_notifications_with_a_mismatched_publisher_are_ignored() {
    let item = Element::builder("item", NS_PUBSUB_EVENT)
        .attr(xml_ncname!("publisher").to_owned(), "mallory@waddle.test")
        .append(call_activity())
        .build();
    assert!(received_activity(&notification(PUBLISHER, item)).is_empty());
}

#[test]
fn a_publisher_resource_under_the_sending_account_is_accepted() {
    let item = Element::builder("item", NS_PUBSUB_EVENT)
        .attr(xml_ncname!("publisher").to_owned(), "bob@waddle.test/phone")
        .append(call_activity())
        .build();
    let events = received_activity(&notification(PUBLISHER, item));
    assert!(matches!(events.as_slice(), [(jid, Some(_))] if jid.as_str() == PUBLISHER));
}

#[test]
fn an_unreadable_publication_clears_the_peer_activity() {
    let wrong_namespace = Element::builder("activity", "urn:example:unexpected").build();
    let missing_general = Element::builder("activity", NS_ACTIVITY)
        .append(
            Element::builder("text", NS_ACTIVITY)
                .append("In a call")
                .build(),
        )
        .build();
    let unknown_general = build_activity_element_with_specific("flying", None, None);
    let no_payload = Element::builder("item", NS_PUBSUB_EVENT).build();
    for item in [
        publication(wrong_namespace),
        publication(missing_general),
        publication(unknown_general),
        no_payload,
    ] {
        let stanza = notification(PUBLISHER, item);
        let events = received_activity(&stanza);
        assert!(
            matches!(events.as_slice(), [(jid, None)] if jid.as_str() == PUBLISHER),
            "{stanza:?}"
        );
    }
}
