//! RFC 6120 §10 destination routing for client IQs over real WebSockets and
//! SCRAM sessions: every destination class, explicit result/error routing,
//! and the no-loop and no-fabrication rules while S2S is not enabled.

use std::time::Duration;

use jid::Jid;
use minidom::Element;
use waddle_ws_test_support::iq::{bound_jid, receive_iq, round_trip, send_iq};
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::carbons::CARBONS_NS;
use waddle_xmpp::disco::DISCO_INFO_NS;
use waddle_xmpp::xep::xep0054::NS_VCARD;
use waddle_xmpp::xep::xep0092::NS_VERSION;
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::xep::xep0410::{interpret_self_ping_response, SelfPingResult};
use xmpp_parsers::iq::{Iq, IqHeader, IqPayload};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const DOMAIN: &str = "localhost";
const PEER_PASSWORD: &str = "destination-routing-peer-password";

async fn connect(
    server: &TestServer,
    username: &str,
    password: &str,
    resource: &str,
) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, username, password, resource)
        .await
        .expect("SCRAM authentication and resource binding")
}

fn jid(raw: &str) -> Jid {
    raw.parse().expect("test JID")
}

fn request(namespace: &str, set: bool, to: Option<Jid>, id: &str) -> Iq {
    let payload = match namespace {
        NS_PING => Element::builder("ping", NS_PING).build(),
        NS_VCARD => Element::builder("vCard", NS_VCARD).build(),
        CARBONS_NS => Element::builder("enable", CARBONS_NS).build(),
        _ => Element::builder("query", namespace).build(),
    };
    IqHeader {
        from: None,
        to,
        id: id.into(),
    }
    .assemble(if set {
        IqPayload::Set(payload)
    } else {
        IqPayload::Get(payload)
    })
}

fn response(to: Jid, error: bool, id: &str) -> Iq {
    let payload = if error {
        IqPayload::Error(StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "en",
            "Peer error.",
        ))
    } else {
        IqPayload::Result(Some(Element::builder("query", DISCO_INFO_NS).build()))
    };
    IqHeader {
        from: None,
        to: Some(to),
        id: id.into(),
    }
    .assemble(payload)
}

/// Assert the bounded RFC 6120 §10.4.3 refusal for a request to an unserved
/// domain: swapped addresses, preserved ID and payload, bound recipient and
/// home-server attribution.
fn assert_unroutable(reply: &Iq, request: &Iq, bound: &Jid) {
    assert_eq!(reply.id(), request.id());
    assert_eq!(reply.from(), request.to(), "RFC 6120 §8.3.1 swapped target");
    assert_eq!(reply.to(), Some(bound), "bound origin, not client from");
    let Iq::Error { error, payload, .. } = reply else {
        panic!("an unserved destination must never be answered locally: {reply:?}");
    };
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(
        error.defined_condition,
        DefinedCondition::RemoteServerNotFound
    );
    assert_eq!(error.by, Some(jid(DOMAIN)));
    let original = match request {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => payload,
        Iq::Result { .. } | Iq::Error { .. } => panic!("request IQ"),
    };
    assert_eq!(payload.as_ref(), Some(original));
}

/// The next IQ on the socket. Used after sending responses that must produce
/// nothing: a following request's reply has to be the first IQ that arrives.
async fn next_iq(client: &mut WsXmppClient) -> Iq {
    let frame = client
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.name() == "iq")
        })
        .await
        .expect("an IQ frame");
    Iq::try_from(frame.parse::<Element>().expect("IQ XML")).expect("typed IQ")
}

async fn assert_silent_then_local_ping(client: &mut WsXmppClient, sentinel: &str) {
    let ping = request(NS_PING, false, None, sentinel);
    send_iq(client, ping).await;
    let reply = next_iq(client).await;
    assert_eq!(
        reply.id(),
        sentinel,
        "no reply may precede the sentinel: {reply:?}"
    );
    assert!(matches!(reply, Iq::Result { .. }), "{reply:?}");
}

#[tokio::test]
async fn server_account_and_service_destinations_keep_local_handling() {
    let server = TestServer::start_with_extra_accounts(&[("peer", PEER_PASSWORD)]);
    let mut origin = connect(&server, "admin", server.fixed_account_password(), "classes").await;
    let bound = bound_jid(&origin);

    // Missing `to` (§10.3), the bare server (§10.5.1), a local bare account
    // (§10.5.3.2) and a configured service domain are answered here.
    for (index, to) in [
        None,
        Some(jid(DOMAIN)),
        Some(Jid::from(bound.to_bare())),
        Some(jid("peer@localhost")),
        Some(jid("muc.localhost")),
        Some(jid("upload.localhost")),
    ]
    .into_iter()
    .enumerate()
    {
        let iq = request(NS_PING, false, to.clone(), &format!("local-{index}"));
        let reply = round_trip(&mut origin, iq).await;
        assert!(matches!(reply, Iq::Result { .. }), "{to:?}: {reply:?}");
    }

    // A server-domain resource can never be bound (§10.5.2): the peer route
    // reports it unavailable instead of a server pong.
    let iq = request(
        NS_PING,
        false,
        Some(jid("localhost/console")),
        "server-resource",
    );
    let reply = round_trip(&mut origin, iq).await;
    let Iq::Error { error, .. } = &reply else {
        panic!("server resource must not be answered as the server: {reply:?}");
    };
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );

    origin.close().await.expect("close origin");
}

