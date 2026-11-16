//! XEP-0030 service discovery of the server domain for a remote asker over
//! S2S (ADR-0020 D10): `disco#info` names a server identity and only the
//! `disco#info` feature, because further features are advertised to remote
//! askers only once verification has shown them (gate G21); `disco#items`
//! lists no item until a service is reachable by remote senders; a node is
//! not found; and the services D10 enables but that are not yet reviewed
//! for remote principals refuse discovery.
//!
//! The server domain also asks: before a local resource's join to a remote
//! JID leaves, the server sends that JID's bare JID a `disco#info` request
//! of its own (XEP-0045 §6.4), and only a response from that JID with that
//! id completes it; any other response to the server domain is dropped
//! unanswered.
//!
//! Two in-process deployments on loopback with the connection-manager
//! fixture's test CA. No PostgreSQL, SpiceDB or second process is involved.

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_routing_support/mod.rs"]
mod support;

use minidom::Element;
use support::{
    error_condition, jid, routed_and_capturing, routed_pair, send, wait_for, Routed, ARRIVES,
    HOME_A, HOME_B, QUIET,
};
use waddle_server::destination::{offer_stanza, IqOffer, StanzaOffer, REMOTE_ROUTE_OFFER_TIMEOUT};
use waddle_server::s2s::route::RouterLimits;
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use waddle_xmpp::muc::presence::NS_MUC;
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::Stanza;
use xmpp_parsers::disco::{DiscoInfoResult, DiscoItemsResult, Feature, Identity};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

const ALICE: &str = "alice@home-a.test/phone";
const BOB: &str = "bob@home-b.test/phone";
/// A JID at the peer that a local resource joins as a room.
const ROOM: &str = "room@home-b.test";
const ROOM_ALICE: &str = "room@home-b.test/alice";

