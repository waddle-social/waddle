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
        .recv_matching(|frame| frame_has_iq_id(frame, id) && frame.contains("<iq"))
        .await
        .expect("iq get response")
}

async fn iq_set_to(client: &mut WsXmppClient, id: &str, to: &str, payload: Element) -> String {
    let iq = Iq::Set {
        from: None,
        to: Some(to.parse().expect("valid iq destination")),
        id: id.to_string(),
        payload,
    };
    client
        .send(&stanza_to_xml(&Stanza::Iq(Box::new(iq))))
        .await
        .expect("send iq set");
    client
        .recv_matching(|frame| frame_has_iq_id(frame, id) && frame.contains("<iq"))
        .await
        .expect("iq set response")
}

fn frame_has_iq_id(frame: &str, id: &str) -> bool {
    frame.contains(&format!(r#"id='{id}'"#)) || frame.contains(&format!(r#"id="{id}""#))
}

#[tokio::test]
async fn spaces_service_disco_info_advertises_xep0503_features() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-service-info").await;

    let info = iq_get_to(
        &mut admin,
        "spaces-info",
        "spaces.localhost",
        Element::builder("query", "http://jabber.org/protocol/disco#info").build(),
    )
    .await;

    for feature in [
        "urn:xmpp:spaces:0",
        "http://jabber.org/protocol/pubsub#retrieve-items",
        "http://jabber.org/protocol/pubsub#multi-items",
        "http://jabber.org/protocol/pubsub#manage-subscriptions",
        "http://jabber.org/protocol/pubsub#modify-affiliations",
        "http://jabber.org/protocol/pubsub#retrieve-affiliations",
    ] {
        assert!(
            info.contains(feature),
            "Spaces service disco#info missing {feature}: {info}"
        );
    }
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_owner_subscriptions_round_trip() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-owner-subs").await;

    let set = iq_set_to(
        &mut admin,
        "spaces-subs-set",
        "spaces.localhost",
        Element::builder("pubsub", "http://jabber.org/protocol/pubsub#owner")
            .append(
                Element::builder("subscriptions", "http://jabber.org/protocol/pubsub#owner")
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), "general")
                    .append(
                        Element::builder("subscription", "http://jabber.org/protocol/pubsub#owner")
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "alice@localhost",
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("subscription").to_owned(),
                                "subscribed",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    )
    .await;
    assert!(
        set.contains("type='result'") || set.contains("type=\"result\""),
        "owner subscriptions set should succeed: {set}"
    );

    let get = iq_get_to(
        &mut admin,
        "spaces-subs-get",
        "spaces.localhost",
        Element::builder("pubsub", "http://jabber.org/protocol/pubsub#owner")
            .append(
                Element::builder("subscriptions", "http://jabber.org/protocol/pubsub#owner")
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), "general")
                    .build(),
            )
            .build(),
    )
    .await;
    assert!(
        get.contains("alice@localhost") && get.contains("subscription='subscribed'"),
        "owner subscriptions get should include added subscriber: {get}"
    );
    let _ = admin.close().await;
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
