//! S2S routing between two in-process deployments on loopback (ADR-0020
//! transport, D7 and D1; gates G8 and G10): a local resource's request is
//! queued on its pair's egress queue, written on a stream dialled on
//! demand and its response delivered once; destinations and peers the
//! router refuses; failures answered with `remote-server-not-found` or
//! `remote-server-timeout`; bounded queues and pending requests; server-
//! minted credentials stripped; local responses sent only for a pending
//! remote request; and the remote principal a stanza is read as.
//!
//! Certificates come from the connection-manager fixture's test CA, the
//! correlation tables are in-memory SQLite databases and local resources
//! are a recording delivery. No PostgreSQL, SpiceDB or second process is
//! involved.

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_routing_support/mod.rs"]
mod support;

use std::time::Duration;

use minidom::Element;
use support::{
    full, jid, listener, routed_and_capturing, routed_pair, start_routed, wait_for, Pki, Spec,
    ARRIVES, HOME_A, HOME_B, QUIET,
};
use waddle_server::destination::{IqOffer, UnroutableReason};
use waddle_server::s2s::conn::StreamDirection;
use waddle_server::s2s::route::{PrincipalError, RemotePrincipal, RouterLimits};
use waddle_server::s2s::verify::TrustProfileKind;
use waddle_xmpp::xep::xep0166::NS_JINGLE;
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::xep::xep_waddle_livekit_transport::{NS_WADDLE_LIVEKIT_TRANSPORT, TRANSPORT_NAME};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const ALICE: &str = "alice@home-a.test/phone";

fn ping(id: &str) -> Iq {
    Iq::Get {
        from: Some(jid(ALICE)),
        to: None,
        id: id.to_owned(),
        payload: Element::builder("ping", NS_PING).build(),
    }
}

fn refused(offer: IqOffer) -> (UnroutableReason, Iq) {
    match offer {
        IqOffer::Refused {
            reason,
            reply: Some(reply),
        } => (reason, *reply),
        other => panic!("expected a refusal with a reply, got {other:?}"),
    }
}