fn query(namespace: &str, node: Option<&str>) -> Element {
    let mut builder = Element::builder("query", namespace);
    if let Some(node) = node {
        builder = builder.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    builder.build()
}

/// A `disco#info` result naming a room (XEP-0045 §6.4).
fn room_info() -> Element {
    Element::from(DiscoInfoResult {
        node: None,
        identities: vec![Identity {
            category: "conference".to_owned(),
            type_: "text".to_owned(),
            lang: None,
            name: None,
        }],
        features: vec![Feature::new(DISCO_INFO_NS), Feature::new(NS_MUC)],
        extensions: Vec::new(),
    })
}

/// Offer alice's XEP-0045 §7.2 join of [`ROOM_ALICE`] to `a`'s router, as
/// the WebSocket path does.
async fn offer_join(a: &Routed, id: &str) -> StanzaOffer {
    let mut join = Presence::new(PresenceType::None);
    join.from = Some(jid(ALICE));
    join.to = Some(jid(ROOM_ALICE));
    join.id = Some(id.to_owned());
    join.payloads.push(Element::builder("x", NS_MUC).build());
    offer_stanza(
        &a.router,
        support::remote(&a.served, ROOM_ALICE),
        Stanza::Presence(join),
        a.served.local(),
        REMOTE_ROUTE_OFFER_TIMEOUT,
    )
    .await
}

fn request(id: &str, payload: Element) -> Iq {
    Iq::Get {
        from: Some(jid(ALICE)),
        to: None,
        id: id.to_owned(),
        payload,
    }
}

#[tokio::test]
async fn disco_info_of_a_peer_server_names_a_server_and_disco_info_only() {
    let pki = support::Pki::new();
    let (a, _b) = routed_pair(&pki, RouterLimits::default()).await;
    assert!(matches!(
        a.offer_iq(HOME_B, request("d1", query(DISCO_INFO_NS, None)))
            .await,
        IqOffer::Accepted
    ));
    let (_, response) = wait_for(ARRIVES, || a.delivery.iqs("d1").into_iter().next())
        .await
        .expect("the peer answers");
    let Iq::Result {
        payload: Some(payload),
        from,
        ..
    } = response
    else {
        panic!("a result with a payload");
    };
    assert_eq!(from, Some(jid(HOME_B)));
    let info = DiscoInfoResult::try_from(payload).expect("a disco#info result");
    assert_eq!(info.node, None);
    assert_eq!(info.identities.len(), 1);
    assert_eq!(info.identities[0].category, "server");
    assert_eq!(info.identities[0].type_, "im");
    let features: Vec<&str> = info
        .features
        .iter()
        .map(|feature| feature.var.as_str())
        .collect();
    assert_eq!(features, [DISCO_INFO_NS]);
}

#[tokio::test]
async fn disco_items_of_a_peer_server_lists_nothing() {
    let pki = support::Pki::new();
    let (a, _b) = routed_pair(&pki, RouterLimits::default()).await;
    assert!(matches!(
        a.offer_iq(HOME_B, request("d2", query(DISCO_ITEMS_NS, None)))
            .await,
        IqOffer::Accepted
    ));
    let (_, response) = wait_for(ARRIVES, || a.delivery.iqs("d2").into_iter().next())
        .await
        .expect("the peer answers");
    let Iq::Result {
        payload: Some(payload),
        ..
    } = response
    else {
        panic!("a result with a payload");
    };
    let items = DiscoItemsResult::try_from(payload).expect("a disco#items result");
    assert!(items.items.is_empty());
}

#[tokio::test]
async fn a_node_of_the_server_domain_is_not_found_for_a_remote_asker() {
    let pki = support::Pki::new();
    let (_a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    for (id, namespace) in [("n1", DISCO_INFO_NS), ("n2", DISCO_ITEMS_NS)] {
        let iq = Iq::Get {
            from: Some(jid(BOB)),
            to: Some(jid(HOME_A)),
            id: id.to_owned(),
            payload: query(namespace, Some("http://jabber.org/protocol/commands")),
        };
        send(&b, HOME_B, HOME_A, iq).await;
        let reply = wait_for(ARRIVES, || captured.with_id(id).into_iter().next())
            .await
            .expect("answered");
        assert_eq!(reply.attr("type"), Some("error"));
        assert_eq!(error_condition(&reply).as_deref(), Some("item-not-found"));
    }
}

#[tokio::test]
async fn discovery_of_a_service_not_yet_reviewed_for_remote_askers_is_refused() {
    let pki = support::Pki::new();
    let (_a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |spec| {
        spec.local.push("muc.home-a.test");
    })
    .await;
    let iq = Iq::Get {
        from: Some(jid(BOB)),
        to: Some(jid("muc.home-a.test")),
        id: "s1".into(),
        payload: query(DISCO_ITEMS_NS, None),
    };
    send(&b, HOME_B, "muc.home-a.test", iq).await;
    let reply = wait_for(ARRIVES, || captured.with_id("s1").into_iter().next())
        .await
        .expect("answered");
    assert_eq!(
        error_condition(&reply).as_deref(),
        Some("service-unavailable")
    );
}

#[tokio::test]
async fn a_join_asks_the_bare_jid_for_disco_info_from_the_server_domain() {
    let pki = support::Pki::new();
    let (a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    let offered = offer_join(&a, "join").await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    let asked = wait_for(ARRIVES, || {
        captured
            .stanzas()
            .into_iter()
            .find(|stanza| stanza.name() == "iq")
    })
    .await
    .expect("the server asks the JID");
    assert_eq!(asked.attr("type"), Some("get"));
    assert_eq!(asked.attr("from"), Some(HOME_A));
    assert_eq!(asked.attr("to"), Some(ROOM));
    let id = asked.attr("id").expect("an id").to_owned();
    assert!(id.starts_with("room-"), "{id}");
    let queries: Vec<&Element> = asked.children().collect();
    assert_eq!(queries.len(), 1);
    assert!(queries[0].is("query", DISCO_INFO_NS));
    assert_eq!(queries[0].attr("node"), None);
    assert_eq!(queries[0].children().count(), 0);

    // Responses that do not match the request complete nothing: another
    // id, or the right id from another JID at the peer.
    for (from, id) in [(ROOM, "room-forged"), ("other@home-b.test", id.as_str())] {
        let response = Iq::Result {
            from: Some(jid(from)),
            to: Some(jid(HOME_A)),
            id: id.to_owned(),
            payload: Some(room_info()),
        };
        send(&b, HOME_B, HOME_A, response).await;
    }
    tokio::time::sleep(QUIET).await;
    assert!(
        captured
            .stanzas()
            .iter()
            .all(|stanza| stanza.name() != "presence"),
        "the join is still held"
    );

    // The JID's own answer naming a room releases the join.
    let response = Iq::Result {
        from: Some(jid(ROOM)),
        to: Some(jid(HOME_A)),
        id,
        payload: Some(room_info()),
    };
    send(&b, HOME_B, HOME_A, response).await;
    let join = wait_for(ARRIVES, || captured.with_id("join").into_iter().next())
        .await
        .expect("the join leaves");
    assert_eq!(join.name(), "presence");
    assert_eq!(join.attr("from"), Some(ALICE));
    assert_eq!(join.attr("to"), Some(ROOM_ALICE));
}

#[tokio::test]
async fn a_response_to_the_server_domain_that_completes_no_request_is_dropped_unanswered() {
    let pki = support::Pki::new();
    let (a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    send(
        &b,
        HOME_B,
        HOME_A,
        Iq::Result {
            from: Some(jid(ROOM)),
            to: Some(jid(HOME_A)),
            id: "room-stray".into(),
            payload: Some(room_info()),
        },
    )
    .await;
    send(
        &b,
        HOME_B,
        HOME_A,
        Iq::Error {
            from: Some(jid(ROOM)),
            to: Some(jid(HOME_A)),
            id: "room-stray-error".into(),
            payload: None,
            error: xmpp_parsers::stanza_error::StanzaError::new(
                xmpp_parsers::stanza_error::ErrorType::Cancel,
                xmpp_parsers::stanza_error::DefinedCondition::ItemNotFound,
                "en",
                "No such room.",
            ),
        },
    )
    .await;
    // A ping sent after them is answered, so they have been read.
    send(
        &b,
        HOME_B,
        HOME_A,
        Iq::Get {
            from: Some(jid(BOB)),
            to: Some(jid(HOME_A)),
            id: "after".into(),
            payload: Element::builder("ping", NS_PING).build(),
        },
    )
    .await;
    assert!(
        wait_for(ARRIVES, || captured.with_id("after").into_iter().next())
            .await
            .is_some(),
        "the ping is answered"
    );
    tokio::time::sleep(QUIET).await;
    assert!(captured.with_id("room-stray").is_empty());
    assert!(captured.with_id("room-stray-error").is_empty());
    assert!(a.delivery.delivered().is_empty());

    // Nothing was recorded about the JID: a join still asks it.
    let offered = offer_join(&a, "join").await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    assert!(
        wait_for(ARRIVES, || captured.stanzas().into_iter().find(
            |stanza| stanza.name() == "iq" && stanza.attr("to") == Some(ROOM)
        ))
        .await
        .is_some(),
        "the JID is asked"
    );
    tokio::time::sleep(QUIET).await;
    assert!(captured.with_id("join").is_empty(), "the join is held");
}
