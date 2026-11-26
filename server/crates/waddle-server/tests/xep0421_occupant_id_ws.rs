//! XEP-0421 occupant-id wiring integration test over WebSocket.
//!
//! End-to-end coverage that the deployment-config secret loaded from
//! `WADDLE_OCCUPANT_ID_SECRET` (set by the test harness in
//! `ws_common/mod.rs`) reaches the actual stamping site, and that the
//! resulting `<occupant-id/>` matches the documented derivation
//! `HMAC_SHA256(secret, room_bare || 0x00 || user_bare)`.

use waddle_ws_test_support as ws_common;

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

    let secret = OccupantIdSecret::try_new(HARNESS_SECRET.as_bytes().to_vec())
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

fn find_descendant<'a>(
    element: &'a xmpp_parsers::minidom::Element,
    name: &str,
    ns: &str,
) -> Option<&'a xmpp_parsers::minidom::Element> {
    element.children().find_map(|child| {
        if child.name() == name && child.ns() == ns {
            Some(child)
        } else {
            find_descendant(child, name, ns)
        }
    })
}

fn room_occupant_id(room: &str) -> String {
    let secret = OccupantIdSecret::try_new(HARNESS_SECRET.as_bytes().to_vec())
        .expect("harness secret meets length floor");
    let room: jid::BareJid = room.parse().expect("room bare jid");
    waddle_xmpp::xep::xep0421::generate_room_occupant_id(&room, &secret)
        .as_str()
        .to_owned()
}

#[tokio::test]
async fn xep_0421_muc_service_advertises_occupant_id_because_every_room_forces_it() {
    // XEP-0421 §Business Rules: "If occupant identifiers are force enabled
    // for all rooms on a MUC service, it SHOULD additionally specify the
    // 'urn:xmpp:occupant-id:0' feature on the MUC service."
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    client
        .send(&format!(
            r#"<iq type="get" id="svc-disco" to="muc.{DOMAIN}"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#
        ))
        .await
        .expect("send service disco");
    let service = client
        .recv_matching(|frame| frame.contains("svc-disco"))
        .await
        .expect("service disco result");
    assert!(
        service.contains("var='urn:xmpp:occupant-id:0'")
            || service.contains(r#"var="urn:xmpp:occupant-id:0""#),
        "MUC service must advertise urn:xmpp:occupant-id:0: {service}"
    );

    let room = format!("oid-disco-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;
    client
        .send(&format!(
            r#"<iq type="get" id="room-disco" to="{room}"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#
        ))
        .await
        .expect("send room disco");
    let room_disco = client
        .recv_matching(|frame| frame.contains("room-disco"))
        .await
        .expect("room disco result");
    assert!(
        room_disco.contains("var='urn:xmpp:occupant-id:0'")
            || room_disco.contains(r#"var="urn:xmpp:occupant-id:0""#),
        "room must advertise urn:xmpp:occupant-id:0: {room_disco}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn xep_0421_room_authored_messages_carry_the_room_occupant_id() {
    // XEP-0421 §Business Rules: "The <occupant-id> element MUST be attached
    // to every message and every presence sent by a MUC." The empty subject
    // of a never-set room and a pin notice are authored by the room itself.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("oid-room-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let expected = room_occupant_id(&room);

    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    let join = client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
    let subject = join
        .iter()
        .find(|frame| frame.contains("<subject"))
        .expect("subject frame");
    assert_eq!(
        extract_occupant_id_attr(subject).as_deref(),
        Some(expected.as_str()),
        "never-set subject carries the room occupant-id: {subject}"
    );

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="pin-target"><body>pin me</body></message>"#
        ))
        .await
        .expect("send target");
    let echo = client
        .recv_matching(|frame| frame.contains("pin me"))
        .await
        .expect("target echo");
    let target = ws_common::extract_attr_after(&echo, "stanza-id", "id").expect("stanza-id");
    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="pin-req"><pinned xmlns="urn:waddle:pin:0" target="{target}"/></message>"#
        ))
        .await
        .expect("send pin");
    let pin = client
        .recv_matching(|frame| frame.contains("<pin-event") && frame.contains("action='pinned'"))
        .await
        .expect("pin notice");
    let element = pin.parse::<xmpp_parsers::minidom::Element>().expect("xml");
    assert!(
        element.attr("from").is_some_and(|from| !from.contains('/')),
        "pin notice is authored by the bare room: {pin}"
    );
    assert_eq!(
        pin.matches("<occupant-id").count(),
        1,
        "pin notice carries exactly one occupant-id: {pin}"
    );
    assert!(find_descendant(&element, "occupant-id", "urn:xmpp:occupant-id:0").is_some());
    assert_eq!(
        extract_occupant_id_attr(&pin).as_deref(),
        Some(expected.as_str()),
        "pin notice carries the room occupant-id: {pin}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="oid-mam" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("oid-mam") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    let replay = frames
        .iter()
        .find(|frame| frame.contains("<pin-event"))
        .unwrap_or_else(|| panic!("MAM replays the pin notice: {frames:?}"));
    assert_eq!(
        replay.matches("<occupant-id").count(),
        1,
        "replayed pin notice carries exactly one occupant-id: {replay}"
    );
    assert_eq!(
        extract_occupant_id_attr(replay).as_deref(),
        Some(expected.as_str()),
        "replayed pin notice keeps the room occupant-id: {replay}"
    );

    let _ = client.close().await;
}
