//! RFC 6120 §4.9.3.13 / §8.3.3.1 and RFC 7395 handling of inbound input the
//! server cannot parse, over a real WebSocket.
//!
//! RFC 6120 §4.9.3.13: "The entity has sent XML that cannot be processed."
//! The receiver "MUST" then send the stream error and close the stream
//! (§4.9.1.1). RFC 6120 §8.3.3.1: bad-request means "the sender has sent a
//! stanza containing XML that does not conform to the appropriate schema or
//! that cannot be processed". An unknown payload namespace stays
//! `service-unavailable` (see `rfc6120_unknown_iq_namespace_ws`).

use waddle_ws_test_support as ws_common;

use std::time::Duration;

use ws_common::{raw_ws as tungstenite, TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const NS_XMPP_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
const NS_STREAMS: &str = "http://etherx.jabber.org/streams";
const NS_XMPP_STREAMS: &str = "urn:ietf:params:xml:ns:xmpp-streams";
const NS_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
const RECV: Duration = Duration::from_secs(5);

async fn connect(server: &TestServer) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &password,
        &format!("parse-failure-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection")
}

async fn assert_not_well_formed_closes_stream(frame: &str) {
    let server = TestServer::start();
    let mut client = connect(&server).await;
    client.send(frame).await.expect("send frame");

    let mut saw_error = false;
    let mut saw_close = false;
    loop {
        match client.recv_raw_timeout(RECV).await.expect("server frame") {
            Some(tungstenite::Message::Text(text)) => {
                let element: Element = text.parse().expect("server frame is XML");
                if element.is("error", NS_STREAMS) {
                    assert!(
                        element
                            .get_child("not-well-formed", NS_XMPP_STREAMS)
                            .is_some(),
                        "expected <not-well-formed/>: {text}"
                    );
                    assert!(!saw_close, "stream error precedes <close/>");
                    saw_error = true;
                } else if element.is("close", NS_FRAMING) {
                    assert!(saw_error, "<close/> without a stream error for {frame:?}");
                    saw_close = true;
                } else {
                    assert!(
                        element.name() != "iq",
                        "not-well-formed input must not get a stanza error: {text}"
                    );
                }
            }
            Some(tungstenite::Message::Close(_)) | None => break,
            Some(_) => {}
        }
    }
    assert!(
        saw_error,
        "no <not-well-formed/> stream error for {frame:?}"
    );
    assert!(saw_close, "stream was not closed for {frame:?}");
}

#[tokio::test]
async fn unterminated_iq_gets_not_well_formed_stream_error() {
    assert_not_well_formed_closes_stream(
        r#"<iq id="broken-iq" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
    )
    .await;
}

#[tokio::test]
async fn mismatched_message_gets_not_well_formed_stream_error() {
    assert_not_well_formed_closes_stream(r#"<message to="localhost"><body>x</message>"#).await;
}

#[tokio::test]
async fn non_xml_frame_gets_not_well_formed_stream_error() {
    assert_not_well_formed_closes_stream("this is not xml").await;
}

async fn assert_iq_bad_request(iq: &str, id: &str) {
    let server = TestServer::start();
    let mut client = connect(&server).await;
    client.send(iq).await.expect("send IQ");
    let frame = client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("IQ response");
    let element: Element = frame
        .parse()
        .unwrap_or_else(|err| panic!("frame must parse as XML: {err}; frame={frame}"));
    assert_eq!(element.attr("type"), Some("error"), "{frame}");
    let error = element
        .get_child("error", "jabber:client")
        .unwrap_or_else(|| panic!("iq error element: {frame}"));
    assert_eq!(error.attr("type"), Some("modify"), "{frame}");
    assert!(
        error.get_child("bad-request", NS_XMPP_STANZAS).is_some(),
        "expected <bad-request/>: {frame}"
    );
    let _ = client.close().await;
}

#[tokio::test]
async fn iq_get_with_two_payloads_is_bad_request() {
    assert_iq_bad_request(
        r#"<iq type="get" id="two-payloads" to="localhost" xmlns="jabber:client"><ping xmlns="urn:xmpp:ping"/><ping xmlns="urn:xmpp:ping"/></iq>"#,
        "two-payloads",
    )
    .await;
}

#[tokio::test]
async fn iq_set_without_payload_is_bad_request() {
    assert_iq_bad_request(
        r#"<iq type="set" id="no-payload" to="localhost" xmlns="jabber:client"/>"#,
        "no-payload",
    )
    .await;
}
