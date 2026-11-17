//! XEP-0503 version 0.3.0 (xsf/xeps
//! 4132c531487fe9450ae90b905b8df950d0d478b2) across S2S, outbound: a local
//! account uses a Space hosted on another server's PubSub service (ADR-0020
//! D4, D7 and D10).
//!
//! - "Fetching spaces from a spaces service", "Fetching the metadata
//!   associated to a space" and "Discovering the content of a space" are
//!   plain XEP-0030 and XEP-0060 requests to the remote service: they
//!   leave with the account's full JID, and the service's answers reach
//!   the resource that asked.
//! - "Joining a space" is a XEP-0060 subscription. The event notifications
//!   of the Space then reach the account, and only those of a node it
//!   subscribed to.
//! - A subscription to a Space is not a grant in any of its rooms: it
//!   writes nothing here but the record that admits the Space's
//!   notifications, and entering a room the Space lists is a join of that
//!   room, verified and decided as any join of a remote room is.
//! - A Space's avatar and banner items (0.3.0, "Spaces images") are
//!   relayed as the service sent them; the server requests nothing they
//!   name.
//!
//! Through the production `/ws` router, the real S2S router and a loopback
//! peer serving `home-b.test`, its PubSub service `spaces.home-b.test` and
//! its MUC service `conference.home-b.test`. No PostgreSQL, SpiceDB or
//! second process is involved.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use minidom::Element;
use support::{
    federated_with, full_jid, holds, jid, receive, send, Adjust, Federated, Pki, LOCAL, QUIET,
};
use waddle_server::db::actor::DbQueryOne;
use waddle_server::db::s2s_accounts::RemotePubSubSubscriptions;
use waddle_server::db::Value;
use waddle_ws_test_support::WsXmppClient;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

const ALICE: &str = "alice@example.com";
/// The peer's Spaces PubSub service.
const SERVICE: &str = "spaces.home-b.test";
/// The peer's MUC service.
const ROOMS: &str = "conference.home-b.test";
/// A Space on [`SERVICE`].
const DEV: &str = "dev";
/// A room the Space lists.
const ROOM: &str = "editors@conference.home-b.test";

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NS_DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";
const NS_DATA: &str = "jabber:x:data";
const NS_BOOKMARKS: &str = "urn:xmpp:bookmarks:1";
const AVATAR_ITEM: &str = "urn:xmpp:spaces:avatar:metadata:0";
const AVATAR_URL: &str = "https://uploads.home-b.test/dev/avatar.jpg";

async fn with_space_host(pki: &Pki) -> Federated {
    federated_with(
        pki,
        Adjust {
            peer_services: &[SERVICE, ROOMS],
            ..Adjust::default()
        },
    )
    .await
}

async fn online_alice(federated: &Federated) -> WsXmppClient {
    federated.account("alice").await;
    federated.online("alice", "phone", 5).await
}

fn to_service(id: &str, set: bool, payload: Element) -> Iq {
    let (to, id) = (Some(jid(SERVICE)), id.to_owned());
    if set {
        Iq::Set {
            from: None,
            to,
            id,
            payload,
        }
    } else {
        Iq::Get {
            from: None,
            to,
            id,
            payload,
        }
    }
}

fn node(name: &str, namespace: &str, node: &str) -> Element {
    Element::builder(name, namespace)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .build()
}

fn pubsub(child: Element) -> Element {
    Element::builder("pubsub", NS_PUBSUB).append(child).build()
}

fn subscription(name: &str, space: &str) -> Element {
    pubsub(
        Element::builder(name, NS_PUBSUB)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), space)
            .attr(minidom::rxml::xml_ncname!("jid").to_owned(), ALICE)
            .build(),
    )
}

/// The service's result for the request `id` of `to`.
fn result(to: &jid::FullJid, id: &str, payload: Option<Element>) -> Iq {
    Iq::Result {
        from: Some(jid(SERVICE)),
        to: Some(jid::Jid::from(to.clone())),
        id: id.to_owned(),
        payload,
    }
}

