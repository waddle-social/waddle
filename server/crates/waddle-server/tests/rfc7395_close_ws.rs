//! RFC 7395 §3.6 client-initiated close over a real WebSocket.
//!
//! A client may send its last stanza, the XMPP `<close/>` and the WebSocket
//! Close frame without waiting for anything in between. The server must
//! still answer the stanza and reply with its own `<close/>`: it handles each
//! frame before it reads the next, so the WebSocket Close is not seen while
//! the frames in front of it are unanswered.

use waddle_ws_test_support as ws_common;

use std::time::Duration;

use minidom::Element;
use ws_common::{raw_ws as tungstenite, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const NS_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
const NS_PING: &str = "urn:xmpp:ping";
const RECV: Duration = Duration::from_secs(5);

/// Extra server environment that hosts every live loop in the connection
/// actor. Only a server built with `test-support` honours it.
#[cfg(feature = "test-support")]
const ACTOR_HOST: (&str, &str) = (waddle_ws_test_support::LIVE_CONNECTION_HOST_ENV, "actor");

fn text(element: &Element) -> tungstenite::Message {
    tungstenite::Message::Text(String::from(element).into())
}

#[tokio::test]
async fn burst_of_stanza_close_and_websocket_close_is_answered_in_order() {
    burst_of_stanza_close_and_websocket_close_is_answered_in_order_case(TestServer::start()).await;
}

/// The same case with the live loop hosted by the connection actor.
#[cfg(feature = "test-support")]
#[tokio::test]
async fn burst_of_stanza_close_and_websocket_close_is_answered_in_order_under_the_actor_host() {
    burst_of_stanza_close_and_websocket_close_is_answered_in_order_case(
        TestServer::start_with_extra_envs(&[], &[ACTOR_HOST]),
    )
    .await;
}

async fn burst_of_stanza_close_and_websocket_close_is_answered_in_order_case(server: TestServer) {
    let password = server.fixed_account_password().to_string();
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("burst-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection");

    let ping = Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "last-ping")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), DOMAIN)
        .append(Element::builder("ping", NS_PING).build())
        .build();
    let close = Element::builder("close", NS_FRAMING).build();
    client
        .send_raw_burst([text(&ping), text(&close), tungstenite::Message::Close(None)])
        .await
        .expect("send burst");

    let mut saw_ping_result = false;
    let mut saw_close = false;
    loop {
        match client.recv_raw_timeout(RECV).await.expect("frame") {
            Some(tungstenite::Message::Text(frame)) => {
                let element: Element = frame.parse().expect("server frame is XML");
                if element.is("iq", "jabber:client") && element.attr("id") == Some("last-ping") {
                    assert_eq!(element.attr("type"), Some("result"), "{frame}");
                    assert!(!saw_close, "the stanza reply must precede <close/>");
                    saw_ping_result = true;
                } else if element.is("close", NS_FRAMING) {
                    saw_close = true;
                }
            }
            Some(tungstenite::Message::Close(_)) | None => break,
            Some(_) => {}
        }
    }
    assert!(saw_ping_result, "the last stanza was answered");
    assert!(saw_close, "the server replied with <close/>");
}