#[tokio::test]
async fn local_full_jid_requests_and_responses_are_forwarded_not_consumed() {
    let server = TestServer::start_with_extra_accounts(&[("peer", PEER_PASSWORD)]);
    let mut origin = connect(
        &server,
        "admin",
        server.fixed_account_password(),
        "full-origin",
    )
    .await;
    let mut peer = connect(&server, "peer", PEER_PASSWORD, "full-peer").await;
    let origin_jid = bound_jid(&origin);
    let peer_jid = bound_jid(&peer);

    // A disco#info request to a local full JID reaches the peer client, not
    // the server's discovery handler (§10.5.4).
    let iq = request(DISCO_INFO_NS, false, Some(peer_jid.clone()), "full-disco");
    send_iq(&mut origin, iq).await;
    let forwarded = receive_iq(&mut peer, "full-disco").await;
    assert!(matches!(forwarded, Iq::Get { .. }));
    assert_eq!(forwarded.from(), Some(&origin_jid), "stamped bound sender");

    // Its result carries a disco#info payload; it must reach the requester
    // unchanged rather than be parsed or swallowed by a namespace handler.
    let reply = response(origin_jid.clone(), false, "full-disco");
    send_iq(&mut peer, reply.clone()).await;
    let delivered = receive_iq(&mut origin, "full-disco").await;
    let Iq::Result {
        payload: Some(query),
        ..
    } = &delivered
    else {
        panic!("forwarded result: {delivered:?}");
    };
    assert_eq!(query.name(), "query");
    assert_eq!(query.ns(), DISCO_INFO_NS);
    assert_eq!(delivered.from(), Some(&peer_jid));

    // A peer error is forwarded as-is; the server neither answers it nor
    // generates an error about it.
    let error = response(origin_jid.clone(), true, "full-error");
    send_iq(&mut peer, error).await;
    let delivered = receive_iq(&mut origin, "full-error").await;
    let Iq::Error { error, .. } = &delivered else {
        panic!("forwarded error: {delivered:?}");
    };
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );
    assert_silent_then_local_ping(&mut peer, "peer-after-error").await;

    // A response to an offline local resource is dropped without an error
    // loop (RFC 6120 §8.3.1 rule 8).
    for error in [false, true] {
        let id = format!("offline-response-{error}");
        send_iq(&mut peer, response(jid("admin@localhost/gone"), error, &id)).await;
    }
    assert_silent_then_local_ping(&mut peer, "peer-after-offline").await;

    origin.close().await.expect("close origin");
    peer.close().await.expect("close peer");
}

#[tokio::test]
async fn remote_and_invalid_requests_are_refused_for_every_namespace_without_fabrication() {
    let server = TestServer::start();
    let mut origin = connect(&server, "admin", server.fixed_account_password(), "remote").await;
    let bound = bound_jid(&origin);
    let targets = [
        // Remote (§10.4) while S2S is not enabled.
        "remote.test",
        "bob@remote.test",
        "bob@remote.test/phone",
        "room@muc.remote.test/nick",
        "localhost.attacker.test",
        "muc.localhost.attacker.test",
        // Invalid: no federation route can reach these.
        "127.0.0.1",
        "[::1]",
        "bob@intranet/phone",
    ];
    let namespaces = [
        NS_PING,
        DISCO_INFO_NS,
        NS_VERSION,
        NS_VCARD,
        CARBONS_NS,
        waddle_xmpp::ns::SESSION,
    ];
    for (target_index, target) in targets.into_iter().enumerate() {
        for (namespace_index, namespace) in namespaces.into_iter().enumerate() {
            for set in [false, true] {
                let id = format!("unserved-{target_index}-{namespace_index}-{set}<&");
                let mut iq = request(namespace, set, Some(jid(target)), &id);
                // A client-supplied `from` never addresses the refusal.
                *iq.from_mut() = Some(jid("forged@elsewhere.test/resource"));
                let reply = round_trip(&mut origin, iq.clone()).await;
                assert_unroutable(&reply, &iq, &bound);
            }
        }
    }

    // XEP-0410 §3.2: a refused remote occupant ping is undecided, never
    // evidence of membership.
    let ping = request(
        NS_PING,
        false,
        Some(jid("room@muc.remote.test/me")),
        "self-ping",
    );
    let reply = round_trip(&mut origin, ping).await;
    assert_eq!(
        interpret_self_ping_response(&reply),
        SelfPingResult::Timeout
    );

    // The connection keeps serving local requests afterwards.
    assert_silent_then_local_ping(&mut origin, "after-remote").await;
    origin.close().await.expect("close origin");
}

