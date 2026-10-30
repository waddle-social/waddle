//! Two authenticated WebSocket clients complete local full-JID IQ exchanges.

use std::time::Duration;

use jid::Jid;
use minidom::Element;
use waddle_ws_test_support::iq::{bound_jid, receive_iq, round_trip, send_iq};
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::protocol::handlers::errors::NS_BLOCKING_ERRORS;
use waddle_xmpp::xep::xep0191::NS_BLOCKING;
use waddle_xmpp::xep::xep0199::NS_PING;
use xmpp_parsers::iq::{Iq, IqHeader, IqPayload};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

async fn connect_pair(
    server: &TestServer,
    credentials: [(&str, &str, &str); 2],
) -> [WsXmppClient; 2] {
    let mut clients = Vec::new();
    for (username, password, resource) in credentials {
        clients.push(
            WsXmppClient::connect_and_auth(
                &server.ws_url(),
                "localhost",
                username,
                password,
                resource,
            )
            .await
            .expect("SCRAM/bind"),
        );
    }
    clients.try_into().ok().expect("two clients")
}

#[tokio::test]
async fn clients_receive_authentic_peer_results_and_errors_over_websockets() {
    let server = TestServer::start_with_extra_accounts(&[("peer", "iq-reply-fixture-password")]);
    let [mut origin, mut peer] = connect_pair(
        &server,
        [
            ("admin", server.fixed_account_password(), "iq-origin"),
            ("peer", "iq-reply-fixture-password", "iq-peer"),
        ],
    )
    .await;
    exercise_roundtrips(&mut origin, &mut peer).await;
    origin.close().await.expect("close origin");
    peer.close().await.expect("close peer");
}

#[tokio::test]
async fn own_account_block_rules_allow_sibling_iq_roundtrips_over_websockets() {
    let server = TestServer::start();
    let [mut origin, mut sibling] = connect_pair(
        &server,
        [
            ("admin", server.fixed_account_password(), "own-origin"),
            ("admin", server.fixed_account_password(), "own-sibling"),
        ],
    )
    .await;
    let own_bare = Jid::from(bound_jid(&origin).to_bare());
    set_block(&mut origin, &own_bare, true).await;
    exercise_roundtrips(&mut origin, &mut sibling).await;
    origin.close().await.expect("close origin");
    sibling.close().await.expect("close sibling");
}

async fn set_block(client: &mut WsXmppClient, target: &Jid, blocked: bool) {
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "change-fixture-block".into(),
        payload: Element::builder(if blocked { "block" } else { "unblock" }, NS_BLOCKING)
            .append(
                Element::builder("item", NS_BLOCKING)
                    .attr(
                        minidom::rxml::xml_ncname!("jid").to_owned(),
                        target.as_str(),
                    )
                    .build(),
            )
            .build(),
    };
    assert!(matches!(round_trip(client, iq).await, Iq::Result { .. }));
}

#[tokio::test]
async fn responder_outgoing_block_drops_replies_and_unblock_restores_delivery() {
    let server = TestServer::start_with_extra_accounts(&[("peer", "iq-reply-fixture-password")]);
    let [mut origin, mut responder] = connect_pair(
        &server,
        [
            ("admin", server.fixed_account_password(), "outgoing-origin"),
            ("peer", "iq-reply-fixture-password", "outgoing-responder"),
        ],
    )
    .await;
    let origin_jid = bound_jid(&origin);
    let responder_jid = bound_jid(&responder);
    let request = Iq::Get {
        from: None,
        to: Some(responder_jid.clone()),
        id: "request-before-responder-blocks".into(),
        payload: Element::builder("ping", NS_PING).build(),
    };
    send_iq(&mut origin, request.clone()).await;
    receive_iq(&mut responder, request.id()).await;
    set_block(&mut responder, &Jid::from(origin_jid.to_bare()), true).await;
    for payload in [
        IqPayload::Result(None),
        IqPayload::Error(StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "en",
            "Unavailable.",
        )),
    ] {
        let reply = IqHeader {
            from: None,
            to: Some(origin_jid.clone()),
            id: request.id().into(),
        }
        .assemble(payload);
        send_iq(&mut responder, reply).await;
    }
    // A local round trip orders a barrier after the responder's replies.
    let ping = Iq::Get {
        from: None,
        to: Some("localhost".parse().expect("home domain")),
        id: "outgoing-block-barrier".into(),
        payload: Element::builder("ping", NS_PING).build(),
    };
    assert!(matches!(
        round_trip(&mut responder, ping).await,
        Iq::Result { .. }
    ));
    assert!(
        origin
            .recv_matching_within(Duration::from_millis(200), |frame| {
                frame.parse::<Element>().is_ok_and(|element| {
                    element.name() == "iq" && element.attr("id") == Some(request.id())
                })
            })
            .await
            .is_err(),
        "outgoing blocked replies must not reach origin"
    );
    let mut outgoing = request.clone();
    *outgoing.to_mut() = Some(origin_jid.clone());
    let response = round_trip(&mut responder, outgoing.clone()).await;
    let Iq::Error { error, payload, .. } = &response else {
        panic!("blocked outgoing request error")
    };
    assert_eq!(response.to(), Some(&responder_jid));
    assert_eq!(response.from(), Some(&origin_jid));
    assert_eq!(response.id(), outgoing.id());
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::NotAcceptable);
    assert_eq!(
        error.other,
        Some(Element::builder("blocked", NS_BLOCKING_ERRORS).build())
    );
    assert!(payload.is_some(), "request payload echoed");
    set_block(&mut responder, &Jid::from(origin_jid.to_bare()), false).await;
    exercise_roundtrips(&mut origin, &mut responder).await;
    origin.close().await.expect("close origin");
    responder.close().await.expect("close responder");
}

async fn exercise_roundtrips(origin: &mut WsXmppClient, peer: &mut WsXmppClient) {
    let origin_jid = bound_jid(origin);
    let peer_jid = bound_jid(peer);
    let payload = Element::builder("ping", NS_PING).build();
    let error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::FeatureNotImplemented,
        "en",
        "Client does not support this request.",
    );
    for response in [
        IqPayload::Result(None),
        IqPayload::Result(Some(payload.clone())),
        IqPayload::Error(error),
    ] {
        let request = Iq::Get {
            from: None,
            to: Some(peer_jid.clone()),
            id: "peer-roundtrip<&".into(),
            payload: payload.clone(),
        };
        send_iq(origin, request.clone()).await;
        let forwarded = receive_iq(peer, request.id()).await;
        let mut expected_request = request.clone();
        *expected_request.from_mut() = Some(origin_jid.clone());
        assert_eq!(forwarded, expected_request);
        let mut reply = IqHeader {
            from: Some(
                "forged@remote.test/resource"
                    .parse()
                    .expect("forged sender"),
            ),
            to: Some(origin_jid.clone()),
            id: request.id().into(),
        }
        .assemble(response);
        if let Iq::Error { payload: echo, .. } = &mut reply {
            *echo = Some(payload.clone());
        }
        send_iq(peer, reply.clone()).await;
        *reply.from_mut() = Some(peer_jid.clone());
        assert_eq!(receive_iq(origin, request.id()).await, reply);
    }
    // Both streams remain usable after forwarding replies.
    for client in [origin, peer] {
        let ping = Iq::Get {
            from: None,
            to: Some("localhost".parse().expect("home domain")),
            id: "local-after-peer-replies".into(),
            payload: payload.clone(),
        };
        assert!(matches!(round_trip(client, ping).await, Iq::Result { .. }));
    }
}
