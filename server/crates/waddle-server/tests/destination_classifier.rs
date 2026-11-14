//! RFC 6120 §10 destination classification and the remote-route hook, tested
//! through the crate's public `destination` API without a running server.

use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, DomainPart, FullJid, Jid};
use minidom::Element;
use waddle_server::destination::{
    offer_iq, unroutable_iq_reply, without_server_minted_credentials, Destination,
    FederationDisabled, InvalidDestination, IqOffer, RemoteDestination, RemoteOutbound,
    RemoteRoute, RemoteRouteOutcome, ServedDomains, ServedService, UnroutableReason,
    REMOTE_ROUTE_OFFER_TIMEOUT,
};
use waddle_xmpp::xep::xep0166::NS_JINGLE;
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::xep::xep0410::{interpret_self_ping_response, SelfPingResult};
use waddle_xmpp::xep::xep_waddle_livekit_transport::{NS_WADDLE_LIVEKIT_TRANSPORT, TRANSPORT_NAME};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const HOME: &str = "example.com";
const SENDER: &str = "alice@example.com/web";

fn domain(raw: &str) -> DomainPart {
    raw.parse().expect("test domain")
}

fn jid(raw: &str) -> Jid {
    raw.parse().expect("test JID")
}

fn served() -> ServedDomains {
    ServedDomains::new(domain(HOME))
        .with_service(ServedService::Muc, domain("muc.example.com"))
        .with_service(ServedService::Spaces, domain("spaces.example.com"))
        .with_service(ServedService::Upload, domain("upload.example.com"))
        .with_service(ServedService::Community, domain("community.example.com"))
        .with_service(ServedService::Extensions, domain("extensions.example.com"))
        .with_service(ServedService::Push, domain("push.example.com"))
        .with_service(ServedService::CallsMixer, domain("calls.example.com"))
}

fn classify(raw: &str) -> Destination {
    served().classify(Some(&jid(raw)))
}

fn ping_get(to: &str) -> Iq {
    Iq::Get {
        from: Some(jid("forged@elsewhere.test/resource")),
        to: Some(jid(to)),
        id: "ping<&".into(),
        payload: Element::builder("ping", NS_PING).build(),
    }
}

#[test]
fn missing_and_home_server_addresses_are_handled_by_the_server() {
    assert_eq!(served().classify(None), Destination::Missing);
    assert_eq!(classify(HOME), Destination::Server);
    // RFC 7622 §3.2: a trailing label separator is not part of the domain.
    assert_eq!(classify("example.com."), Destination::Server);
    assert_eq!(
        classify("example.com/console"),
        Destination::ServerResource("example.com/console".parse().expect("full"))
    );
}

#[test]
fn local_account_addresses_keep_bare_and_full_semantics() {
    assert_eq!(
        classify("bob@example.com"),
        Destination::LocalBare("bob@example.com".parse().expect("bare"))
    );
    assert_eq!(
        classify("bob@example.com/phone"),
        Destination::LocalFull("bob@example.com/phone".parse().expect("full"))
    );
}

#[test]
fn a_trailing_domain_dot_is_removed_before_classification() {
    // RFC 7622 §3.2: the trailing label separator is not part of the domain.
    // The `jid` crate keeps it in `node@domain.` and offsets the resourcepart
    // of `node@domain./resource`, so both forms are checked part by part.
    let Destination::LocalBare(bare) = classify("bob@example.com.") else {
        panic!("local bare account");
    };
    assert_eq!(bare.node().map(|node| node.as_str()), Some("bob"));
    assert_eq!(bare.domain().as_str(), HOME);
    assert_eq!(bare, "bob@example.com".parse::<BareJid>().expect("bare"));

    let Destination::LocalFull(full) = classify("bob@example.com./phone") else {
        panic!("local full JID");
    };
    assert_eq!(full.node().map(|node| node.as_str()), Some("bob"));
    assert_eq!(full.domain().as_str(), HOME);
    assert_eq!(full.resource().as_str(), "phone");
    assert_eq!(
        full,
        "bob@example.com/phone".parse::<FullJid>().expect("full")
    );

    assert_eq!(
        classify("example.com./console"),
        Destination::ServerResource("example.com/console".parse().expect("full"))
    );
    assert_eq!(
        classify("room@muc.example.com./nick"),
        Destination::Service {
            service: ServedService::Muc,
            address: jid("room@muc.example.com/nick"),
        }
    );

    for (address, canonical) in [
        ("bob@remote.test.", "bob@remote.test"),
        ("bob@remote.test./phone", "bob@remote.test/phone"),
    ] {
        let Destination::Remote(remote) = classify(address) else {
            panic!("{address} must be remote");
        };
        assert_eq!(remote.address(), &jid(canonical), "{address}");
        assert_eq!(remote.domain().as_str(), "remote.test", "{address}");
        assert_eq!(
            remote
                .address()
                .resource()
                .map(|resource| resource.as_str()),
            jid(canonical).resource().map(|resource| resource.as_str()),
            "{address}"
        );
    }

    assert!(matches!(
        classify("bob@intranet./phone"),
        Destination::Invalid {
            reason: InvalidDestination::UnqualifiedHost,
            ..
        }
    ));
}

