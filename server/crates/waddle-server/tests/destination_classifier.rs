//! RFC 6120 §10 destination classification and the remote-route hook, tested
//! through the crate's public `destination` API without a running server.

use jid::{DomainPart, Jid};
use minidom::Element;
use waddle_server::destination::{
    unroutable_iq_reply, Destination, FederationDisabled, InvalidDestination, RemoteOutbound,
    RemoteRoute, RemoteRouteOutcome, ServedDomains, ServedService, UnroutableReason,
};
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::xep::xep0410::{interpret_self_ping_response, SelfPingResult};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
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

#[tokio::test]
async fn disabled_federation_returns_the_offered_stanza_unchanged() {
    let Destination::Remote(destination) = classify("bob@remote.test/phone") else {
        panic!("remote");
    };
    let mut stanza = ping_get("bob@remote.test/phone");
    *stanza.from_mut() = Some(jid(SENDER));
    let outcome = FederationDisabled
        .route(RemoteOutbound {
            sender: jid(SENDER),
            destination: destination.clone(),
            stanza: Stanza::Iq(Box::new(stanza.clone())),
        })
        .await;
    let RemoteRouteOutcome::Unroutable { reason, outbound } = outcome else {
        panic!("federation is disabled: nothing may be accepted");
    };
    assert_eq!(reason, UnroutableReason::FederationDisabled);
    assert_eq!(outbound.sender, jid(SENDER));
    assert_eq!(outbound.destination, destination);
    let Stanza::Iq(returned) = outbound.stanza else {
        panic!("IQ returned");
    };
    assert_eq!(*returned, stanza);
}
