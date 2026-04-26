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

    // The retraction message itself is archived as its own row with a
    // live `<retract>` payload — that's the "I retracted X" timeline
    // entry clients render.
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("<retract ") && frame.contains(&target)),
        "MAM did not replay retraction event: {frames:?}"
    );

    // XEP-0424 §"prevent further distribution… by replacing the
    // original message with a tombstone": the original row's body
    // must not appear in MAM, and a `<retracted/>` tombstone must
    // take its place.
    let tombstone = frames
        .iter()
        .find(|frame| frame.contains("<retracted "))
        .unwrap_or_else(|| panic!("MAM missing tombstone for retracted message: {frames:?}"));
    assert!(
        !tombstone.contains("<body>remove me</body>"),
        "tombstone leaked original body: {tombstone}"
    );
    assert!(
        frames
            .iter()
            .all(|frame| !frame.contains("<body>remove me</body>")),
        "original body must not appear in any MAM result after retraction: {frames:?}"
    );

    client.close().await;
}

#[tokio::test]
async fn dm_retraction_tombstones_both_archives() {
    // XEP-0424 §"prevent further distribution" applies to both
    // archives that hold a 1:1 message — the sender's and the
    // recipient's. The retraction is sent via the sender's path; the
    // recipient's archive must independently observe the tombstone.
    let _guard = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-password-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let admin_resource = format!("xep0424-admin-{}", uuid::Uuid::new_v4());
    let admin_password = server.fixed_account_password().to_string();
    let mut admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &admin_password,
        &admin_resource,
    )
    .await
    .expect("connect admin");
    let bob_resource = format!("xep0424-bob-{}", uuid::Uuid::new_v4());
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &bob_resource,
    )
    .await
    .expect("connect bob");

    let original_id = format!("dm-{}", uuid::Uuid::new_v4());
    admin
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@{DOMAIN}" id="{original_id}"><body>retract this dm</body></message>"#
        ))
        .await
        .expect("send DM");
    bob.recv_matching(|frame| frame.contains("retract this dm"))
        .await
        .expect("bob receives DM");

    admin
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@{DOMAIN}" id="dm-retract-1">
                <retract xmlns="urn:xmpp:message-retract:1" id="{original_id}"/>
                <body>/me retracted a previous message</body>
            </message>"#
        ))
        .await
        .expect("send retraction");
    bob.recv_matching(|frame| frame.contains("urn:xmpp:message-retract:1"))
        .await
        .expect("bob receives retraction");

    // Bob queries his own MAM and must not see the original body.
    bob.send(
        r#"<iq type="set" id="bob-mam" to="bob@localhost"><query xmlns="urn:xmpp:mam:2"/></iq>"#,
    )
    .await
    .expect("bob MAM query");
    let bob_frames = bob
        .recv_until(|frame| frame.contains("bob-mam") && frame.contains("<fin"))
        .await
        .expect("bob MAM frames");
    assert!(
        bob_frames
            .iter()
            .all(|frame| !frame.contains("<body>retract this dm</body>")),
        "recipient archive leaked original body after retraction: {bob_frames:?}"
    );
    assert!(
        bob_frames.iter().any(|frame| frame.contains("<retracted ")),
        "recipient archive missing tombstone for retracted DM: {bob_frames:?}"
    );

    // Admin's MAM also tombstones.
    admin
        .send(
            r#"<iq type="set" id="admin-mam" to="admin@localhost"><query xmlns="urn:xmpp:mam:2"/></iq>"#,
        )
        .await
        .expect("admin MAM query");
    let admin_frames = admin
        .recv_until(|frame| frame.contains("admin-mam") && frame.contains("<fin"))
        .await
        .expect("admin MAM frames");
    assert!(
        admin_frames
            .iter()
            .all(|frame| !frame.contains("<body>retract this dm</body>")),
        "sender archive leaked original body after retraction: {admin_frames:?}"
    );
    assert!(
        admin_frames
            .iter()
            .any(|frame| frame.contains("<retracted ")),
        "sender archive missing tombstone for retracted DM: {admin_frames:?}"
    );

    bob.close().await;
    admin.close().await;
}
