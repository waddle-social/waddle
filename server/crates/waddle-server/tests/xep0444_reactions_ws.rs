//! XEP-0444 reaction integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0444-{}", uuid::Uuid::new_v4());
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
async fn reaction_routes_and_replays_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("react-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-1"><body>react here</body></message>"#
        ))
        .await
        .expect("send original");
    let target = stanza_id(
        &client
            .recv_matching(|frame| frame.contains("react here"))
            .await
            .expect("original echo"),
    );

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reaction-1">
                <reactions xmlns="urn:xmpp:reactions:0" id="{target}">
                    <reaction>👍</reaction>
                </reactions>
            </message>"#
        ))
        .await
        .expect("send reaction");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:reactions:0"))
        .await
        .expect("reaction echo");
    assert!(echo.contains(&target), "missing reaction target: {echo}");

    client
        .send(&format!(
            r#"<iq type="set" id="mam-react" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-react") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("urn:xmpp:reactions:0") && frame.contains(&target)),
        "MAM did not replay reaction: {frames:?}"
    );

    client.close().await;
}