#[test]
fn every_configured_service_domain_is_classified_as_that_service() {
    for (address, expected) in [
        ("muc.example.com", ServedService::Muc),
        ("room@muc.example.com", ServedService::Muc),
        ("room@muc.example.com/nick", ServedService::Muc),
        ("spaces.example.com", ServedService::Spaces),
        ("upload.example.com", ServedService::Upload),
        ("community.example.com", ServedService::Community),
        ("bot@extensions.example.com/bot", ServedService::Extensions),
        ("push.example.com", ServedService::Push),
        ("calls.example.com", ServedService::CallsMixer),
    ] {
        assert_eq!(
            classify(address),
            Destination::Service {
                service: expected,
                address: jid(address),
            },
            "{address}"
        );
    }
}

#[test]
fn unserved_domains_are_remote_with_exact_matching_only() {
    for address in [
        "remote.test",
        "bob@remote.test",
        "bob@remote.test/phone",
        "room@muc.remote.test/nick",
        "unserved.example.com",
        "notexample.com",
        "example.com.attacker.test",
        "muc.example.com.attacker.test",
        "calls.example.com.attacker.test",
    ] {
        let Destination::Remote(remote) = classify(address) else {
            panic!("{address} must be remote");
        };
        assert_eq!(remote.address(), &jid(address));
        assert_eq!(remote.domain(), jid(address).domain());
    }
}

#[test]
fn ip_literals_and_unqualified_hosts_are_invalid_destinations() {
    for (address, reason) in [
        ("127.0.0.1", InvalidDestination::IpLiteral),
        ("bob@192.0.2.10/phone", InvalidDestination::IpLiteral),
        ("[::1]", InvalidDestination::IpLiteral),
        ("bob@[2001:db8::1]", InvalidDestination::IpLiteral),
        ("intranet", InvalidDestination::UnqualifiedHost),
        ("bob@intranet/phone", InvalidDestination::UnqualifiedHost),
    ] {
        assert_eq!(
            classify(address),
            Destination::Invalid {
                address: jid(address),
                reason,
            },
            "{address}"
        );
    }
}

#[test]
fn a_single_label_home_domain_is_still_local() {
    let served = ServedDomains::new(domain("localhost"))
        .with_service(ServedService::Muc, domain("muc.localhost"));
    assert_eq!(
        served.classify(Some(&jid("localhost"))),
        Destination::Server
    );
    assert!(matches!(
        served.classify(Some(&jid("room@muc.localhost/nick"))),
        Destination::Service {
            service: ServedService::Muc,
            ..
        }
    ));
    assert!(matches!(
        served.classify(Some(&jid("otherhost"))),
        Destination::Invalid {
            reason: InvalidDestination::UnqualifiedHost,
            ..
        }
    ));
}

