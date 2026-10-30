//! Two authenticated WebSocket clients complete local full-JID IQ exchanges.

use minidom::Element;
use waddle_ws_test_support::iq::{bound_jid, receive_iq, round_trip, send_iq};
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::xep::xep0199::NS_PING;
use xmpp_parsers::iq::{Iq, IqHeader, IqPayload};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

#[tokio::test]
async fn clients_receive_authentic_peer_results_and_errors_over_websockets() {
    let server = TestServer::start_with_extra_accounts(&[("peer", "iq-reply-fixture-password")]);
    let mut clients = Vec::new();
    for (username, password, resource) in [
        ("admin", server.fixed_account_password(), "iq-origin"),
        ("peer", "iq-reply-fixture-password", "iq-peer"),
    ] {
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
    let [mut origin, mut peer]: [WsXmppClient; 2] = clients.try_into().ok().expect("two clients");
    let origin_jid = bound_jid(&origin);
    let peer_jid = bound_jid(&peer);
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
        send_iq(&mut origin, request.clone()).await;
        let forwarded = receive_iq(&mut peer, request.id()).await;
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
        send_iq(&mut peer, reply.clone()).await;
        *reply.from_mut() = Some(peer_jid.clone());
        assert_eq!(receive_iq(&mut origin, request.id()).await, reply);
    }
    // Both streams remain usable after forwarding replies; no synthetic reply
    // was sent back to the answering client.
    for client in [&mut origin, &mut peer] {
        let ping = Iq::Get {
            from: None,
            to: Some("localhost".parse().expect("home domain")),
            id: "local-after-peer-replies".into(),
            payload: payload.clone(),
        };
        assert!(matches!(round_trip(client, ping).await, Iq::Result { .. }));
    }
    origin.close().await.expect("close origin");
    peer.close().await.expect("close peer");
}
