//! XEP-0424 message retraction integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0424-{}", uuid::Uuid::new_v4());
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

fn stanza_id(frame: &str) -> String {
    extract_attr_after(frame, "stanza-id", "id").expect("stanza-id id")
}

#[tokio::test]
async fn retraction_routes_and_replays_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("retract-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-1"><body>remove me</body></message>"#
        ))
        .await
        .expect("send original");
    let target = stanza_id(
        &client
            .recv_matching(|frame| frame.contains("remove me"))
            .await
            .expect("original echo"),
    );

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="retract-1">
                <retract xmlns="urn:xmpp:message-retract:1" id="{target}"/>
                <body>/me retracted a previous message</body>
            </message>"#
        ))
        .await
        .expect("send retraction");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:message-retract:1"))
        .await
        .expect("retraction echo");
    assert!(echo.contains(&target), "missing retraction target: {echo}");

    client
        .send(&format!(
            r#"<iq type="set" id="mam-retract" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-retract") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("urn:xmpp:message-retract:1") && frame.contains(&target)),
        "MAM did not replay retraction: {frames:?}"
    );

    client.close().await;
}
