//! XEP-0199 ping across S2S (ADR-0020 D10): a local resource pings a peer
//! server and receives its result; the server domain answers a remote
//! asker's ping; a ping of a local account or service from a remote asker
//! is refused until that traffic is reviewed (D7, D10); and a ping to a
//! peer that cannot be reached is answered with `remote-server-timeout`
//! (RFC 6120 §10.4.3).
//!
//! Two in-process deployments on loopback with the connection-manager
//! fixture's test CA. No PostgreSQL, SpiceDB or second process is involved.

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_routing_support/mod.rs"]
mod support;

use minidom::Element;
use support::{
    error_condition, jid, listener, routed_and_capturing, routed_pair, send, start_routed,
    wait_for, Spec, ARRIVES, HOME_A, HOME_B,
};
use waddle_server::destination::IqOffer;
use waddle_server::s2s::route::RouterLimits;
use waddle_xmpp::xep::xep0199::NS_PING;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::DefinedCondition;

const ALICE: &str = "alice@home-a.test/phone";
const BOB: &str = "bob@home-b.test/phone";

fn ping_from(from: &str, to: Option<&str>, id: &str) -> Iq {
    Iq::Get {
        from: Some(jid(from)),
        to: to.map(jid),
        id: id.to_owned(),
        payload: Element::builder("ping", NS_PING).build(),
    }
}

#[tokio::test]
async fn a_local_resource_pings_a_peer_server_and_receives_its_result() {
    let pki = support::Pki::new();
    let (a, _b) = routed_pair(&pki, RouterLimits::default()).await;
    assert!(matches!(
        a.offer_iq(HOME_B, ping_from(ALICE, None, "ping-1")).await,
        IqOffer::Accepted
    ));
    let (to, response) = wait_for(ARRIVES, || a.delivery.iqs("ping-1").into_iter().next())
        .await
        .expect("the peer answers");
    assert_eq!(to.to_string(), ALICE);
    let Iq::Result { from, payload, .. } = response else {
        panic!("a result");
    };
    assert_eq!(from, Some(jid(HOME_B)));
    assert_eq!(payload, None);
}

#[tokio::test]
async fn the_server_domain_answers_a_remote_askers_ping_and_refuses_one_for_an_account() {
    let pki = support::Pki::new();
    let (_a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    send(&b, HOME_B, HOME_A, ping_from(BOB, Some(HOME_A), "s2s-ping")).await;
    let reply = wait_for(ARRIVES, || captured.with_id("s2s-ping").into_iter().next())
        .await
        .expect("answered");
    assert_eq!(reply.attr("type"), Some("result"));
    assert_eq!(reply.attr("from"), Some(HOME_A));
    assert_eq!(reply.attr("to"), Some(BOB));

    send(
        &b,
        HOME_B,
        HOME_A,
        ping_from(BOB, Some("alice@home-a.test/phone"), "account-ping"),
    )
    .await;
    let reply = wait_for(ARRIVES, || {
        captured.with_id("account-ping").into_iter().next()
    })
    .await
    .expect("answered");
    assert_eq!(
        error_condition(&reply).as_deref(),
        Some("service-unavailable")
    );
}

#[tokio::test]
async fn a_ping_to_an_unreachable_peer_is_answered_with_remote_server_timeout() {
    let pki = support::Pki::new();
    let (listener_a, _) = listener().await;
    let (_closed, dead) = support::transport::dead_port();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.endpoints = vec![(HOME_B, dead)];
    let a = start_routed(
        &pki,
        &spec,
        listener_a,
        support::correlation_database("xep0199-s2s-unreachable").await,
        RouterLimits::default(),
    )
    .await;
    assert!(matches!(
        a.offer_iq(HOME_B, ping_from(ALICE, None, "ping-dead"))
            .await,
        IqOffer::Accepted
    ));
    let (_, response) = wait_for(ARRIVES, || a.delivery.iqs("ping-dead").into_iter().next())
        .await
        .expect("answered");
    let Iq::Error { error, .. } = response else {
        panic!("an error");
    };
    assert_eq!(
        error.defined_condition,
        DefinedCondition::RemoteServerTimeout
    );
}
