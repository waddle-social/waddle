//! XEP-0060 (1.30.0) event notifications from remote services to local
//! accounts across S2S (ADR-0020 D7; gate G18): a notification reaches the
//! account only for a node it subscribed to (recorded when its
//! `<subscribe/>` leaves, forgotten when its `<unsubscribe/>` does), or from
//! the personal eventing service (XEP-0163) of a remote account whose
//! presence it is subscribed to. A notification is admitted only when every
//! node it names is, and the gate applies before anything else, a message
//! of type `error` included. Anything else carrying an event is dropped
//! unanswered. A notification that is delivered loses, at the top and
//! inside its items, what a client would read as its own server's
//! statement: a XEP-0511 link card, a link-preview request, inbox and pin
//! payloads, and a stanza id in the account's name; at the top, and in a
//! message an item forwards, every payload in a namespace only this
//! deployment defines, which an item's own payload may still use. Through
//! the production
//! `/ws` router, the real S2S router and a loopback peer serving
//! `home-b.test` and `pubsub.home-b.test`.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use minidom::Element;
use support::{
    contact, federated_with, holds, jid, receive, send, Adjust, Federated, Pki, BOB, PEER,
};
use waddle_server::db::s2s_accounts::RemotePubSubSubscriptions;
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::pubsub::{NS_PUBSUB, NS_PUBSUB_EVENT};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Message, MessageType};

const ALICE: &str = "alice@example.com";
const SERVICE: &str = "pubsub.home-b.test";
const NODE: &str = "princely_musings";
/// RFC 4287, the payload of the XEP-0060 examples' items.
const ATOM: &str = "http://www.w3.org/2005/Atom";

async fn with_service(pki: &Pki) -> Federated {
    federated_with(
        pki,
        Adjust {
            peer_services: &[SERVICE],
            ..Adjust::default()
        },
    )
    .await
}

/// The `<items/>` of an event notification about `node`.
fn items(node: &str) -> Element {
    Element::builder("items", NS_PUBSUB_EVENT)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .append(
            Element::builder("item", NS_PUBSUB_EVENT)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "item-1")
                .build(),
        )
        .build()
}

/// An event notification about each of `nodes` from `from` to `to`.
fn event_about(from: &str, to: &str, nodes: &[&str], id: &str) -> Message {
    let mut message = Message::new_with_type(MessageType::Headline, Some(jid(to)));
    message.from = Some(jid(from));
    message.id = Some(Id(id.to_owned()));
    message.payloads.push(
        Element::builder("event", NS_PUBSUB_EVENT)
            .append_all(nodes.iter().map(|node| items(node)))
            .build(),
    );
    message
}

/// An event notification about `node` from `from` to `to`.
fn event(from: &str, to: &str, node: &str, id: &str) -> Message {
    event_about(from, to, &[node], id)
}

/// A `<subscribe/>` or `<unsubscribe/>` request for `node` of the service.
fn subscription(id: &str, element: &str) -> Iq {
    Iq::Set {
        from: None,
        to: Some(jid(SERVICE)),
        id: id.to_owned(),
        payload: Element::builder("pubsub", NS_PUBSUB)
            .append(
                Element::builder(element, NS_PUBSUB)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), NODE)
                    .attr(minidom::rxml::xml_ncname!("jid").to_owned(), ALICE)
                    .build(),
            )
            .build(),
    }
}

/// Wait until the account's record of `node` is `held`.
async fn recorded(federated: &Federated, held: bool) {
    let store = RemotePubSubSubscriptions::new(
        federated
            .loopback
            .state
            .deps
            .app_state
            .db_pool
            .global()
            .clone(),
    );
    let account: jid::BareJid = ALICE.parse().expect("a JID");
    let service: jid::BareJid = SERVICE.parse().expect("a JID");
    support::routing::wait_for_async(support::ARRIVES, || {
        let store = store.clone();
        let account = account.clone();
        let service = service.clone();
        async move {
            (store.holds(&account, &service, NODE).await.ok() == Some(held)).then_some(())
        }
    })
    .await
    .expect("the subscription record changes");
}

async fn online_alice(federated: &Federated) -> WsXmppClient {
    federated.account("alice").await;
    federated.online("alice", "phone", 5).await
}

#[tokio::test]
async fn a_notification_for_a_node_the_account_subscribed_to_is_delivered() {
    let pki = Pki::new();
    let federated = with_service(&pki).await;
    let mut phone = online_alice(&federated).await;

    send(&mut phone, subscription("sub-1", "subscribe")).await;
    federated.peer_receives("sub-1").await;
    recorded(&federated, true).await;

    federated
        .from_peer_service(SERVICE, event(SERVICE, ALICE, NODE, "ev-1"))
        .await;
    receive(&mut phone, "message", "ev-1").await;
}

