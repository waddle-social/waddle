//! XEP-0461 reply integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0461-{}", uuid::Uuid::new_v4());
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
async fn reply_routes_and_replays_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reply-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-1"><body>question</body></message>"#
        ))
        .await
        .expect("send original");
    let target = stanza_id(
        &client
            .recv_matching(|frame| frame.contains("question"))
            .await
            .expect("original echo"),
    );

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reply-1">
                <body>answer</body>
                <reply xmlns="urn:xmpp:reply:0" to="{room}/{USERNAME}" id="{target}"/>
            </message>"#
        ))
        .await
        .expect("send reply");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:reply:0"))
        .await
        .expect("reply echo");
    assert!(echo.contains(&target), "missing reply target: {echo}");

    client
        .send(&format!(
            r#"<iq type="set" id="mam-reply" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-reply") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("urn:xmpp:reply:0") && frame.contains(&target)),
        "MAM did not replay reply: {frames:?}"
    );

    client.close().await;
}
