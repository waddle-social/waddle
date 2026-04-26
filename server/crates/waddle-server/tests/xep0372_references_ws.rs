//! XEP-0372 reference integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0372-{}", uuid::Uuid::new_v4());
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
async fn references_route_and_replay_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reference-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reference-1">
                <body>Hello @admin</body>
                <reference xmlns="urn:xmpp:reference:0" type="mention" begin="6" end="12" uri="xmpp:admin@localhost"/>
            </message>"#
        ))
        .await
        .expect("send reference");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:reference:0"))
        .await
        .expect("reference echo");
    assert!(
        echo.contains("xmpp:admin@localhost"),
        "missing reference uri: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-reference" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-reference") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("urn:xmpp:reference:0")
                && frame.contains("xmpp:admin@localhost")),
        "MAM did not replay reference: {frames:?}"
    );

    client.close().await;
}

#[tokio::test]
async fn reference_without_required_attributes_returns_bad_request() {
    // XEP-0372: '<reference/>' MUST contain a 'type' and a 'uri'.
    // Server should reject malformed references with bad-request so
    // misbehaving clients don't poison archives with junk payloads.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reference-bad-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="bad-reference">
                <body>broken reference</body>
                <reference xmlns="urn:xmpp:reference:0" begin="0" end="6"/>
            </message>"#
        ))
        .await
        .expect("send malformed reference");
    let error = client
        .recv_matching(|frame| frame.contains("<bad-request"))
        .await
        .expect("bad-request error");
    assert!(
        error.contains("type=\"error\""),
        "not an error stanza: {error}"
    );

    client.close().await;
}
