//! XEP-0030 service discovery of the server domain for a remote asker over
//! S2S (ADR-0020 D10): `disco#info` names a server identity and only the
//! `disco#info` feature, because further features are advertised to remote
//! askers only once verification has shown them (gate G21); `disco#items`
//! lists no item until a service is reachable by remote senders; a node is
//! not found; and the services D10 enables but that are not yet reviewed
//! for remote principals refuse discovery.
//!
//! Two in-process deployments on loopback with the connection-manager
//! fixture's test CA. No PostgreSQL, SpiceDB or second process is involved.

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_routing_support/mod.rs"]
mod support;

use minidom::Element;
use support::{
    error_condition, jid, routed_and_capturing, routed_pair, send, wait_for, ARRIVES, HOME_A,
    HOME_B,
};
use waddle_server::destination::IqOffer;
use waddle_server::s2s::route::RouterLimits;
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use xmpp_parsers::disco::{DiscoInfoResult, DiscoItemsResult};
use xmpp_parsers::iq::Iq;

const ALICE: &str = "alice@home-a.test/phone";
const BOB: &str = "bob@home-b.test/phone";

fn query(namespace: &str, node: Option<&str>) -> Element {
    let mut builder = Element::builder("query", namespace);
    if let Some(node) = node {
        builder = builder.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    builder.build()
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