/// The XEP-0402 bookmark of [`ROOM`], as a Space lists a room.
fn room_item(namespace: &str) -> Element {
    Element::builder("item", namespace)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), ROOM)
        .append(
            Element::builder("conference", NS_BOOKMARKS)
                .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Editors")
                .build(),
        )
        .build()
}

/// An event notification of `space` from the service to the account.
fn event(space: &str, id: &str, child: Element) -> Message {
    let mut message = Message::new_with_type(MessageType::Headline, Some(jid(ALICE)));
    message.from = Some(jid(SERVICE));
    message.id = Some(Id(id.to_owned()));
    message.payloads.push(
        Element::builder("event", NS_PUBSUB_EVENT)
            .append(
                Element::builder("items", NS_PUBSUB_EVENT)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), space)
                    .append(child)
                    .build(),
            )
            .build(),
    );
    message
}

fn subscriptions(federated: &Federated) -> RemotePubSubSubscriptions {
    RemotePubSubSubscriptions::new(
        federated
            .loopback
            .state
            .deps
            .app_state
            .db_pool
            .global()
            .clone(),
    )
}

/// Wait until the account's record of its subscription to `space` is
/// `held`.
async fn recorded(federated: &Federated, space: &'static str, held: bool) {
    let store = subscriptions(federated);
    let account: jid::BareJid = ALICE.parse().expect("a JID");
    let service: jid::BareJid = SERVICE.parse().expect("a JID");
    support::routing::wait_for_async(support::ARRIVES, || {
        let store = store.clone();
        let account = account.clone();
        let service = service.clone();
        async move {
            (store.holds(&account, &service, space).await.ok() == Some(held)).then_some(())
        }
    })
    .await
    .expect("the subscription record changes");
}

async fn rows(federated: &Federated, table: &str) -> i64 {
    let row = federated
        .loopback
        .state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbQueryOne {
            sql: format!("SELECT COUNT(*) FROM {table}"),
            params: Vec::new(),
        })
        .await
        .expect("count query")
        .expect("count row");
    match row.first() {
        Some(Value::Integer(count)) => *count,
        other => panic!("unexpected count {other:?}"),
    }
}

fn descendant<'a>(element: &'a Element, name: &str) -> Option<&'a Element> {
    element.children().find_map(|child| {
        if child.name() == name {
            Some(child)
        } else {
            descendant(child, name)
        }
    })
}