/// The namespace of a Waddle status preference, an item payload a remote
/// Waddle account's personal eventing service publishes.
const NS_STATUS_PREFERENCE: &str = "urn:waddle:status-preference:0";

/// Payloads in namespaces only this deployment defines that no other rule
/// names: a call thread, a moderation score and an extension's own.
fn waddle_only_payloads() -> Vec<Element> {
    vec![
        Element::builder(
            "call-thread",
            waddle_xmpp::xep::xep_waddle_call_thread::NS_WADDLE_CALL_THREAD,
        )
        .build(),
        Element::builder("safety-scores", "urn:waddle:safety-scores:1").build(),
        Element::builder("result", "urn:waddle:extension:1:example").build(),
    ]
}

/// An event notification about `node` whose item carries an Atom entry, a
/// status preference and a forwarded message, and, beside and inside them,
/// what only the recipient's server states.
fn event_carrying_forgeries(id: &str) -> Message {
    let url = url::Url::parse("https://video.example.org/watch").expect("a URL");
    let card = || {
        waddle_xmpp::xep::build_link_metadata_element(
            &waddle_xmpp::xep::LinkMetadata::new(url.clone()).with_title("forged"),
        )
    };
    let forged_id = Element::builder("stanza-id", waddle_xmpp_core::xep0359::NS_SID)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "forged")
        .attr(minidom::rxml::xml_ncname!("by").to_owned(), ALICE)
        .build();
    let entry = Element::builder("entry", ATOM)
        .append(Element::builder("title", ATOM).append("Princely musings"))
        .append(card())
        .append(forged_id.clone())
        .build();
    // XEP-0297: a message the item forwards, read as a message in its own
    // right.
    let mut forwarded = Message::new_with_type(MessageType::Chat, Some(jid(ALICE)));
    forwarded.from = Some(jid("carol@home-b.test/desk"));
    forwarded.payloads.extend(waddle_only_payloads());
    let item = Element::builder("item", NS_PUBSUB_EVENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "item-1")
        .append(entry)
        .append(Element::builder("status", NS_STATUS_PREFERENCE).build())
        .append(
            Element::builder("forwarded", xmpp_parsers::ns::FORWARD)
                .append(Element::from(forwarded))
                .build(),
        )
        .append(card())
        .append(waddle_xmpp::xep::build_link_preview_request_element(
            &waddle_xmpp::xep::LinkPreviewToken::try_new("a-forged-token").expect("a token"),
        ))
        .append(Element::builder("entry", waddle_xmpp::xep::NS_INBOX).build())
        .append(
            Element::builder("pinned", waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0).build(),
        )
        .build();
    let mut message = Message::new_with_type(MessageType::Headline, Some(jid(ALICE)));
    message.from = Some(jid(SERVICE));
    message.id = Some(Id(id.to_owned()));
    message.payloads.push(
        Element::builder("event", NS_PUBSUB_EVENT)
            .append(
                Element::builder("items", NS_PUBSUB_EVENT)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), NODE)
                    .append(item)
                    .build(),
            )
            .build(),
    );
    message.payloads.push(card());
    message.payloads.push(forged_id);
    message.payloads.extend(waddle_only_payloads());
    message
}

/// Every element below `element`, at any depth, that only the recipient's
/// server states, of those [`event_carrying_forgeries`] forges: anywhere,
/// the link card and request, inbox and pin payloads and the forged stanza
/// id; directly inside a message, the delivered one or one an item
/// forwards, any payload in a namespace only this deployment defines.
fn forged_below(element: &Element) -> Vec<String> {
    let message = element.name() == "message";
    element
        .children()
        .flat_map(|child| {
            let mut found = forged_below(child);
            let ns = child.ns();
            if waddle_xmpp::xep::is_link_metadata_element(child)
                || waddle_xmpp::xep::is_link_preview_request_element(child)
                || ns == waddle_xmpp::xep::NS_INBOX
                || ns == waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0
                || (message && ns.starts_with("urn:waddle:"))
                || (child.is("stanza-id", waddle_xmpp_core::xep0359::NS_SID)
                    && child.attr("id") == Some("forged"))
            {
                found.push([ns.as_str(), " ", child.name()].concat());
            }
            found
        })
        .collect()
}