#[test]
fn unroutable_request_error_is_swapped_bounded_and_locally_attributed() {
    let sender = jid(SENDER);
    for reason in [
        UnroutableReason::FederationDisabled,
        UnroutableReason::InvalidDestination(InvalidDestination::IpLiteral),
        UnroutableReason::InvalidDestination(InvalidDestination::UnqualifiedHost),
    ] {
        let request = ping_get("room@muc.remote.test/nick");
        let reply = unroutable_iq_reply(&request, &sender, sender.domain(), reason)
            .expect("a request always receives an error");
        let Iq::Error {
            from,
            to,
            id,
            payload,
            error,
        } = &reply
        else {
            panic!("an unroutable request must never be acknowledged: {reply:?}");
        };
        assert_eq!(from.as_ref(), request.to(), "RFC 6120 §8.3.1 swap");
        assert_eq!(to.as_ref(), Some(&sender), "bound sender, not client from");
        assert_eq!(id, request.id());
        let Iq::Get {
            payload: original, ..
        } = &request
        else {
            panic!("get");
        };
        assert_eq!(payload.as_ref(), Some(original));
        assert_eq!(error.type_, ErrorType::Cancel);
        assert_eq!(
            error.defined_condition,
            DefinedCondition::RemoteServerNotFound
        );
        assert_eq!(error.by, Some(jid(HOME)));
        // XEP-0410 §3.2: a remote occupant ping is undecided, never "joined".
        assert_eq!(
            interpret_self_ping_response(&reply),
            SelfPingResult::Timeout
        );
    }
}

#[test]
fn unroutable_results_and_errors_never_receive_a_reply() {
    let sender = jid(SENDER);
    let result = Iq::Result {
        from: None,
        to: Some(jid("bob@remote.test/phone")),
        id: "result".into(),
        payload: Some(Element::builder("ping", NS_PING).build()),
    };
    let error = Iq::Error {
        from: None,
        to: Some(jid("bob@remote.test/phone")),
        id: "error".into(),
        payload: None,
        error: StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "en",
            "peer error",
        ),
    };
    for iq in [result, error] {
        for reason in [
            UnroutableReason::FederationDisabled,
            UnroutableReason::InvalidDestination(InvalidDestination::IpLiteral),
        ] {
            assert!(
                unroutable_iq_reply(&iq, &sender, sender.domain(), reason).is_none(),
                "RFC 6120 §8.3.1 rule 8: no error in reply to {iq:?}"
            );
        }
    }
}

fn remote(address: &str) -> RemoteDestination {
    let Destination::Remote(remote) = classify(address) else {
        panic!("{address} must be remote");
    };
    remote
}

fn sent_ping(to: &str) -> Iq {
    let mut iq = ping_get(to);
    *iq.from_mut() = Some(jid(SENDER));
    iq
}

fn assert_request_refusal(
    offer: IqOffer,
    request: &Iq,
    expected: UnroutableReason,
    echoed: bool,
) -> StanzaError {
    let IqOffer::Refused { reason, reply } = offer else {
        panic!("a refused request must report the refusal: {offer:?}");
    };
    assert_eq!(reason, expected);
    let Some(Iq::Error {
        from,
        to,
        id,
        payload,
        error,
    }) = reply.map(|reply| *reply)
    else {
        panic!("a refused request must always receive an error");
    };
    assert_eq!(id, request.id());
    assert_eq!(from.as_ref(), request.to(), "RFC 6120 §8.3.1 swap");
    assert_eq!(to, Some(jid(SENDER)), "the IQ's own sender");
    assert_eq!(error.by, Some(jid(HOME)));
    let Iq::Get {
        payload: original, ..
    } = request
    else {
        panic!("get");
    };
    if echoed {
        assert_eq!(payload.as_ref(), Some(original));
    } else {
        assert_eq!(payload, None, "only the offered IQ may be echoed");
    }
    error
}

#[tokio::test]
async fn disabled_federation_returns_the_offered_stanza_unchanged() {
    let destination = remote("bob@remote.test/phone");
    let stanza = sent_ping("bob@remote.test/phone");
    let outbound = RemoteOutbound::new(destination.clone(), Stanza::Iq(Box::new(stanza.clone())))
        .expect("a stanza with from has a sender");
    assert_eq!(outbound.sender(), &jid(SENDER), "sender is the stanza from");
    let outcome = FederationDisabled.route(outbound).await;
    let RemoteRouteOutcome::Unroutable { reason, outbound } = outcome else {
        panic!("federation is disabled: nothing may be accepted");
    };
    assert_eq!(reason, UnroutableReason::FederationDisabled);
    assert_eq!(outbound.sender(), &jid(SENDER));
    assert_eq!(outbound.destination(), &destination);
    let (_, Stanza::Iq(returned)) = outbound.into_parts() else {
        panic!("IQ returned");
    };
    assert_eq!(*returned, stanza);
}

