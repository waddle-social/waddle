//! XEP-0308 message correction integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0308-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("connect and auth");
    (server, client)
}

async fn setup_with_bob() -> (TestServer, WsXmppClient, WsXmppClient) {
    let bob_password = format!("ws-test-bob-password-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let admin_resource = format!("xep0308-admin-{}", uuid::Uuid::new_v4());
    let admin_password = server.fixed_account_password().to_string();
    let admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &admin_password,
        &admin_resource,
    )
    .await
    .expect("connect admin");
    let bob_resource = format!("xep0308-bob-{}", uuid::Uuid::new_v4());
    let bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &bob_resource,
    )
    .await
    .expect("connect bob");
    (server, admin, bob)
}

async fn join_room(client: &mut WsXmppClient, room: &str) {
    join_room_as(client, room, USERNAME).await;
}

async fn join_room_as(client: &mut WsXmppClient, room: &str, nick: &str) {
    client
        .send(&format!(
            r#"<presence to="{room}/{nick}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

#[tokio::test]
async fn correction_routes_and_replays_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("correct-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-1"><body>typo</body></message>"#
        ))
        .await
        .expect("send original");
    client
        .recv_matching(|frame| frame.contains("typo"))
        .await
        .expect("original echo");
    let target = "orig-1";

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="fix-1">
                <body>fixed</body>
                <replace xmlns="urn:xmpp:message-correct:0" id="{target}"/>
            </message>"#
        ))
        .await
        .expect("send correction");
    let echo = client
        .recv_matching(|frame| frame.contains("fixed"))
        .await
        .expect("correction echo");
    assert!(
        echo.contains("urn:xmpp:message-correct:0"),
        "missing replace: {echo}"
    );
    assert!(echo.contains(&target), "missing correction target: {echo}");

    client
        .send(&format!(
            r#"<iq type="set" id="mam-correct" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-correct") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("urn:xmpp:message-correct:0") && frame.contains(&target)),
        "MAM did not replay correction: {frames:?}"
    );

    client.close().await;
}

#[tokio::test]
async fn correction_can_target_original_message_id() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("correct-id-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-client-id"><body>typo by id</body></message>"#
        ))
        .await
        .expect("send original");
    client
        .recv_matching(|frame| frame.contains("typo by id"))
        .await
        .expect("original echo");

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="fix-client-id">
                <body>fixed by id</body>
                <replace xmlns="urn:xmpp:message-correct:0" id="orig-client-id"/>
            </message>"#
        ))
        .await
        .expect("send correction");
    let echo = client
        .recv_matching(|frame| frame.contains("fixed by id"))
        .await
        .expect("correction echo");
    assert!(
        echo.contains("urn:xmpp:message-correct:0"),
        "missing replace: {echo}"
    );
    assert!(
        echo.contains("orig-client-id"),
        "missing correction target: {echo}"
    );

    client.close().await;
}

#[tokio::test]
async fn correction_without_target_id_returns_bad_request() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("correct-invalid-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="bad-correction">
                <body>fixed</body>
                <replace xmlns="urn:xmpp:message-correct:0"/>
            </message>"#
        ))
        .await
        .expect("send malformed correction");
    let error = client
        .recv_matching(|frame| frame.contains("<bad-request"))
        .await
        .expect("bad request error");
    assert!(
        error.contains("type=\"error\""),
        "not an error stanza: {error}"
    );

    client.close().await;
}

#[tokio::test]
async fn correction_from_different_occupant_returns_forbidden() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut admin, mut bob) = setup_with_bob().await;
    let room = format!("correct-forbidden-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room_as(&mut admin, &room, USERNAME).await;
    join_room_as(&mut bob, &room, "bob").await;

    admin
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-forbid"><body>owned by admin</body></message>"#
        ))
        .await
        .expect("send original");
    admin
        .recv_matching(|frame| frame.contains("owned by admin"))
        .await
        .expect("original echo");
    let target = "orig-forbid";

    bob.send(&format!(
        r#"<message type="groupchat" to="{room}" id="bad-owner">
                <body>not mine</body>
                <replace xmlns="urn:xmpp:message-correct:0" id="{target}"/>
            </message>"#
    ))
    .await
    .expect("send unauthorized correction");
    let error = bob
        .recv_matching(|frame| frame.contains("<forbidden"))
        .await
        .expect("forbidden error");
    assert!(
        error.contains("type=\"error\""),
        "not an error stanza: {error}"
    );

    admin.close().await;
    bob.close().await;
}
