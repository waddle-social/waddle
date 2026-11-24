//! RFC 6120 §8.4 / §10.3.3 unknown-namespace IQ conformance over WebSocket.
//!
//! "If the IQ stanza is of type "get" or "set" and the server does not
//! understand the namespace that qualifies the payload, the server MUST
//! return an error to the sending entity, which MUST be
//! <service-unavailable/>." XEP-0077 §3 adds: "If the host does not
//! support In-Band Registration, it MUST return a <service-unavailable/>
//! error." (xep-0077.xml:173). Waddle has no in-band registration, so
//! `jabber:iq:register` is an unknown namespace.

use waddle_ws_test_support as ws_common;

use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const NS_XMPP_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

fn iq_xml(iq_type: &str, id: &str, payload: Element) -> String {
    let iq = Element::builder("iq", "jabber:client")
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("type").to_owned(),
            iq_type,
        )
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("id").to_owned(),
            id,
        )
        .append(payload)
        .build();
    let mut buf = Vec::new();
    iq.write_to(&mut buf).expect("serialize IQ");
    String::from_utf8(buf).expect("minidom serializes UTF-8")
}

async fn assert_service_unavailable(iq_type: &str, id: &str, payload: Element) {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &password,
        &format!("unknown-ns-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection");

    client
        .send(&iq_xml(iq_type, id, payload))
        .await
        .expect("send IQ");
    let frame = client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("IQ response");
    let element = frame
        .parse::<Element>()
        .unwrap_or_else(|err| panic!("frame must parse as XML: {err}; frame={frame}"));
    assert_eq!(element.attr("type"), Some("error"), "{frame}");
    let error = element
        .get_child("error", "jabber:client")
        .unwrap_or_else(|| panic!("iq error element: {frame}"));
    assert_eq!(error.attr("type"), Some("cancel"), "{frame}");
    assert!(
        error
            .get_child("service-unavailable", NS_XMPP_STANZAS)
            .is_some(),
        "expected <service-unavailable/>: {frame}"
    );
    assert!(
        error
            .get_child("feature-not-implemented", NS_XMPP_STANZAS)
            .is_none(),
        "{frame}"
    );
    let _ = client.close().await;
}

#[tokio::test]
async fn unknown_namespace_get_to_server_is_service_unavailable() {
    assert_service_unavailable(
        "get",
        "unknown-ns-get",
        Element::builder("query", "urn:example:unknown:0").build(),
    )
    .await;
}

#[tokio::test]
async fn unknown_namespace_set_to_server_is_service_unavailable() {
    assert_service_unavailable(
        "set",
        "unknown-ns-set",
        Element::builder("thing", "urn:example:unknown:0").build(),
    )
    .await;
}

#[tokio::test]
async fn register_get_to_server_is_service_unavailable() {
    assert_service_unavailable(
        "get",
        "register-get",
        Element::builder("query", "jabber:iq:register").build(),
    )
    .await;
}