#[tokio::test]
async fn responses_to_unserved_server_and_service_destinations_are_dropped_silently() {
    let server = TestServer::start();
    let mut origin = connect(&server, "admin", server.fixed_account_password(), "drops").await;
    let targets = [
        "remote.test",
        "bob@remote.test/phone",
        "127.0.0.1",
        "bob@intranet/phone",
        "localhost",
        "localhost/console",
        "admin@localhost",
        "muc.localhost",
        "room@muc.localhost/nick",
    ];
    for (index, target) in targets.into_iter().enumerate() {
        for error in [false, true] {
            send_iq(
                &mut origin,
                response(jid(target), error, &format!("drop-{index}-{error}")),
            )
            .await;
        }
    }
    // RFC 6120 §8.2.3 rule 4 and §8.3.1 rule 8: none of those may be answered.
    assert_silent_then_local_ping(&mut origin, "after-drops").await;
    origin.close().await.expect("close origin");
}

#[tokio::test]
async fn unserved_requests_before_binding_are_refused_not_answered_locally() {
    let server = TestServer::start();
    let mut client = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("WebSocket connect");
    client
        .authenticate(DOMAIN, "admin", server.fixed_account_password())
        .await
        .expect("SCRAM authentication");

    // RFC 6120 §7.1: before binding, a stanza to another domain is not
    // processed. It is neither routed nor answered by a namespace handler
    // (a disco#info here must not return this server's identity).
    for (index, target) in ["remote.test", "bob@remote.test/phone", "127.0.0.1"]
        .into_iter()
        .enumerate()
    {
        for namespace in [DISCO_INFO_NS, NS_PING] {
            let iq = request(
                namespace,
                false,
                Some(jid(target)),
                &format!("prebind-{index}-{namespace}"),
            );
            let reply = round_trip(&mut client, iq.clone()).await;
            let Iq::Error { error, .. } = &reply else {
                panic!("{target}: pre-bind request must be refused: {reply:?}");
            };
            assert_eq!(reply.from(), iq.to(), "RFC 6120 §8.3.1 swap");
            assert_eq!(reply.to(), None, "no bound address to answer");
            assert_eq!(error.type_, ErrorType::Auth);
            assert_eq!(error.defined_condition, DefinedCondition::NotAuthorized);
            assert_eq!(error.by, None);
        }
    }

    // The refusal is a stanza error: the stream stays usable for binding.
    client
        .bind("after-prebind")
        .await
        .expect("bind after refusals");
    client.close().await.expect("close client");
}

#[tokio::test]
async fn a_trailing_domain_dot_routes_to_the_canonical_destination() {
    let server = TestServer::start_with_extra_accounts(&[("peer", PEER_PASSWORD)]);
    let mut origin = connect(
        &server,
        "admin",
        server.fixed_account_password(),
        "dot-origin",
    )
    .await;
    let mut peer = connect(&server, "peer", PEER_PASSWORD, "dot-peer").await;
    let origin_jid = bound_jid(&origin);

    // `peer@localhost./dot-peer` names the bound peer resource (RFC 7622
    // §3.2), not a resource called "/dot-peer" or a remote domain.
    let iq = request(
        DISCO_INFO_NS,
        false,
        Some(jid("peer@localhost./dot-peer")),
        "dot-local",
    );
    send_iq(&mut origin, iq).await;
    let forwarded = receive_iq(&mut peer, "dot-local").await;
    assert!(matches!(forwarded, Iq::Get { .. }), "{forwarded:?}");
    assert_eq!(forwarded.from(), Some(&origin_jid));
    assert_eq!(
        forwarded.to().map(Jid::as_str),
        Some("peer@localhost/dot-peer"),
        "the peer receives the canonical address it bound"
    );

    // A remote destination with a trailing dot is still remote, and the
    // refusal comes from its canonical address.
    for (index, (target, canonical)) in [
        ("bob@remote.test.", "bob@remote.test"),
        ("bob@remote.test./phone", "bob@remote.test/phone"),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("dot-remote-{index}");
        let iq = request(NS_PING, false, Some(jid(target)), &id);
        let reply = round_trip(&mut origin, iq).await;
        let canonical_request = request(NS_PING, false, Some(jid(canonical)), &id);
        assert_eq!(reply.from().map(Jid::as_str), Some(canonical));
        assert_unroutable(&reply, &canonical_request, &origin_jid);
    }

    origin.close().await.expect("close origin");
    peer.close().await.expect("close peer");
}
