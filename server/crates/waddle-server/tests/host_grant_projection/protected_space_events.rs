use std::time::Duration;

use jid::BareJid;
use minidom::{rxml::xml_ncname, Element};
use waddle_ws_test_support::{
    iq::{round_trip, send_iq},
    stanza::{is_result, pubsub_publish, pubsub_retract, pubsub_subscribe, result_payload, set},
    WsXmppClient,
};
use waddle_xmpp::pubsub::{NS_PUBSUB, NS_PUBSUB_EVENT};
use waddle_xmpp::xep::xep0402::{build_bookmark_element, Bookmark};
use xmpp_parsers::iq::Iq;

use super::{host_grant_revocation_support::Replica, PEER};

const ARRIVES: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(600);
const NS_BOOKMARK: &str = "urn:xmpp:bookmarks:1";

fn event_for(stanza: &Element, service: &BareJid, node: &str) -> bool {
    stanza.is("message", "jabber:client")
        && stanza.attr("from") == Some(service.as_str())
        && stanza
            .get_child("event", NS_PUBSUB_EVENT)
            .and_then(|event| event.get_child("items", NS_PUBSUB_EVENT))
            .is_some_and(|items| items.attr("node") == Some(node))
}

async fn receive_event(
    client: &mut WsXmppClient,
    service: &BareJid,
    node: &str,
    wait: Duration,
) -> Option<Element> {
    client
        .recv_matching_within(wait, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|stanza| event_for(&stanza, service, node))
        })
        .await
        .ok()
        .map(|frame| frame.parse().expect("event XML"))
}

pub(super) async fn assert_local_bookmark_events(
    replica: &Replica,
    owner: &mut WsXmppClient,
    service: &BareJid,
    node: &str,
    room: &BareJid,
) {
    let mut subscriber = replica.client("alice", "event-listener").await;
    let recipient = subscriber.full_jid.clone().expect("bound subscriber");
    let publish = |id: &str, title: &str| {
        set(
            id,
            service.as_str(),
            pubsub_publish(
                node,
                room.as_str(),
                build_bookmark_element(&Bookmark::new(room.clone()).with_name(title)),
            ),
        )
    };
    let retract = |id: &str| set(id, service.as_str(), pubsub_retract(node, room.as_str()));

    let history = round_trip(owner, publish("space-event-before", "Before subscription")).await;
    assert!(is_result(&history), "{history:?}");
    assert!(
        receive_event(&mut subscriber, service, node, QUIET)
            .await
            .is_none(),
        "an authorized account without a preference received an event"
    );

    let subscription_request = set(
        "space-event-subscribe",
        service.as_str(),
        pubsub_subscribe(node, PEER),
    );
    send_iq(&mut subscriber, subscription_request).await;
    let next: Element = subscriber
        .recv_matching_within(ARRIVES, |frame| {
            frame.parse::<Element>().is_ok_and(|stanza| {
                event_for(&stanza, service, node)
                    || (stanza.name() == "iq" && stanza.attr("id") == Some("space-event-subscribe"))
            })
        })
        .await
        .expect("subscribe result or unexpected historical event")
        .parse()
        .expect("subscribe response XML");
    let subscribed = Iq::try_from(next).expect("subscription reply precedes any event");
    assert!(is_result(&subscribed), "{subscribed:?}");
    let subid = result_payload(&subscribed)
        .and_then(|pubsub| pubsub.get_child("subscription", NS_PUBSUB))
        .and_then(|subscription| subscription.attr("subid"))
        .expect("exact subscription ID");

    let published = round_trip(owner, publish("space-event-current", "Current publication")).await;
    assert!(is_result(&published), "{published:?}");
    let event = receive_event(&mut subscriber, service, node, ARRIVES)
        .await
        .expect("local publication event");
    assert_eq!(event.attr("to"), Some(recipient.as_str()));
    assert_eq!(event.attr("type"), Some("headline"));
    let items = event
        .get_child("event", NS_PUBSUB_EVENT)
        .and_then(|event| event.get_child("items", NS_PUBSUB_EVENT))
        .expect("event items");
    assert_eq!(items.children().count(), 1);
    let item = items
        .get_child("item", NS_PUBSUB_EVENT)
        .expect("published item");
    assert_eq!(item.attr("id"), Some(room.as_str()));
    assert_eq!(
        item.get_child("conference", NS_BOOKMARK)
            .and_then(|conference| conference.attr("name")),
        Some("Current publication")
    );

    let retracted = round_trip(owner, retract("space-event-retract")).await;
    assert!(is_result(&retracted), "{retracted:?}");
    let event = receive_event(&mut subscriber, service, node, ARRIVES)
        .await
        .expect("local retraction event");
    assert_eq!(event.attr("to"), Some(recipient.as_str()));
    let removed = event
        .get_child("event", NS_PUBSUB_EVENT)
        .and_then(|event| event.get_child("items", NS_PUBSUB_EVENT))
        .and_then(|items| items.get_child("retract", NS_PUBSUB_EVENT))
        .expect("retraction");
    assert_eq!(removed.attr("id"), Some(room.as_str()));

    let unsubscribe = Element::builder("unsubscribe", NS_PUBSUB)
        .attr(xml_ncname!("node").to_owned(), node)
        .attr(xml_ncname!("jid").to_owned(), PEER)
        .attr(xml_ncname!("subid").to_owned(), subid)
        .build();
    let removed = round_trip(
        &mut subscriber,
        set(
            "space-event-unsubscribe",
            service.as_str(),
            Element::builder("pubsub", NS_PUBSUB)
                .append(unsubscribe)
                .build(),
        ),
    )
    .await;
    assert!(is_result(&removed), "{removed:?}");
    let published = round_trip(owner, publish("space-event-after", "After unsubscribe")).await;
    assert!(is_result(&published), "{published:?}");
    assert!(
        receive_event(&mut subscriber, service, node, QUIET)
            .await
            .is_none(),
        "an unsubscribed local account received an event"
    );
    let cleaned = round_trip(owner, retract("space-event-cleanup")).await;
    assert!(is_result(&cleaned), "{cleaned:?}");
}
