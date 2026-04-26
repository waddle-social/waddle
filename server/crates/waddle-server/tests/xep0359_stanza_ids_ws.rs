//! XEP-0359 stanza-id integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{disco_info_query, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0359-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("connect and auth");
    (server, client)
}

async fn join_room(client: &mut WsXmppClient, room: &str) {
    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

#[tokio::test]
async fn room_replaces_spoofed_room_stanza_id_and_preserves_origin_id() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("sid-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="client-msg-1">
                <body>sid body</body>
                <stanza-id xmlns="urn:xmpp:sid:0" id="spoofed" by="{room}"/>
                <origin-id xmlns="urn:xmpp:sid:0" id="origin-1"/>
            </message>"#
        ))
        .await
        .expect("send message");

    let echo = client
        .recv_matching(|frame| frame.contains("sid body"))
        .await
        .expect("echo");
    assert!(echo.contains("urn:xmpp:sid:0"), "echo missing sid: {echo}");
    assert!(echo.contains("origin-1"), "origin-id not preserved: {echo}");
    assert!(
        !echo.contains("spoofed"),
        "spoofed room stanza-id leaked: {echo}"
    );
    assert!(echo.contains(&format!("by=\"{room}\"")) || echo.contains(&format!("by='{room}'")));

    client.close().await;
}

#[tokio::test]
async fn server_disco_advertises_rich_message_features() {
    // The shared `server_features()` catalogue from `waddle-xmpp-core`
    // must be reflected by the live server-disco IQ path so clients
    // can discover XEP support. Without this, advertised behaviour
    // is invisible to clients (the inverse of the project's
    // "no advertise without behaviour" rule — equally bad).
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    let response = disco_info_query(&mut client, DOMAIN, "rich-disco-1")
        .await
        .expect("disco#info response");

    for ns in [
        "urn:xmpp:sid:0",
        "urn:xmpp:reply:0",
        "urn:xmpp:message-correct:0",
        "urn:xmpp:message-retract:1",
        "urn:xmpp:reactions:0",
        "urn:xmpp:reference:0",
        "urn:xmpp:fallback:0",
        "urn:xmpp:threads:0",
    ] {
        assert!(
            response.contains(ns),
            "server disco#info missing feature {ns}: {response}"
        );
    }

    client.close().await;
}