fn stanza_error(iq: &Iq) -> &xmpp_parsers::stanza_error::StanzaError {
    match iq {
        Iq::Error { error, .. } => error,
        other => panic!("expected an IQ error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_request_to_a_peer_is_answered_and_its_response_delivered_once() {
    let pki = Pki::new();
    let (a, _b) = routed_pair(&pki, RouterLimits::default()).await;
    assert!(matches!(
        a.offer_iq(HOME_B, ping("p1")).await,
        IqOffer::Accepted
    ));
    let delivered = wait_for(ARRIVES, || {
        let iqs = a.delivery.iqs("p1");
        (!iqs.is_empty()).then_some(iqs)
    })
    .await
    .expect("the peer's response is delivered");
    let (to, response) = &delivered[0];
    assert_eq!(*to, full(ALICE));
    let Iq::Result { from, to, .. } = response else {
        panic!("a result, got {response:?}");
    };
    assert_eq!(from.as_ref(), Some(&jid(HOME_B)));
    assert_eq!(to.as_ref(), Some(&jid(ALICE)));
    assert!(
        wait_for(ARRIVES, || (a.router.pending_requests() == 0).then_some(()))
            .await
            .is_some()
    );
    tokio::time::sleep(QUIET).await;
    assert_eq!(a.delivery.iqs("p1").len(), 1, "delivered at most once");
}

#[tokio::test]
async fn requests_to_peers_admission_refuses_are_answered_without_dialling() {
    let pki = Pki::new();
    let (listener_a, _) = listener().await;
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.deny = vec!["denied.test"];
    let a = start_routed(
        &pki,
        &spec,
        listener_a,
        support::correlation_database("s2s-routing-admission").await,
        RouterLimits::default(),
    )
    .await;
    for (destination, id) in [("stranger.test", "s1"), ("sub.denied.test", "s2")] {
        let request = ping(id);
        let (reason, reply) = refused(a.offer_iq(destination, request).await);
        assert_eq!(reason, UnroutableReason::PeerNotAdmitted);
        assert_eq!(reply.id(), id);
        assert_eq!(reply.to(), Some(&jid(ALICE)));
        let error = stanza_error(&reply);
        assert_eq!(
            error.defined_condition,
            DefinedCondition::RemoteServerNotFound
        );
        assert_eq!(error.type_, ErrorType::Cancel);
    }
    assert!(a.deployment.handle.streams().is_empty(), "nothing dialled");
    assert_eq!(a.router.pending_requests(), 0);
}

#[tokio::test]
async fn a_peer_that_cannot_be_reached_answers_the_request_with_remote_server_timeout() {
    let pki = Pki::new();
    let (listener_a, _) = listener().await;
    let (_closed, dead) = support::transport::dead_port();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.endpoints = vec![(HOME_B, dead)];
    let a = start_routed(
        &pki,
        &spec,
        listener_a,
        support::correlation_database("s2s-routing-unreachable").await,
        RouterLimits::default(),
    )
    .await;
    assert!(matches!(
        a.offer_iq(HOME_B, ping("t1")).await,
        IqOffer::Accepted
    ));
    let delivered = wait_for(ARRIVES, || a.delivery.iqs("t1").into_iter().next())
        .await
        .expect("the requester is answered");
    let error = stanza_error(&delivered.1);
    assert_eq!(
        error.defined_condition,
        DefinedCondition::RemoteServerTimeout
    );
    assert_eq!(error.type_, ErrorType::Wait);
    assert_eq!(delivered.1.from(), Some(&jid(HOME_B)));
    assert_eq!(a.router.pending_requests(), 0);

    // While the domain backs off, the next request fails at once.
    assert!(matches!(
        a.offer_iq(HOME_B, ping("t2")).await,
        IqOffer::Accepted
    ));
    let delivered = wait_for(ARRIVES, || a.delivery.iqs("t2").into_iter().next())
        .await
        .expect("the requester is answered");
    assert_eq!(
        stanza_error(&delivered.1).defined_condition,
        DefinedCondition::RemoteServerTimeout
    );
}

#[tokio::test]
async fn a_full_egress_queue_refuses_with_resource_constraint() {
    let pki = Pki::new();
    let (listener_a, _) = listener().await;
    // A peer that accepts TCP connections and never speaks: the first
    // request occupies the worker until the negotiation deadline.
    let (silent, silent_address) = listener().await;
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.endpoints = vec![(HOME_B, silent_address)];
    let limits = RouterLimits {
        queue_per_peer: 1,
        ..RouterLimits::default()
    };
    let a = start_routed(
        &pki,
        &spec,
        listener_a,
        support::correlation_database("s2s-routing-backpressure").await,
        limits,
    )
    .await;
    let mut accepted = Vec::new();
    let mut busy = None;
    for index in 0..4 {
        let id = ["q", &index.to_string()].concat();
        match a.offer_iq(HOME_B, ping(&id)).await {
            IqOffer::Accepted => accepted.push(id),
            IqOffer::Refused { reason, reply } => {
                busy = Some((reason, reply));
                break;
            }
            IqOffer::Unattributed => panic!("the request has a sender"),
        }
    }
    let (reason, reply) = busy.expect("the queue fills");
    assert_eq!(reason, UnroutableReason::EgressBusy);
    let reply = reply.expect("the refused request is answered");
    let error = stanza_error(&reply);
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ResourceConstraint
    );
    assert_eq!(error.type_, ErrorType::Wait);
    assert!(!accepted.is_empty() && accepted.len() <= 2);
    // Every accepted request is still answered once the dial fails.
    for id in &accepted {
        assert!(
            wait_for(Duration::from_secs(40), || a
                .delivery
                .iqs(id)
                .into_iter()
                .next())
            .await
            .is_some(),
            "request {id} is answered"
        );
    }
    drop(silent);
}

#[tokio::test]
async fn pending_requests_are_bounded_per_resource_and_ids_are_unique() {
    let pki = Pki::new();
    let (listener_a, _) = listener().await;
    let (silent, silent_address) = listener().await;
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.endpoints = vec![(HOME_B, silent_address)];
    let limits = RouterLimits {
        pending_per_resource: 2,
        ..RouterLimits::default()
    };
    let a = start_routed(
        &pki,
        &spec,
        listener_a,
        support::correlation_database("s2s-routing-bounds").await,
        limits,
    )
    .await;
    assert!(matches!(
        a.offer_iq(HOME_B, ping("b1")).await,
        IqOffer::Accepted
    ));
    let (reason, reply) = refused(a.offer_iq(HOME_B, ping("b1")).await);
    assert_eq!(reason, UnroutableReason::InvalidRequest);
    assert_eq!(
        stanza_error(&reply).defined_condition,
        DefinedCondition::BadRequest
    );
    assert!(matches!(
        a.offer_iq(HOME_B, ping("b2")).await,
        IqOffer::Accepted
    ));
    let (reason, _) = refused(a.offer_iq(HOME_B, ping("b3")).await);
    assert_eq!(reason, UnroutableReason::EgressBusy);
    let long_id = "x".repeat(waddle_server::s2s::route::MAX_REQUEST_ID_BYTES + 1);
    let (reason, _) = refused(a.offer_iq(HOME_B, ping(&long_id)).await);
    assert_eq!(reason, UnroutableReason::InvalidRequest);
    drop(silent);
}

#[tokio::test]
async fn only_a_bound_resource_of_a_local_account_sends_requests_to_peers() {
    let pki = Pki::new();
    let (a, _b) = routed_pair(&pki, RouterLimits::default()).await;
    for sender in [
        "alice@home-a.test",
        "home-a.test",
        "room@muc.home-a.test/nick",
    ] {
        let mut request = ping("r1");
        *request.from_mut() = Some(jid(sender));
        let (reason, reply) = refused(a.offer_iq(HOME_B, request).await);
        assert_eq!(reason, UnroutableReason::RefusedByPolicy, "{sender}");
        assert_eq!(
            stanza_error(&reply).defined_condition,
            DefinedCondition::ServiceUnavailable
        );
    }
}

#[tokio::test]
async fn a_router_without_a_transport_refuses_every_stanza() {
    let pki = Pki::new();
    let spec = Spec::new(HOME_A, HOME_B);
    let config = std::sync::Arc::new(spec.load(&pki).await.expect("a valid configuration"));
    let served = support::served(HOME_A);
    let router =
        waddle_server::s2s::route::S2sRouter::new(waddle_server::s2s::route::RouterOptions {
            config,
            served: served.clone(),
            correlations: std::sync::Arc::new(waddle_server::s2s::route::SqlIqCorrelations::new(
                support::correlation_database("s2s-routing-detached").await,
            )),
            limits: RouterLimits::default(),
            stop: None,
        });
    let offer = waddle_server::destination::offer_iq(
        &router,
        support::remote(&served, HOME_B),
        ping("d1"),
        &HOME_A.parse::<jid::DomainPart>().expect("a domain"),
        waddle_server::destination::REMOTE_ROUTE_OFFER_TIMEOUT,
    )
    .await;
    let (reason, _) = refused(offer);
    assert_eq!(reason, UnroutableReason::EgressBusy);
    assert!(router.shutdown(Duration::from_secs(5)).await);
}

#[tokio::test]
async fn server_minted_credentials_never_leave_on_a_stream() {
    let pki = Pki::new();
    let (a, _b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    let transport = Element::builder(TRANSPORT_NAME, NS_WADDLE_LIVEKIT_TRANSPORT)
        .append(Element::builder("token", NS_WADDLE_LIVEKIT_TRANSPORT).append("secret"))
        .build();
    let content = Element::builder("content", NS_JINGLE)
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "voice")
        .append(transport)
        .build();
    let jingle = Element::builder("jingle", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-initiate",
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), "sid-1")
        .append(content)
        .build();
    let request = Iq::Set {
        from: Some(jid(ALICE)),
        to: None,
        id: "j1".into(),
        payload: jingle,
    };
    assert!(matches!(
        a.offer_iq("bob@home-b.test/phone", request).await,
        IqOffer::Accepted
    ));
    let sent = wait_for(ARRIVES, || captured.with_id("j1").into_iter().next())
        .await
        .expect("the request reaches the peer");
    let jingle = sent
        .children()
        .find(|child| child.is("jingle", NS_JINGLE))
        .expect("the Jingle payload");
    let content = jingle
        .children()
        .find(|child| child.name() == "content")
        .expect("the content is kept");
    assert!(content
        .children()
        .all(|child| !child.is(TRANSPORT_NAME, NS_WADDLE_LIVEKIT_TRANSPORT)));
}

