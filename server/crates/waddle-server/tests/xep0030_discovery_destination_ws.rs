//! Foreign discovery rejection and canonical local discovery over actual
//! WebSockets and SCRAM-bound sessions.

use minidom::Element;
use waddle_ws_test_support::iq::{bound_jid as full_jid, receive_iq, round_trip, send_iq};
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const DOMAIN: &str = "localhost";

fn query(namespace: &str, target: &str, id: &str) -> Iq {
    Iq::Get {
        from: None,
        to: Some(target.parse().expect("discovery target")),
        id: id.to_owned(),
        payload: Element::builder("query", namespace)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                "foreign-node<&",
            )
            .build(),
    }
}

#[tokio::test]
async fn foreign_discovery_denials_preserve_local_discovery_and_real_peer_forwarding() {
    let server = TestServer::start_with_extra_accounts(&[("peer", "discovery-fixture-password")]);
    let mut origin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "disco-origin",
    )
    .await
    .expect("origin SCRAM/bind");
    let bound = full_jid(&origin);
    for namespace in [DISCO_INFO_NS, DISCO_ITEMS_NS] {
        for target in [
            "remote.test",
            "bob@remote.test/phone",
            "room@muc.remote.test/nick",
        ] {
            let mut request = query(namespace, target, "foreign-disco<&");
            *request.from_mut() = Some(
                "forged@elsewhere.test/resource"
                    .parse()
                    .expect("forged from"),
            );
            let reply = round_trip(&mut origin, request.clone()).await;
            assert_eq!(reply.id(), request.id());
            assert_eq!(reply.from(), request.to());
            assert_eq!(reply.to(), Some(&bound));
            let Iq::Error { error, payload, .. } = reply else {
                panic!("foreign request cannot return local identity/services")
            };
            assert_eq!(error.type_, ErrorType::Cancel);
            assert_eq!(
                error.defined_condition,
                DefinedCondition::RemoteServerNotFound
            );
            assert_eq!(error.by, Some(DOMAIN.parse().expect("home domain")));
            let Iq::Get {
                payload: original, ..
            } = request
            else {
                panic!("get")
            };
            assert_eq!(payload.as_ref(), Some(&original));
        }
    }

    for namespace in [DISCO_INFO_NS, DISCO_ITEMS_NS] {
        let mut request = query(namespace, DOMAIN, "local-disco");
        let Iq::Get { payload, .. } = &mut request else {
            panic!("get")
        };
        *payload = Element::builder("query", namespace).build();
        let reply = round_trip(&mut origin, request).await;
        let Iq::Result {
            payload: Some(query),
            ..
        } = reply
        else {
            panic!("local discovery result")
        };
        assert_eq!(query.ns(), namespace);
        if namespace == DISCO_INFO_NS {
            assert!(query.children().any(
                |child| child.name() == "identity" && child.attr("category") == Some("server")
            ));
        } else {
            assert!(query
                .children()
                .any(|child| child.name() == "item" && child.attr("jid") == Some("muc.localhost")));
            assert!(query.children().any(
                |child| child.name() == "item" && child.attr("jid") == Some("spaces.localhost")
            ));
        }
    }

    let mut peer = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "peer",
        "discovery-fixture-password",
        "disco-peer",
    )
    .await
    .expect("peer SCRAM/bind");
    for namespace in [DISCO_INFO_NS, DISCO_ITEMS_NS] {
        let mut request = query(namespace, full_jid(&peer).as_str(), "peer-disco");
        send_iq(&mut origin, request.clone()).await;
        let forwarded = receive_iq(&mut peer, request.id()).await;
        *request.from_mut() = Some(bound.clone());
        assert_eq!(forwarded, request);
    }
    origin.close().await.expect("close origin");
    peer.close().await.expect("close peer");
}

fn info_query(target: &str, id: &str) -> Iq {
    Iq::Get {
        from: None,
        to: Some(target.parse().expect("discovery target")),
        id: id.to_owned(),
        payload: Element::builder("query", DISCO_INFO_NS).build(),
    }
}

fn identities(query: &Element) -> Vec<(Option<&str>, Option<&str>, Option<&str>)> {
    query
        .children()
        .filter(|child| child.is("identity", DISCO_INFO_NS))
        .map(|child| {
            (
                child.attr("category"),
                child.attr("type"),
                child.attr("name"),
            )
        })
        .collect()
}

/// The disco#info result `reply` must come from `canonical` and must not
/// carry the server's own identity.
fn info_result_from<'a>(reply: &'a Iq, canonical: &str) -> &'a Element {
    assert_eq!(reply.from().map(|jid| jid.as_str()), Some(canonical));
    let Iq::Result {
        payload: Some(query),
        ..
    } = reply
    else {
        panic!("{canonical} must answer with its own disco#info: {reply:?}")
    };
    assert!(query.is("query", DISCO_INFO_NS));
    assert!(
        !identities(query).contains(&(Some("server"), Some("im"), Some("Waddle"))),
        "{canonical} answered with the server identity: {query:?}"
    );
    query
}

/// The item-not-found error `reply` must come from `canonical`.
fn item_not_found_from(reply: &Iq, canonical: &str) {
    assert_eq!(reply.from().map(|jid| jid.as_str()), Some(canonical));
    let Iq::Error { error, .. } = reply else {
        panic!("{canonical} does not exist and must not be answered by the server: {reply:?}")
    };
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::ItemNotFound);
}

// RFC 7622 §3.2: a trailing label separator does not change the entity. A
// disco#info request to a local account, the MUC service or a room written
// with one is answered by that entity, from its canonical address, and never
// by the server on its behalf.
#[tokio::test]
async fn a_trailing_domain_dot_is_answered_by_the_addressed_local_entity() {
    let server = TestServer::start_with_extra_accounts(&[("peer", "discovery-fixture-password")]);
    let mut origin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "disco-dot-origin",
    )
    .await
    .expect("origin SCRAM/bind");

    // The sender's own bare JID is its personal archive and PEP service.
    let reply = round_trip(&mut origin, info_query("admin@localhost.", "dot-own")).await;
    let query = info_result_from(&reply, "admin@localhost");
    assert!(identities(query).contains(&(
        Some("pubsub"),
        Some("pep"),
        Some("Personal Eventing Protocol")
    )));
    assert!(identities(query).contains(&(Some("server"), Some("im"), Some("Personal Archive"))));

    // Another existing account answers as a PEP service.
    let reply = round_trip(&mut origin, info_query("peer@localhost.", "dot-peer")).await;
    let query = info_result_from(&reply, "peer@localhost");
    assert_eq!(
        identities(query),
        vec![(
            Some("pubsub"),
            Some("pep"),
            Some("Personal Eventing Protocol")
        )]
    );

    // An account that does not exist is reported missing, not described.
    let reply = round_trip(&mut origin, info_query("bob@localhost.", "dot-missing")).await;
    item_not_found_from(&reply, "bob@localhost");

    // The MUC service describes itself.
    let reply = round_trip(&mut origin, info_query("muc.localhost.", "dot-muc")).await;
    let query = info_result_from(&reply, "muc.localhost");
    assert!(identities(query).contains(&(
        Some("conference"),
        Some("text"),
        Some("Waddle Chatrooms")
    )));

    // A room that does not exist is reported missing by the MUC service.
    let reply = round_trip(
        &mut origin,
        info_query("no-such-room@muc.localhost.", "dot-room"),
    )
    .await;
    item_not_found_from(&reply, "no-such-room@muc.localhost");

    origin.close().await.expect("close origin");
}
