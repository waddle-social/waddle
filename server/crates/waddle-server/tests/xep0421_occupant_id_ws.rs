//! XEP-0421 occupant-id wiring integration test over WebSocket.
//!
//! End-to-end coverage that the deployment-config secret loaded from
//! `WADDLE_OCCUPANT_ID_SECRET` (set by the test harness in
//! `ws_common/mod.rs`) reaches the actual stamping site, and that the
//! resulting `<occupant-id/>` matches the documented derivation
//! `HMAC_SHA256(secret, room_bare || 0x00 || user_bare)`.

mod ws_common;

use tokio::sync::Mutex;
use waddle_xmpp::xep::xep0421::{generate_occupant_id, OccupantIdSecret};
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

/// Must match `WADDLE_OCCUPANT_ID_SECRET` set by `ws_common/mod.rs`.
/// Co-locating it here ensures the assertion fails loudly if the harness
/// ever changes the secret without updating this test.
const HARNESS_SECRET: &str = "integration-test-occupant-id-secret-32-bytes-long";

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0421-{}", uuid::Uuid::new_v4());
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

fn extract_occupant_id_attr(frame: &str) -> Option<String> {
    // Stanzas come over the wire as text; cheap substring extraction
    // is sufficient for this assertion-only test.
    let needle = "<occupant-id";
    let start = frame.find(needle)?;
    let tail = &frame[start..];
    let id_attr = tail.find("id=")?;
    let after = &tail[id_attr + 3..];
    let quote = after.chars().next()?;
    let inner = &after[1..];
    let end = inner.find(quote)?;
    Some(inner[..end].to_string())
}

#[tokio::test]
async fn xep_0421_groupchat_reflection_carries_occupant_id_keyed_by_env_secret() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("oid-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="client-msg-1">
                <body>occupant-id wiring check</body>
            </message>"#
        ))
        .await
        .expect("send message");

    let echo = client
        .recv_matching(|frame| frame.contains("occupant-id wiring check"))
        .await
        .expect("echo");

    let stamped = extract_occupant_id_attr(&echo)
        .unwrap_or_else(|| panic!("reflected message must carry <occupant-id/>; got: {echo}"));

    let secret = OccupantIdSecret::new(HARNESS_SECRET.as_bytes().to_vec())
        .expect("harness secret meets length floor");
    let room_bare: jid::BareJid = room.parse().expect("room bare jid");
    let user_bare: jid::BareJid = format!("{USERNAME}@{DOMAIN}")
        .parse()
        .expect("user bare jid");
    let expected = generate_occupant_id(&user_bare, &room_bare, &secret);

    assert_eq!(
        stamped,
        expected.as_str(),
        "stamped occupant-id must match HMAC(env-secret, room || 0x00 || user); \
         echo={echo}"
    );

    let _ = client.close().await;
}
