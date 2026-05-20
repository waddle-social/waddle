//! XEP-0503 Spaces wire-conformance integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use waddle_xmpp::Stanza;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::{iq::Iq, minidom::Element};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, resource)
        .await
        .expect("admin connect")
}

fn stanza_to_xml(stanza: &Stanza) -> String {
    let mut buf = Vec::new();
    stanza
        .to_element()
        .write_to(&mut buf)
        .expect("serializing stanza to Vec<u8> should not fail");
    String::from_utf8(buf).expect("xmpp_parsers serializes valid UTF-8")
}

async fn iq_get_to(client: &mut WsXmppClient, id: &str, to: &str, payload: Element) -> String {
    let iq = Iq::Get {
        from: None,
        to: Some(to.parse().expect("valid iq destination")),
        id: id.to_string(),
        payload,
    };
    client
        .send(&stanza_to_xml(&Stanza::Iq(Box::new(iq))))
        .await
        .expect("send iq get");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("iq get response")
}

#[tokio::test]
async fn seeded_general_space_lists_bookmarked_rooms() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-seeded").await;

    let spaces = iq_get_to(
        &mut admin,
        "spaces-items",
        "spaces.localhost",
        Element::builder("query", "http://jabber.org/protocol/disco#items").build(),
    )
    .await;
    assert!(
        spaces.contains("node='general'"),
        "expected seeded general space node: {spaces}"
    );

    let items = iq_get_to(
        &mut admin,
        "general-items",
        "spaces.localhost",
        Element::builder("pubsub", "http://jabber.org/protocol/pubsub")
            .append(
                Element::builder("items", "http://jabber.org/protocol/pubsub")
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), "general")
                    .build(),
            )
            .build(),
    )
    .await;
    assert!(
        items.contains("chat@muc.localhost") && items.contains("announcements@muc.localhost"),
        "expected seeded room bookmarks: {items}"
    );
    assert!(
        items.contains("conference") && items.contains("urn:xmpp:bookmarks:1"),
        "expected XEP-0402 conference bookmark items: {items}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn room_disco_advertises_parent_space_metadata() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-room-disco").await;

    let info = iq_get_to(
        &mut admin,
        "chat-info",
        "chat@muc.localhost",
        Element::builder("query", "http://jabber.org/protocol/disco#info").build(),
    )
    .await;

    assert!(
        info.contains("var='urn:xmpp:spaces:0'"),
        "expected XEP-0503 feature: {info}"
    );
    assert!(
        info.contains("<value>urn:xmpp:spaces:0</value>")
            && info.contains("var='parent'")
            && info.contains("xmpp:spaces.localhost?;node=general"),
        "expected XEP-0503 parent data form: {info}"
    );
    assert!(
        info.contains("http://jabber.org/protocol/muc#roominfo")
            && info.contains("muc#roomconfig_pubsub")
            && info.contains("xmpp:spaces.localhost?;node=general"),
        "expected MUC roominfo pubsub compatibility field: {info}"
    );
    let _ = admin.close().await;
}
