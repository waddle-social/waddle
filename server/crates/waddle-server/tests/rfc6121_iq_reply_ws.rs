//! Two authenticated WebSocket clients complete local full-JID IQ exchanges.

use minidom::Element;
use waddle_ws_test_support::iq::{bound_jid, receive_iq, round_trip, send_iq};
use waddle_ws_test_support::{TestServer, WsXmppClient};
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
    let own_bare = bound_jid(&origin).to_bare();
    let block = Iq::Set {
        from: None,
        to: None,
        id: "block-own-account".into(),
        payload: Element::builder("block", NS_BLOCKING)
            .append(
                Element::builder("item", NS_BLOCKING)
                    .attr(
                        minidom::rxml::xml_ncname!("jid").to_owned(),
                        own_bare.as_str(),
                    )
                    .build(),
            )
            .build(),
    };
    assert!(matches!(
        round_trip(&mut origin, block).await,
        Iq::Result { .. }
    ));
    exercise_roundtrips(&mut origin, &mut sibling).await;
    origin.close().await.expect("close origin");
    sibling.close().await.expect("close sibling");
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
    // Both streams remain usable after forwarding replies; no synthetic reply
    // was sent back to the answering client.
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