#[tokio::test]
async fn a_delivered_notification_loses_what_only_the_recipients_server_states() {
    let pki = Pki::new();
    let federated = with_service(&pki).await;
    let mut phone = online_alice(&federated).await;

    send(&mut phone, subscription("sub-6", "subscribe")).await;
    federated.peer_receives("sub-6").await;
    recorded(&federated, true).await;

    federated
        .from_peer_service(SERVICE, event_carrying_forgeries("ev-6"))
        .await;
    let delivered = receive(&mut phone, "message", "ev-6").await;
    assert!(forged_below(&delivered).is_empty(), "{delivered:?}");
    // The item keeps its entry, its own status preference payload and the
    // message it forwards.
    let item = delivered
        .get_child("event", NS_PUBSUB_EVENT)
        .and_then(|event| event.get_child("items", NS_PUBSUB_EVENT))
        .and_then(|items| items.get_child("item", NS_PUBSUB_EVENT))
        .expect("the item is delivered");
    let entry = item
        .get_child("entry", ATOM)
        .expect("the item's entry is delivered");
    assert!(entry.get_child("title", ATOM).is_some(), "{entry:?}");
    assert!(
        item.get_child("status", NS_STATUS_PREFERENCE).is_some(),
        "{item:?}"
    );
    assert!(
        item.get_child("forwarded", xmpp_parsers::ns::FORWARD)
            .is_some_and(|forwarded| forwarded.children().any(|child| child.name() == "message")),
        "{item:?}"
    );
}

#[tokio::test]
async fn a_notification_for_a_node_never_subscribed_to_is_dropped() {
    let pki = Pki::new();
    let federated = with_service(&pki).await;
    let mut phone = online_alice(&federated).await;

    let mark = federated.peer_mark();
    federated
        .from_peer_service(SERVICE, event(SERVICE, ALICE, NODE, "ev-2"))
        .await;

    let barrier = federated.peer_barrier(SERVICE, ALICE).await;
    assert!(!holds(
        &federated.client_settle(&mut phone).await,
        "message",
        "ev-2"
    ));
    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
}

#[tokio::test]
async fn a_notification_after_unsubscribing_is_dropped() {
    let pki = Pki::new();
    let federated = with_service(&pki).await;
    let mut phone = online_alice(&federated).await;

    send(&mut phone, subscription("sub-3", "subscribe")).await;
    recorded(&federated, true).await;
    send(&mut phone, subscription("unsub-3", "unsubscribe")).await;
    federated.peer_receives("unsub-3").await;
    recorded(&federated, false).await;

    federated
        .from_peer_service(SERVICE, event(SERVICE, ALICE, NODE, "ev-3"))
        .await;
    federated.peer_barrier(SERVICE, ALICE).await;
    assert!(!holds(
        &federated.client_settle(&mut phone).await,
        "message",
        "ev-3"
    ));
}

#[tokio::test]
async fn an_error_carrying_an_event_is_dropped_even_for_a_subscribed_node() {
    let pki = Pki::new();
    let federated = with_service(&pki).await;
    let mut phone = online_alice(&federated).await;
    send(&mut phone, subscription("sub-4", "subscribe")).await;
    recorded(&federated, true).await;

    // An error to a bound full JID is otherwise delivered; one carrying an
    // event is no notification the account asked for (§7.1).
    let mut error = event(SERVICE, "alice@example.com/phone", NODE, "ev-4");
    error.type_ = MessageType::Error;
    federated.from_peer_service(SERVICE, error).await;
    federated.peer_barrier(SERVICE, ALICE).await;
    assert!(!holds(
        &federated.client_settle(&mut phone).await,
        "message",
        "ev-4"
    ));
}

#[tokio::test]
async fn a_notification_naming_any_node_not_subscribed_to_is_dropped_whole() {
    let pki = Pki::new();
    let federated = with_service(&pki).await;
    let mut phone = online_alice(&federated).await;
    send(&mut phone, subscription("sub-5", "subscribe")).await;
    recorded(&federated, true).await;

    federated
        .from_peer_service(
            SERVICE,
            event_about(SERVICE, ALICE, &[NODE, "other_node"], "ev-5"),
        )
        .await;
    federated
        .from_peer_service(SERVICE, event(SERVICE, ALICE, NODE, "ev-5-control"))
        .await;
    // The control, decided after it, arrives; the mixed one never did.
    let frames = support::receive_until(&mut phone, |frame| {
        frame.name() == "message" && frame.attr("id") == Some("ev-5-control")
    })
    .await;
    assert!(!holds(&frames, "message", "ev-5"), "{frames:?}");
}

#[tokio::test]
async fn a_personal_eventing_notification_reaches_an_account_subscribed_to_its_owner() {
    let pki = Pki::new();
    let federated = with_service(&pki).await;
    let mut phone = online_alice(&federated).await;
    contact(&federated, ALICE, BOB, "to").await;

    federated
        .from_peer(event(BOB, ALICE, "urn:xmpp:avatar:metadata", "pep-1"))
        .await;
    receive(&mut phone, "message", "pep-1").await;

    // Mallory's personal eventing service is not one Alice subscribed to.
    federated
        .from_peer(event(
            "mallory@home-b.test",
            ALICE,
            "urn:xmpp:avatar:metadata",
            "pep-2",
        ))
        .await;
    federated.peer_barrier("mallory@home-b.test", ALICE).await;
    assert!(!holds(
        &federated.client_settle(&mut phone).await,
        "message",
        "pep-2"
    ));
    let _ = PEER;
}