#[test]
fn an_outbound_stanza_without_from_has_no_sender() {
    let stanza = Stanza::Iq(Box::new(ping_get("bob@remote.test/phone")));
    let mut unsent = ping_get("bob@remote.test/phone");
    *unsent.from_mut() = None;
    let unsent = Stanza::Iq(Box::new(unsent));
    assert!(RemoteOutbound::new(remote("bob@remote.test/phone"), stanza).is_ok());
    let Err(returned) = RemoteOutbound::new(remote("bob@remote.test/phone"), unsent.clone()) else {
        panic!("no sender, no outbound");
    };
    let (Stanza::Iq(returned), Stanza::Iq(unsent)) = (*returned, unsent) else {
        panic!("IQ returned");
    };
    assert_eq!(returned, unsent);
}

#[tokio::test]
async fn offer_iq_refuses_requests_with_the_echoed_payload_and_drops_responses() {
    let request = sent_ping("bob@remote.test/phone");
    let offer = offer_iq(
        &FederationDisabled,
        remote("bob@remote.test/phone"),
        request.clone(),
        &domain(HOME),
        REMOTE_ROUTE_OFFER_TIMEOUT,
    )
    .await;
    let error = assert_request_refusal(offer, &request, UnroutableReason::FederationDisabled, true);
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(
        error.defined_condition,
        DefinedCondition::RemoteServerNotFound
    );

    let result = Iq::Result {
        from: Some(jid(SENDER)),
        to: Some(jid("bob@remote.test/phone")),
        id: "result".into(),
        payload: None,
    };
    let offer = offer_iq(
        &FederationDisabled,
        remote("bob@remote.test/phone"),
        result,
        &domain(HOME),
        REMOTE_ROUTE_OFFER_TIMEOUT,
    )
    .await;
    assert!(
        matches!(offer, IqOffer::Refused { reply: None, .. }),
        "RFC 6120 §8.3.1 rule 8: {offer:?}"
    );

    let mut unsent = ping_get("bob@remote.test/phone");
    *unsent.from_mut() = None;
    let offer = offer_iq(
        &FederationDisabled,
        remote("bob@remote.test/phone"),
        unsent,
        &domain(HOME),
        REMOTE_ROUTE_OFFER_TIMEOUT,
    )
    .await;
    assert!(matches!(offer, IqOffer::Unattributed), "{offer:?}");
}

struct AcceptingRoute;

#[async_trait]
impl RemoteRoute for AcceptingRoute {
    async fn route(&self, _outbound: RemoteOutbound) -> RemoteRouteOutcome {
        RemoteRouteOutcome::Accepted
    }
}

#[tokio::test]
async fn offer_iq_reports_an_accepted_stanza_without_a_reply() {
    let offer = offer_iq(
        &AcceptingRoute,
        remote("bob@remote.test/phone"),
        sent_ping("bob@remote.test/phone"),
        &domain(HOME),
        REMOTE_ROUTE_OFFER_TIMEOUT,
    )
    .await;
    assert!(matches!(offer, IqOffer::Accepted), "{offer:?}");
}

/// A route that records the stanza it is offered and refuses it.
#[derive(Default)]
struct RecordingRoute {
    offered: std::sync::Mutex<Option<(RemoteDestination, Stanza)>>,
}

#[async_trait]
impl RemoteRoute for RecordingRoute {
    async fn route(&self, outbound: RemoteOutbound) -> RemoteRouteOutcome {
        if let Ok(mut offered) = self.offered.lock() {
            *offered = Some((outbound.destination().clone(), outbound.stanza().clone()));
        }
        RemoteRouteOutcome::Unroutable {
            reason: UnroutableReason::FederationDisabled,
            outbound: Box::new(outbound),
        }
    }
}

