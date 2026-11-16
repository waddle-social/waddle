//! XEP-0060 (1.30.0) event notifications from remote services to local
//! accounts across S2S (ADR-0020 D7; gate G18): a notification reaches the
//! account only for a node it subscribed to (recorded when its
//! `<subscribe/>` leaves, forgotten when its `<unsubscribe/>` does), or from
//! the personal eventing service (XEP-0163) of a remote account whose
//! presence it is subscribed to. Anything else carrying an event is dropped
//! unanswered. Through the production `/ws` router, the real S2S router and
//! a loopback peer serving `home-b.test` and `pubsub.home-b.test`.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use minidom::Element;
use support::{contact, federated_with, jid, receive, send, Adjust, Federated, Pki, BOB, PEER};
use waddle_server::db::s2s_accounts::RemotePubSubSubscriptions;
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::pubsub::{NS_PUBSUB, NS_PUBSUB_EVENT};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Message, MessageType};

const ALICE: &str = "alice@example.com";
const SERVICE: &str = "pubsub.home-b.test";
const NODE: &str = "princely_musings";

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

/// An event notification about `node` from `from` to `to`.
fn event(from: &str, to: &str, node: &str, id: &str) -> Message {
    let mut message = Message::new_with_type(MessageType::Headline, Some(jid(to)));
    message.from = Some(jid(from));
    message.id = Some(Id(id.to_owned()));
    message.payloads.push(
        Element::builder("event", NS_PUBSUB_EVENT)
            .append(
                Element::builder("items", NS_PUBSUB_EVENT)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                    .append(
                        Element::builder("item", NS_PUBSUB_EVENT)
                            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "item-1")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    message
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

#[tokio::test]
async fn a_notification_for_a_node_never_subscribed_to_is_dropped() {
    let pki = Pki::new();
    let federated = with_service(&pki).await;
    let mut phone = online_alice(&federated).await;

    federated
        .from_peer_service(SERVICE, event(SERVICE, ALICE, NODE, "ev-2"))
        .await;

    assert!(!support::arrives_quietly(&mut phone, "message", "ev-2").await);
    assert!(federated.peer_received_after_quiet("ev-2").await.is_empty());
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
    assert!(!support::arrives_quietly(&mut phone, "message", "ev-3").await);
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
    assert!(!support::arrives_quietly(&mut phone, "message", "pep-2").await);
    let _ = PEER;
}