#[tokio::test]
async fn a_local_response_leaves_only_for_a_pending_remote_request() {
    let pki = Pki::new();
    let (a, _b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    // No remote request reaches a local entity in this release, so a
    // response from one is never pending and never leaves.
    let response = Iq::Result {
        from: Some(jid(ALICE)),
        to: None,
        id: "unsolicited".into(),
        payload: None,
    };
    assert!(matches!(
        a.offer_iq("bob@home-b.test/phone", response).await,
        IqOffer::Accepted
    ));
    // A request on the same pair after it shows the queue moved on.
    assert!(matches!(
        a.offer_iq(HOME_B, ping("after")).await,
        IqOffer::Accepted
    ));
    assert!(
        wait_for(ARRIVES, || captured.with_id("after").into_iter().next())
            .await
            .is_some()
    );
    assert!(captured.with_id("unsolicited").is_empty());
}

#[tokio::test]
async fn a_remote_principal_carries_the_streams_authenticated_domain_and_strength() {
    let pki = Pki::new();
    let (a, b) = routed_pair(&pki, RouterLimits::default()).await;
    assert!(matches!(
        a.offer_iq(HOME_B, ping("rp")).await,
        IqOffer::Accepted
    ));
    let inbound = wait_for(ARRIVES, || {
        b.deployment
            .handle
            .streams()
            .into_iter()
            .find(|stream| stream.direction() == StreamDirection::Inbound)
    })
    .await
    .expect("home-b.test holds the inbound stream from home-a.test");
    let principal =
        RemotePrincipal::from_stream(&inbound, &jid(ALICE)).expect("a principal of the stream");
    assert_eq!(principal.address(), &jid(ALICE));
    assert_eq!(principal.account().as_str(), "alice@home-a.test");
    assert_eq!(principal.home().as_str(), HOME_A);
    assert_eq!(
        principal.strength(),
        waddle_server::s2s::conn::AuthenticationStrength::Pkix
    );
    assert_eq!(principal.trust_profile(), TrustProfileKind::PrivateCa);
    assert_eq!(principal.full(), Some(full(ALICE)));

    for (address, error) in [
        ("mallory@evil.test", PrincipalError::WrongDomain),
        ("alice@sub.home-a.test", PrincipalError::WrongDomain),
        ("alice@home-b.test", PrincipalError::WrongDomain),
    ] {
        assert_eq!(
            RemotePrincipal::from_stream(&inbound, &jid(address)),
            Err(error),
            "{address}"
        );
    }
    let outbound = b
        .deployment
        .handle
        .streams()
        .into_iter()
        .find(|stream| stream.direction() == StreamDirection::Outbound);
    if let Some(outbound) = outbound {
        assert_eq!(
            RemotePrincipal::from_stream(&outbound, &jid(ALICE)),
            Err(PrincipalError::NotInbound)
        );
    }
}