#[tokio::test]
async fn an_offered_stanza_names_the_canonical_destination() {
    for (raw, canonical) in [
        ("bob@remote.test.", "bob@remote.test"),
        ("bob@remote.test./phone", "bob@remote.test/phone"),
        ("remote.test.", "remote.test"),
    ] {
        let route = RecordingRoute::default();
        let request = sent_ping(raw);
        let offer = offer_iq(
            &route,
            remote(raw),
            request,
            &domain(HOME),
            REMOTE_ROUTE_OFFER_TIMEOUT,
        )
        .await;
        let (destination, stanza) = route
            .offered
            .lock()
            .expect("recorded offer")
            .take()
            .expect("the IQ was offered");
        assert_eq!(destination.address(), &jid(canonical));
        let Stanza::Iq(offered) = stanza else {
            panic!("IQ offered");
        };
        assert_eq!(offered.to(), Some(&jid(canonical)), "{raw}");
        assert_eq!(offered.to().map(Jid::as_str), Some(canonical), "{raw}");

        let IqOffer::Refused {
            reply: Some(reply), ..
        } = offer
        else {
            panic!("a refused request is answered");
        };
        assert_eq!(reply.from().map(Jid::as_str), Some(canonical), "{raw}");
    }
}

/// A route that blocks on its peer instead of enqueueing.
struct StalledRoute;

#[async_trait]
impl RemoteRoute for StalledRoute {
    async fn route(&self, _outbound: RemoteOutbound) -> RemoteRouteOutcome {
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn a_stalled_route_times_out_into_remote_server_timeout() {
    let request = sent_ping("bob@remote.test/phone");
    let offer = offer_iq(
        &StalledRoute,
        remote("bob@remote.test/phone"),
        request.clone(),
        &domain(HOME),
        Duration::from_millis(50),
    )
    .await;
    let error = assert_request_refusal(offer, &request, UnroutableReason::RouteTimedOut, false);
    assert_eq!(error.type_, ErrorType::Wait);
    assert_eq!(
        error.defined_condition,
        DefinedCondition::RemoteServerTimeout
    );
}

/// A route that refuses but hands back something other than what it was
/// offered.
enum SwappingRoute {
    Message,
    OtherIq,
}

#[async_trait]
impl RemoteRoute for SwappingRoute {
    async fn route(&self, outbound: RemoteOutbound) -> RemoteRouteOutcome {
        let (destination, _) = outbound.into_parts();
        let stanza = match self {
            Self::Message => {
                let mut message = Message::new(Some(jid("bob@remote.test/phone")));
                message.from = Some(jid(SENDER));
                Stanza::Message(message)
            }
            Self::OtherIq => {
                let mut other = sent_ping("bob@remote.test/phone");
                *other.id_mut() = "a-different-request".into();
                Stanza::Iq(Box::new(other))
            }
        };
        let outbound = RemoteOutbound::new(destination, stanza).expect("sender");
        RemoteRouteOutcome::Unroutable {
            reason: UnroutableReason::FederationDisabled,
            outbound: Box::new(outbound),
        }
    }
}

#[tokio::test]
async fn a_route_returning_another_stanza_still_fails_the_request_closed() {
    for route in [SwappingRoute::Message, SwappingRoute::OtherIq] {
        let request = sent_ping("bob@remote.test/phone");
        let offer = offer_iq(
            &route,
            remote("bob@remote.test/phone"),
            request.clone(),
            &domain(HOME),
            REMOTE_ROUTE_OFFER_TIMEOUT,
        )
        .await;
        assert_request_refusal(offer, &request, UnroutableReason::FederationDisabled, false);
    }
}

#[test]
fn server_minted_jingle_credentials_are_removed_before_leaving_the_server() {
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
    let iq = Iq::Set {
        from: Some(jid(SENDER)),
        to: Some(jid("bob@remote.test/phone")),
        id: "jingle".into(),
        payload: jingle,
    };
    let Iq::Set { payload, .. } = without_server_minted_credentials(iq) else {
        panic!("still a set");
    };
    assert!(payload.is("jingle", NS_JINGLE));
    let content = payload
        .get_child("content", NS_JINGLE)
        .expect("sender content kept");
    assert!(content
        .get_child(TRANSPORT_NAME, NS_WADDLE_LIVEKIT_TRANSPORT)
        .is_none());

    // Other payloads pass through untouched.
    let ping = sent_ping("bob@remote.test/phone");
    assert_eq!(without_server_minted_credentials(ping.clone()), ping);
}
