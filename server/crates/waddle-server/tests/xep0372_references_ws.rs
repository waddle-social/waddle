//! XEP-0372 reference integration tests over WebSocket.

use waddle_ws_test_support as ws_common;

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

    let _ = client.close().await;
}

#[tokio::test]
async fn data_reference_with_anchor_routes_and_replays_from_mam() {
    // XEP-0372: type="data" references attach a URI annotation to a span of
    // body text — the standard wire shape for clickable links inside chat
    // messages. The server must route them unchanged and MAM must replay them.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reference-data-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reference-data-1">
                <body>see https://example.com</body>
                <reference xmlns="urn:xmpp:reference:0" type="data" begin="4" end="23" uri="https://example.com" anchor="https://example.com"/>
            </message>"#
        ))
        .await
        .expect("send data reference");
    let echo = client
        .recv_matching(|frame| {
            frame.contains("urn:xmpp:reference:0") && frame.contains("type='data'")
        })
        .await
        .expect("data reference echo");
    assert!(
        echo.contains("https://example.com"),
        "missing data reference uri: {echo}"
    );
    assert!(
        echo.contains("anchor='https://example.com'"),
        "missing anchor attribute: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-data-reference" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-data-reference") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames.iter().any(|frame| frame.contains("type='data'")
            && frame.contains("https://example.com")
            && frame.contains("anchor='https://example.com'")),
        "MAM did not replay data reference with anchor: {frames:?}"
    );

    let _ = client.close().await;
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
        error.contains("type='error'"),
        "not an error stanza: {error}"
    );

    let _ = client.close().await;
}