#[tokio::test]
async fn a_local_account_discovers_and_reads_a_space_hosted_elsewhere() {
    let pki = Pki::new();
    let federated = with_space_host(&pki).await;
    let mut phone = online_alice(&federated).await;
    let alice = full_jid(&phone);

    // "Fetching spaces from a spaces service".
    send(
        &mut phone,
        to_service(
            "list-1",
            false,
            Element::builder("query", NS_DISCO_ITEMS).build(),
        ),
    )
    .await;
    let asked = federated.peer_receives("list-1").await;
    assert_eq!(asked.attr("from"), Some(alice.as_str()));
    assert_eq!(asked.attr("to"), Some(SERVICE));
    let listing = Element::builder("query", NS_DISCO_ITEMS)
        .append(
            Element::builder("item", NS_DISCO_ITEMS)
                .attr(minidom::rxml::xml_ncname!("jid").to_owned(), SERVICE)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), DEV)
                .attr(
                    minidom::rxml::xml_ncname!("name").to_owned(),
                    "The developers corner",
                )
                .build(),
        )
        .build();
    federated
        .peer_service_sends(SERVICE, result(&alice, "list-1", Some(listing)))
        .await;
    let listed = receive(&mut phone, "iq", "list-1").await;
    assert_eq!(listed.attr("type"), Some("result"));
    assert_eq!(listed.attr("from"), Some(SERVICE));
    assert_eq!(
        descendant(&listed, "item").and_then(|item| item.attr("node")),
        Some(DEV)
    );

    // "Fetching the metadata associated to a space".
    send(
        &mut phone,
        to_service("info-1", false, node("query", NS_DISCO_INFO, DEV)),
    )
    .await;
    let asked = federated.peer_receives("info-1").await;
    assert_eq!(
        descendant(&asked, "query").and_then(|query| query.attr("node")),
        Some(DEV)
    );
    let metadata = Element::builder("query", NS_DISCO_INFO)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), DEV)
        .append(
            Element::builder("identity", NS_DISCO_INFO)
                .attr(minidom::rxml::xml_ncname!("category").to_owned(), "pubsub")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "leaf")
                .build(),
        )
        .append(
            Element::builder("x", NS_DATA)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
                .append(
                    Element::builder("field", NS_DATA)
                        .attr(minidom::rxml::xml_ncname!("var").to_owned(), "pubsub#type")
                        .append(
                            Element::builder("value", NS_DATA)
                                .append("urn:xmpp:spaces:0")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    federated
        .peer_service_sends(SERVICE, result(&alice, "info-1", Some(metadata)))
        .await;
    let described = receive(&mut phone, "iq", "info-1").await;
    assert_eq!(
        descendant(&described, "value")
            .map(Element::text)
            .as_deref(),
        Some("urn:xmpp:spaces:0")
    );

    // "Discovering the content of a space", with an image item whose
    // metadata names a URL (0.3.0, "Spaces images").
    send(
        &mut phone,
        to_service("items-1", false, pubsub(node("items", NS_PUBSUB, DEV))),
    )
    .await;
    federated.peer_receives("items-1").await;
    let mark = federated.peer_mark();
    let content = pubsub(
        Element::builder("items", NS_PUBSUB)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), DEV)
            .append(room_item(NS_PUBSUB))
            .append(
                Element::builder("item", NS_PUBSUB)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), AVATAR_ITEM)
                    .append(
                        Element::builder("metadata", "urn:xmpp:avatar:metadata")
                            .append(
                                Element::builder("info", "urn:xmpp:avatar:metadata")
                                    .attr(minidom::rxml::xml_ncname!("url").to_owned(), AVATAR_URL)
                                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "sha1")
                                    .attr(
                                        minidom::rxml::xml_ncname!("type").to_owned(),
                                        "image/jpeg",
                                    )
                                    .attr(minidom::rxml::xml_ncname!("bytes").to_owned(), "420")
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    federated
        .peer_service_sends(SERVICE, result(&alice, "items-1", Some(content)))
        .await;
    let read = receive(&mut phone, "iq", "items-1").await;
    assert!(descendant(&read, "conference").is_some(), "{read:?}");
    // The image item is relayed as sent, and the server asks nothing of
    // anybody because of it.
    assert_eq!(
        descendant(&read, "info").and_then(|info| info.attr("url")),
        Some(AVATAR_URL)
    );
    let barrier = federated.peer_barrier(SERVICE, ALICE).await;
    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
}

#[tokio::test]
async fn joining_a_space_hosted_elsewhere_is_a_subscription_whose_events_are_delivered() {
    let pki = Pki::new();
    let federated = with_space_host(&pki).await;
    let mut phone = online_alice(&federated).await;
    let alice = full_jid(&phone);

    send(
        &mut phone,
        to_service("join-1", true, subscription("subscribe", DEV)),
    )
    .await;
    let asked = federated.peer_receives("join-1").await;
    assert_eq!(asked.attr("from"), Some(alice.as_str()));
    let subscribe = descendant(&asked, "subscribe").expect("the subscription request");
    assert_eq!(subscribe.attr("node"), Some(DEV));
    assert_eq!(subscribe.attr("jid"), Some(ALICE));
    federated
        .peer_service_sends(SERVICE, result(&alice, "join-1", None))
        .await;
    receive(&mut phone, "iq", "join-1").await;
    recorded(&federated, DEV, true).await;

    // A room listed, and a room taken out.
    federated
        .peer_service_sends(SERVICE, event(DEV, "ev-1", room_item(NS_PUBSUB_EVENT)))
        .await;
    let listed = receive(&mut phone, "message", "ev-1").await;
    assert_eq!(listed.attr("from"), Some(SERVICE));
    assert!(descendant(&listed, "conference").is_some(), "{listed:?}");
    let retraction = Element::builder("retract", NS_PUBSUB_EVENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), ROOM)
        .build();
    federated
        .peer_service_sends(SERVICE, event(DEV, "ev-2", retraction))
        .await;
    receive(&mut phone, "message", "ev-2").await;

    // Another Space of the same service is not the one subscribed to.
    federated
        .peer_service_sends(SERVICE, event("sales", "ev-3", room_item(NS_PUBSUB_EVENT)))
        .await;
    federated.peer_barrier(SERVICE, ALICE).await;
    assert!(!holds(
        &federated.client_settle(&mut phone).await,
        "message",
        "ev-3"
    ));

    // Leaving the Space ends its notifications.
    send(
        &mut phone,
        to_service("leave-1", true, subscription("unsubscribe", DEV)),
    )
    .await;
    federated.peer_receives("leave-1").await;
    recorded(&federated, DEV, false).await;
    federated
        .peer_service_sends(SERVICE, event(DEV, "ev-4", room_item(NS_PUBSUB_EVENT)))
        .await;
    federated.peer_barrier(SERVICE, ALICE).await;
    assert!(!holds(
        &federated.client_settle(&mut phone).await,
        "message",
        "ev-4"
    ));
}

