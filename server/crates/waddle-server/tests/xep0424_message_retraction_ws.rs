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
    assert!(
        frames.iter().any(|frame| frame.contains("<retract ")
            && frame.contains(&format!("id='{room_stanza_id}'"))),
        "MAM did not replay retraction event citing the room stanza-id: {frames:?}"
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

const REASON: &str = "Sent to the wrong person";
const ORIGINAL_TEXT: &str = "secret-text-v1";
const CORRECTION_TEXTS: [&str; 2] = ["secret-text-v2", "secret-text-v3"];

/// XEP-0424 §Tombstones (xep-0424.xml line 307): the archive replaces the
/// original and each of its XEP-0308 corrections with a tombstone, the
/// tombstone of a correction keeps its `<replace/>`, and the retraction's
/// `<reason/>` is copied into each tombstone (line 276).
fn assert_corrections_tombstoned(frames: &[String], original_id: &str, retraction_id: &str) {
    let tombstones: Vec<&String> = frames
        .iter()
        .filter(|frame| frame.contains("<retracted "))
        .collect();
    assert_eq!(
        tombstones.len(),
        3,
        "original and both corrections must be tombstoned: {frames:?}"
    );
    let correction_tombstones: Vec<&&String> = tombstones
        .iter()
        .filter(|frame| frame.contains("urn:xmpp:message-correct:0"))
        .collect();
    assert_eq!(
        correction_tombstones.len(),
        2,
        "exactly the two correction tombstones keep <replace/>: {tombstones:?}"
    );
    for tombstone in &tombstones {
        assert!(
            tombstone.contains(&format!("id='{retraction_id}'")),
            "every tombstone carries the retraction id: {tombstone}"
        );
        assert!(
            tombstone.contains(&format!("<reason>{REASON}</reason>")),
            "every tombstone copies the retraction reason: {tombstone}"
        );
        assert!(
            !tombstone.contains("<body"),
            "tombstone must not carry a body: {tombstone}"
        );
    }
    for tombstone in correction_tombstones {
        assert!(
            tombstone.contains("<replace xmlns='urn:xmpp:message-correct:0'")
                && tombstone.contains(&format!("message-correct:0' id='{original_id}'/>")),
            "correction tombstone must keep <replace id> of the original: {tombstone}"
        );
    }
    for text in std::iter::once(ORIGINAL_TEXT).chain(CORRECTION_TEXTS) {
        assert!(
            frames.iter().all(|frame| !frame.contains(text)),
            "retracted text {text:?} must not appear anywhere in the MAM result: {frames:?}"
        );
    }
}

/// Connect `admin` and `bob` to a fresh server.
async fn connect_admin_and_bob() -> (TestServer, WsXmppClient, WsXmppClient) {
    let bob_password = format!("ws-test-bob-password-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let admin_password = server.fixed_account_password().to_string();
    let admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &admin_password,
        &format!("xep0424-admin-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect admin");
    let bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("xep0424-bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect bob");
    (server, admin, bob)
}

/// Query the whole archive at `archive` and return every frame up to `<fin>`.
async fn query_mam(client: &mut WsXmppClient, archive: &str, query_id: &str) -> Vec<String> {
    client
        .send(&format!(
            r#"<iq type="set" id="{query_id}" to="{archive}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    client
        .recv_until(|frame| frame.contains(query_id) && frame.contains("<fin"))
        .await
        .expect("MAM frames")
}

/// Send each of [`CORRECTION_TEXTS`] as a correction of `original_id` using
/// `message_attrs` for the `<message>` element, waiting for each to be delivered
/// to `receiver` (the sender itself when `None`).
async fn send_corrections(
    sender: &mut WsXmppClient,
    mut receiver: Option<&mut WsXmppClient>,
    message_attrs: &str,
    original_id: &str,
) {
    for (index, text) in CORRECTION_TEXTS.iter().enumerate() {
        sender
            .send(&format!(
                r#"<message {message_attrs} id="fix-{index}">
                    <body>{text}</body>
                    <replace xmlns="urn:xmpp:message-correct:0" id="{original_id}"/>
                </message>"#
            ))
            .await
            .expect("send correction");
        let waiting = receiver.as_deref_mut().unwrap_or(&mut *sender);
        waiting
            .recv_matching(|frame| frame.contains(text))
            .await
            .expect("correction delivered");
    }
}

/// A correction of a retracted message is refused with `<item-not-found/>`.
async fn assert_late_correction_rejected(
    client: &mut WsXmppClient,
    message_attrs: &str,
    original_id: &str,
) {
    client
        .send(&format!(
            r#"<message {message_attrs} id="fix-late">
                <body>secret-text-late</body>
                <replace xmlns="urn:xmpp:message-correct:0" id="{original_id}"/>
            </message>"#
        ))
        .await
        .expect("send late correction");
    let error = client
        .recv_matching(|frame| frame.contains("<item-not-found"))
        .await
        .expect("late correction must be rejected");
    assert!(
        error.contains("type='error'"),
        "not an error stanza: {error}"
    );
}

#[tokio::test]
async fn groupchat_retraction_tombstones_corrections_with_reason() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("retract-corr-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    let original_id = "orig-corrected";
    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="{original_id}"><body>{ORIGINAL_TEXT}</body></message>"#
        ))
        .await
        .expect("send original");
    let original_echo = client
        .recv_matching(|frame| frame.contains(ORIGINAL_TEXT))
        .await
        .expect("original echo");
    let room_stanza_id = extract_attr_after(&original_echo, "stanza-id", "id")
        .expect("room reflection must carry a <stanza-id by='room'/>");

    let attrs = format!(r#"type="groupchat" to="{room}""#);
    send_corrections(&mut client, None, &attrs, original_id).await;
    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="retract-corr">
                <retract xmlns="urn:xmpp:message-retract:1" id="{room_stanza_id}">
                    <reason>{REASON}</reason>
                </retract>
                <body>/me retracted a previous message</body>
            </message>"#
        ))
        .await
        .expect("send retraction");
    client
        .recv_matching(|frame| frame.contains("urn:xmpp:message-retract:1"))
        .await
        .expect("retraction echo");

    let frames = query_mam(&mut client, &room, "mam-corr").await;
    assert_corrections_tombstoned(&frames, original_id, "retract-corr");
    // The original's tombstone is the one without a <replace/>.
    assert!(
        frames
            .iter()
            .filter(|frame| frame.contains("<retracted "))
            .any(|frame| !frame.contains("urn:xmpp:message-correct:0")),
        "the original must have its own tombstone without <replace/>: {frames:?}"
    );

    assert_late_correction_rejected(&mut client, &attrs, original_id).await;
    let frames = query_mam(&mut client, &room, "mam-late").await;
    assert!(
        frames
            .iter()
            .all(|frame| !frame.contains("secret-text-late")),
        "rejected late correction must not be archived: {frames:?}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn dm_retraction_tombstones_corrections_with_reason_in_both_archives() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut admin, mut bob) = connect_admin_and_bob().await;

    let original_id = format!("dm-corr-{}", uuid::Uuid::new_v4());
    admin
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@{DOMAIN}" id="{original_id}"><body>{ORIGINAL_TEXT}</body></message>"#
        ))
        .await
        .expect("send DM");
    bob.recv_matching(|frame| frame.contains(ORIGINAL_TEXT))
        .await
        .expect("bob receives DM");
    let attrs = format!(r#"xmlns="jabber:client" type="chat" to="bob@{DOMAIN}""#);
    send_corrections(&mut admin, Some(&mut bob), &attrs, &original_id).await;

    admin
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@{DOMAIN}" id="dm-retract-corr">
                <retract xmlns="urn:xmpp:message-retract:1" id="{original_id}">
                    <reason>{REASON}</reason>
                </retract>
                <body>/me retracted a previous message</body>
            </message>"#
        ))
        .await
        .expect("send retraction");
    bob.recv_matching(|frame| frame.contains("urn:xmpp:message-retract:1"))
        .await
        .expect("bob receives retraction");

    let bob_frames = query_mam(&mut bob, &format!("bob@{DOMAIN}"), "bob-mam-corr").await;
    assert_corrections_tombstoned(&bob_frames, &original_id, "dm-retract-corr");
    let admin_frames = query_mam(&mut admin, &format!("admin@{DOMAIN}"), "admin-mam-corr").await;
    assert_corrections_tombstoned(&admin_frames, &original_id, "dm-retract-corr");

    assert_late_correction_rejected(&mut admin, &attrs, &original_id).await;

    let _ = bob.close().await;
    let _ = admin.close().await;
}
