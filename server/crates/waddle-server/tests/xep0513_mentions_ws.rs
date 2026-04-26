//! XEP-0513 explicit mention integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0513-{}", uuid::Uuid::new_v4());
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
async fn explicit_mentions_route_and_replay_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mentions-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="mention-1">
                <body>@admin please check this</body>
                <mention xmlns="urn:xmpp:mentions:0" begin="0" end="6" jid="admin@localhost"/>
            </message>"#
        ))
        .await
        .expect("send mention");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:mentions:0"))
        .await
        .expect("mention echo");
    assert!(
        echo.contains("admin@localhost"),
        "missing mentioned jid: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-mention" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-mention") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("urn:xmpp:mentions:0")
                && frame.contains("admin@localhost")),
        "MAM did not replay mention: {frames:?}"
    );

    client.close().await;
}

#[tokio::test]
async fn mention_without_target_attribute_returns_bad_request() {
    // XEP-0513: a '<mention/>' must identify its target via 'jid',
    // 'occupantid', or 'mentions' (group). Decorative mentions with
    // none of these are not interpretable by receivers and are
    // rejected with bad-request.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mention-bad-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="bad-mention">
                <body>broken mention</body>
                <mention xmlns="urn:xmpp:mentions:0" begin="0" end="6"/>
            </message>"#
        ))
        .await
        .expect("send malformed mention");
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
