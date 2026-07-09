//! XEP-0424 message retraction integration tests over WebSocket.

use waddle_ws_test_support as ws_common;

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

#[tokio::test]
async fn xep_0424_groupchat_retraction_round_trip_succeeds() {
    // L4 wire-trace coverage: alice sends a groupchat message, the room
    // reflects it with a `<stanza-id by='room'/>`, and alice retracts
    // citing that **room-assigned XEP-0359 stanza-id** — exactly what a
    // conformant XEP-0424 client does (xep-0424.xml lines 158, 230-232),
    // including waddle's own chat frontend (`replyableId = stampedByRoom`).
    // The retraction MUST be reflected (proving the validator resolved
    // the target instead of returning `<item-not-found/>`), and a
    // `<retracted/>` tombstone MUST replace the original row in MAM
    // (XEP-0424 §"prevent further distribution").
    //
    // Regression: the groupchat retraction lookup keyed off the wire `id`
    // attribute (the `stanza_id` column), so a conformant retraction
    // citing the room stanza-id returned `<item-not-found/>` and channel
    // deletes silently did nothing.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("retract-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    let original_id = "orig-1";
    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="{original_id}"><body>remove me</body></message>"#
        ))
        .await
        .expect("send original");
    let original_echo = client
        .recv_matching(|frame| frame.contains("remove me"))
        .await
        .expect("original echo");
    // The id a conformant client retracts by: the room's XEP-0359 stamp.
    let room_stanza_id = extract_attr_after(&original_echo, "stanza-id", "id")
        .expect("room reflection must carry a <stanza-id by='room'/>");
    assert_ne!(
        room_stanza_id, original_id,
        "room stanza-id must be distinct from the wire id so the test exercises the real path"
    );

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="retract-1">
                <retract xmlns="urn:xmpp:message-retract:1" id="{room_stanza_id}"/>
                <body>/me retracted a previous message</body>
            </message>"#
        ))
        .await
        .expect("send retraction");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:message-retract:1"))
        .await
        .expect("retraction echo");
    assert!(
        echo.contains(&format!("id='{room_stanza_id}'")),
        "retraction echo must cite the room stanza-id: {echo}"
    );
    assert!(
        !echo.contains("<error"),
        "successful retraction must not be an error stanza: {echo}"
    );

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
    let retraction_event = frames
        .iter()
        .find(|frame| {
            frame.contains("<retract ") && frame.contains(&format!("id='{room_stanza_id}'"))
        })
        .unwrap_or_else(|| {
            panic!("MAM did not replay retraction event citing the room stanza-id: {frames:?}")
        });
    let retraction_archive_id = extract_attr_after(retraction_event, "stanza-id", "id")
        .expect("archived retraction event has a canonical room stanza-id");
    assert_ne!(
        retraction_archive_id, "retract-1",
        "the fenced archive proof uses the room stanza-id, while the tombstone cites wire id"
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
        tombstone.contains("<retracted") && tombstone.contains("id='retract-1'"),
        "tombstone must cite the retraction stanza id: {tombstone}"
    );
    assert!(
        frames
            .iter()
            .all(|frame| !frame.contains("<body>remove me</body>")),
        "original body must not appear in any MAM result after retraction: {frames:?}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn groupchat_retraction_rejects_reused_nickname_generation() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("retract-generation-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="old-generation"><body>old occupancy</body></message>"#
        ))
        .await
        .expect("send original");
    let original_echo = client
        .recv_matching(|frame| frame.contains("old occupancy"))
        .await
        .expect("original echo");
    let target = extract_attr_after(&original_echo, "stanza-id", "id").expect("room stanza-id");

    client
        .send(&format!(
            r#"<presence type="unavailable" to="{room}/{USERNAME}"/>"#
        ))
        .await
        .expect("leave old occupancy");
    client
        .recv_matching(|frame| frame.contains("type='unavailable'") && frame.contains(&room))
        .await
        .expect("self unavailable");
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="cross-generation-retract">
                <retract xmlns="urn:xmpp:message-retract:1" id="{target}"/>
                <body>/me tried to retract a previous occupancy</body>
            </message>"#
        ))
        .await
        .expect("send stale-generation retraction");
    let error = client
        .recv_matching(|frame| {
            frame.contains("cross-generation-retract") && frame.contains("<error")
        })
        .await
        .expect("retraction error");
    assert!(
        error.contains("<forbidden"),
        "nick reuse must not authorize a prior occupancy's message: {error}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-generation" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("query MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-generation") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames.iter().any(|frame| frame.contains("old occupancy")),
        "rejected cross-generation retraction must leave the original intact: {frames:?}"
    );

    let _ = client.close().await;
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
    let bob_tombstone = bob_frames
        .iter()
        .find(|frame| frame.contains("<retracted "))
        .unwrap_or_else(|| {
            panic!("recipient archive missing tombstone for retracted DM: {bob_frames:?}")
        });
    assert!(
        bob_tombstone.contains("id='dm-retract-1'"),
        "recipient tombstone must cite the retraction stanza id: {bob_tombstone}"
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
    let admin_tombstone = admin_frames
        .iter()
        .find(|frame| frame.contains("<retracted "))
        .unwrap_or_else(|| {
            panic!("sender archive missing tombstone for retracted DM: {admin_frames:?}")
        });
    assert!(
        admin_tombstone.contains("id='dm-retract-1'"),
        "sender tombstone must cite the retraction stanza id: {admin_tombstone}"
    );

    let _ = bob.close().await;
    let _ = admin.close().await;
}
