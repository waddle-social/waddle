//! Foreign discovery rejection over actual WebSockets and SCRAM-bound sessions.

use std::time::Duration;

use jid::Jid;
use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const DOMAIN: &str = "localhost";

async fn send_iq(client: &mut WsXmppClient, iq: Iq) {
    let element: Element = iq.into();
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize IQ");
    client
        .send(&String::from_utf8(bytes).expect("IQ UTF-8"))
        .await
        .expect("send IQ");
}

async fn receive_iq(client: &mut WsXmppClient, id: &str) -> Iq {
    let frame = client
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.name() == "iq" && element.attr("id") == Some(id))
        })
        .await
        .expect("correlated IQ");
    Iq::try_from(frame.parse::<Element>().expect("IQ XML")).expect("typed IQ")
}

async fn round_trip(client: &mut WsXmppClient, iq: Iq) -> Iq {
    let id = iq.id().to_owned();
    send_iq(client, iq).await;
    receive_iq(client, &id).await
}

fn full_jid(client: &WsXmppClient) -> Jid {
    client
        .full_jid
        .as_deref()
        .expect("bound client")
        .parse()
        .expect("bound JID")
}

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
            assert_eq!(error.defined_condition, DefinedCondition::NotAllowed);
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