/// ADR-0020 D4: a Space subscription is not a room grant, and nothing is
/// derived from it. It records what admits the Space's notifications and
/// nothing else; a room the Space lists is entered by a join of that room,
/// which this server first verifies with the room's own service and the
/// room's host then decides.
#[tokio::test]
async fn a_subscription_to_a_remote_space_grants_nothing_in_its_rooms() {
    let pki = Pki::new();
    let federated = with_space_host(&pki).await;
    let mut phone = online_alice(&federated).await;
    let alice = full_jid(&phone);

    send(
        &mut phone,
        to_service("sub-1", true, subscription("subscribe", DEV)),
    )
    .await;
    federated.peer_receives("sub-1").await;
    federated
        .peer_service_sends(SERVICE, result(&alice, "sub-1", None))
        .await;
    receive(&mut phone, "iq", "sub-1").await;
    recorded(&federated, DEV, true).await;
    federated
        .peer_service_sends(SERVICE, event(DEV, "ev-5", room_item(NS_PUBSUB_EVENT)))
        .await;
    receive(&mut phone, "message", "ev-5").await;

    // The subscription and the room it was told of left one record: the
    // one that admits the Space's notifications. No occupancy of the room,
    // no bookmark of it and no roster entry was written, and nothing was
    // sent to the room or its service.
    assert_eq!(rows(&federated, "s2s_remote_pubsub_subscriptions").await, 1);
    assert_eq!(rows(&federated, "s2s_remote_room_occupancies").await, 0);
    assert_eq!(rows(&federated, "s2s_remote_room_verdicts").await, 0);
    assert_eq!(rows(&federated, "roster_items").await, 0);
    let state = &federated.loopback.state;
    let bookmarks = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&jid(ALICE).to_bare(), NS_BOOKMARKS, None, &[])
        .await
        .unwrap_or_default();
    assert!(bookmarks.is_empty(), "{bookmarks:?}");
    assert!(federated
        .peer_received(|stanza| { stanza.attr("to").is_some_and(|to| to.ends_with(ROOMS)) })
        .is_empty());

    // Entering the room is a join like any other: held until this server
    // has asked the room's service what it is, whatever the account is
    // subscribed to.
    let mut join = Presence::new(PresenceType::None);
    join.to = Some(jid(&[ROOM, "/alice"].concat()));
    join.id = Some("enter-1".to_owned());
    join.payloads
        .push(Element::builder("x", "http://jabber.org/protocol/muc").build());
    send(&mut phone, join).await;
    let verification = federated
        .peer_receives_matching(|stanza| {
            stanza.name() == "iq"
                && stanza.attr("type") == Some("get")
                && stanza.attr("from") == Some(LOCAL)
                && stanza.attr("to") == Some(ROOMS)
                && descendant(stanza, "query").is_some_and(|query| query.ns() == NS_DISCO_INFO)
        })
        .await;
    assert_eq!(verification.attr("from"), Some(LOCAL));
    tokio::time::sleep(QUIET).await;
    assert!(
        federated
            .peer_received(|stanza| stanza.name() == "presence"
                && stanza.attr("to").is_some_and(|to| to.starts_with(ROOM)))
            .is_empty(),
        "the join left before its room was verified"
    );
}
