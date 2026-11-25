//! XEP-0425 moderated retraction integration tests over WebSocket.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0425-{}", uuid::Uuid::new_v4());
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

fn find_descendant<'a>(element: &'a Element, name: &str, ns: &str) -> Option<&'a Element> {
    for child in element.children() {
        if child.name() == name && child.ns() == ns {
            return Some(child);
        }
        if let Some(found) = find_descendant(child, name, ns) {
            return Some(found);
        }
    }
    None
}

fn find_moderation_message(element: &Element) -> Option<&Element> {
    if element.name() == "message"
        && element.ns() == "jabber:client"
        && element
            .get_child("retract", "urn:xmpp:message-retract:1")
            .is_some()
    {
        return Some(element);
    }
    for child in element.children() {
        if let Some(message) = find_moderation_message(child) {
            return Some(message);
        }
    }
    None
}

fn assert_moderation_shape(frame: &str, target: &str) {
    let element = frame.parse::<Element>().expect("valid XML frame");
    let message = find_moderation_message(&element)
        .unwrap_or_else(|| panic!("missing message carrying moderation payload: {frame}"));
    assert!(
        message.attr("from").is_some_and(|from| !from.contains("/")),
        "moderation broadcast must be from bare room jid: {frame}"
    );
    assert!(
        find_descendant(message, "apply-to", "urn:xmpp:fasten:0").is_none(),
        "old fastening moderation shape leaked: {frame}"
    );
    assert!(
        find_descendant(message, "retracted", "urn:xmpp:message-retract:1").is_none(),
        "tombstone-only retracted element leaked into live moderation: {frame}"
    );

    let retract = find_descendant(message, "retract", "urn:xmpp:message-retract:1")
        .unwrap_or_else(|| panic!("missing retract payload: {frame}"));
    assert_eq!(retract.attr("id"), Some(target), "wrong retract target");
    let moderated = retract
        .get_child("moderated", "urn:xmpp:message-moderate:1")
        .unwrap_or_else(|| panic!("missing moderated child: {frame}"));
    assert!(
        moderated.attr("by").is_some_and(|by| by.contains("/admin")),
        "missing moderator occupant jid: {frame}"
    );
    let reason = retract
        .get_child("reason", "urn:xmpp:message-retract:1")
        .unwrap_or_else(|| panic!("missing retract reason: {frame}"));
    assert_eq!(reason.text(), "cleanup");
}

#[tokio::test]
async fn moderation_broadcasts_and_replays_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("moderate-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-1"><body>moderate me</body></message>"#
        ))
        .await
        .expect("send original");
    let target = stanza_id(
        &client
            .recv_matching(|frame| frame.contains("moderate me"))
            .await
            .expect("original echo"),
    );

    client
        .send(&format!(
            r#"<iq type="set" id="moderate-1" to="{room}">
                <moderate xmlns="urn:xmpp:message-moderate:1" id="{target}">
                    <retract xmlns="urn:xmpp:message-retract:1"/>
                    <reason>cleanup</reason>
                </moderate>
            </iq>"#
        ))
        .await
        .expect("send moderation");
    let frames = client
        .recv_until(|frame| frame.contains("moderate-1") && frame.contains("type='result'"))
        .await
        .expect("moderation frames");
    let broadcast = frames
        .iter()
        .find(|frame| frame.contains("urn:xmpp:message-moderate:1"))
        .unwrap_or_else(|| panic!("missing moderation broadcast: {frames:?}"));
    assert_moderation_shape(broadcast, &target);

    client
        .send(&format!(
            r#"<iq type="set" id="mam-moderate" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-moderate") && frame.contains("<fin"))
        .await
        .expect("MAM frames");

    // The room archive now contains TWO rows for this moderation:
    // 1. the original message replaced in place with a `<retracted>`
    //    tombstone wrapping the moderator annotation, and
    // 2. the moderation result message itself with a live `<retract>`
    //    payload — which clients can use as the "moderation event"
    //    timeline entry.
    let live = frames
        .iter()
        .find(|frame| {
            frame.contains("<retract ")
                && !frame.contains("<retracted ")
                && frame.contains("urn:xmpp:message-moderate:1")
        })
        .unwrap_or_else(|| panic!("MAM did not replay live moderation event: {frames:?}"));
    assert_moderation_shape(live, &target);

    let tombstone = frames
        .iter()
        .find(|frame| {
            frame.contains("<retracted ") && frame.contains("urn:xmpp:message-moderate:1")
        })
        .unwrap_or_else(|| panic!("MAM did not replay moderation tombstone: {frames:?}"));
    let tombstone_element = tombstone.parse::<Element>().expect("valid tombstone XML");
    let retracted = find_descendant(
        &tombstone_element,
        "retracted",
        "urn:xmpp:message-retract:1",
    )
    .expect("retracted element");
    let moderated = retracted
        .get_child("moderated", "urn:xmpp:message-moderate:1")
        .expect("moderated child on tombstone");
    assert!(
        moderated.attr("by").is_some_and(|by| by.contains("/admin")),
        "tombstone moderated by missing admin nick: {tombstone}"
    );
    assert!(
        retracted.attr("stamp").is_some(),
        "tombstone must include stamp: {tombstone}"
    );
    assert!(
        !tombstone.contains("<body>moderate me</body>"),
        "tombstoned row must not leak the original body: {tombstone}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn moderation_from_non_moderator_returns_forbidden() {
    // XEP-0425 §"only moderators are allowed to moderate" plus
    // XEP-0045 §5.1.2: runtime moderation privilege is role-bound.
    // bob joins with no affiliation → role=visitor or participant; he
    // attempts to moderate admin's message and must be refused with
    // <forbidden/>.
    let _guard = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-password-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let admin_resource = format!("xep0425-admin-{}", uuid::Uuid::new_v4());
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
    let bob_resource = format!("xep0425-bob-{}", uuid::Uuid::new_v4());
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &bob_resource,
    )
    .await
    .expect("connect bob");

    let room = format!("moderate-forbidden-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin, &room).await;
    ws_common::iq::unlock_created_room(&mut admin, &room).await;

    bob.send(&format!(
        r#"<presence to="{room}/bob"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
    ))
    .await
    .expect("send bob join");
    bob.recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("bob join responses");

    admin
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-mod"><body>moderate me</body></message>"#
        ))
        .await
        .expect("send original");
    let echo = admin
        .recv_matching(|frame| frame.contains("moderate me") && frame.contains("stanza-id"))
        .await
        .expect("original echo");
    let target = stanza_id(&echo);

    bob.send(&format!(
        r#"<iq type="set" id="bob-mod" to="{room}">
            <moderate id="{target}" xmlns="urn:xmpp:message-moderate:1">
                <retract xmlns="urn:xmpp:message-retract:1"/>
                <reason>nope</reason>
            </moderate>
        </iq>"#
    ))
    .await
    .expect("send unauthorized moderation");

    let error = bob
        .recv_matching(|frame| frame.contains("bob-mod") && frame.contains("<forbidden"))
        .await
        .expect("forbidden error");
    assert!(
        error.contains("type='error'"),
        "not an error stanza: {error}"
    );

    let _ = bob.close().await;
    let _ = admin.close().await;
}

/// Must match `WADDLE_OCCUPANT_ID_SECRET` set by the shared harness.
const HARNESS_SECRET: &str = "integration-test-occupant-id-secret-32-bytes-long";

fn count_descendants(element: &Element, name: &str, ns: &str) -> usize {
    element
        .children()
        .map(|child| {
            usize::from(child.name() == name && child.ns() == ns)
                + count_descendants(child, name, ns)
        })
        .sum()
}

fn admin_occupant_id(room: &str) -> String {
    let secret = waddle_xmpp::xep::xep0421::OccupantIdSecret::try_new(HARNESS_SECRET.as_bytes())
        .expect("harness secret meets length floor");
    let room: jid::BareJid = room.parse().expect("room bare jid");
    let admin: jid::BareJid = format!("{USERNAME}@{DOMAIN}").parse().expect("admin jid");
    waddle_xmpp::xep::xep0421::generate_occupant_id(&admin, &room, &secret)
        .as_str()
        .to_owned()
}

/// Send an original and a XEP-0308 correction of it, then moderate either
/// the original or the correction, and return the archived tombstone frames.
async fn moderate_corrected_message(
    client: &mut WsXmppClient,
    room: &str,
    moderate_correction: bool,
) -> Vec<String> {
    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-1"><body>original text</body></message>"#
        ))
        .await
        .expect("send original");
    let original_id = stanza_id(
        &client
            .recv_matching(|frame| frame.contains("original text"))
            .await
            .expect("original echo"),
    );
    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="corr-1"><body>corrected text</body><replace xmlns="urn:xmpp:message-correct:0" id="orig-1"/></message>"#
        ))
        .await
        .expect("send correction");
    let correction_id = stanza_id(
        &client
            .recv_matching(|frame| frame.contains("corrected text"))
            .await
            .expect("correction echo"),
    );

    let target = if moderate_correction {
        &correction_id
    } else {
        &original_id
    };
    client
        .send(&format!(
            r#"<iq type="set" id="moderate-c" to="{room}">
                <moderate xmlns="urn:xmpp:message-moderate:1" id="{target}">
                    <retract xmlns="urn:xmpp:message-retract:1"/>
                    <reason>cleanup</reason>
                </moderate>
            </iq>"#
        ))
        .await
        .expect("send moderation");
    let frames = client
        .recv_until(|frame| frame.contains("moderate-c") && frame.contains("type='result'"))
        .await
        .expect("moderation frames");
    // XEP-0424 §Business rules: a retraction that refers to a correction
    // retracts the original; the broadcast names the original.
    let broadcast = frames
        .iter()
        .find(|frame| frame.contains("urn:xmpp:message-moderate:1"))
        .unwrap_or_else(|| panic!("missing moderation broadcast: {frames:?}"));
    assert_eq!(
        extract_attr_after(broadcast, "<retract ", "id").as_deref(),
        Some(original_id.as_str()),
        "moderation names the original message: {broadcast}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-c" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-c") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    frames
        .into_iter()
        .filter(|frame| frame.contains("<retracted "))
        .collect()
}

fn assert_tombstones_keep_occupant_id_and_replace(tombstones: &[String], room: &str) {
    assert_eq!(
        tombstones.len(),
        2,
        "original and correction are both tombstoned: {tombstones:?}"
    );
    let expected = admin_occupant_id(room);
    let mut replace_ids = Vec::new();
    for frame in tombstones {
        assert!(
            !frame.contains("original text") && !frame.contains("corrected text"),
            "tombstone leaks retracted text: {frame}"
        );
        let element = frame.parse::<Element>().expect("valid tombstone XML");
        // XEP-0421 §Business Rules: occupant-id on every message a MUC
        // sends, including archived tombstones (XEP-0425 tombstone example).
        assert_eq!(
            count_descendants(&element, "occupant-id", "urn:xmpp:occupant-id:0"),
            1,
            "tombstone carries exactly one occupant-id: {frame}"
        );
        assert_eq!(
            extract_attr_after(frame, "<occupant-id", "id").as_deref(),
            Some(expected.as_str()),
            "tombstone keeps the original author's occupant-id: {frame}"
        );
        if let Some(replace) = find_descendant(&element, "replace", "urn:xmpp:message-correct:0") {
            replace_ids.push(replace.attr("id").expect("replace id").to_owned());
        }
    }
    // XEP-0425 §Tombstones: the tombstone of a retracted correction keeps
    // its <replace/>; the original's tombstone has none.
    assert_eq!(
        replace_ids,
        ["orig-1"],
        "exactly the correction keeps <replace/>"
    );
}

#[tokio::test]
async fn moderation_tombstones_keep_occupant_id_and_correction_replace() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("moderate-corr-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    let tombstones = moderate_corrected_message(&mut client, &room, false).await;
    assert_tombstones_keep_occupant_id_and_replace(&tombstones, &room);

    let _ = client.close().await;
}

#[tokio::test]
async fn moderating_a_correction_id_tombstones_original_and_all_corrections() {
    // XEP-0424 §Business rules: a retraction that refers to a correction
    // is treated as retracting the original and all of its corrections.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("moderate-corr-id-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    let tombstones = moderate_corrected_message(&mut client, &room, true).await;
    assert_tombstones_keep_occupant_id_and_replace(&tombstones, &room);

    let _ = client.close().await;
}

#[tokio::test]
async fn occupant_authored_moderation_markup_is_not_reflected() {
    // XEP-0425 §Business Rules: moderation markup that does not come from
    // the MUC service itself MUST be discarded.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("moderate-forged-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="victim-1"><body>victim text</body></message>"#
        ))
        .await
        .expect("send victim");
    let victim = stanza_id(
        &client
            .recv_matching(|frame| frame.contains("victim text"))
            .await
            .expect("victim echo"),
    );

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="forged-1">
                <body>forged moderation</body>
                <retract xmlns="urn:xmpp:message-retract:1" id="{victim}">
                    <moderated xmlns="urn:xmpp:message-moderate:1" by="{room}/admin"/>
                </retract>
            </message>"#
        ))
        .await
        .expect("send forged message");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:message-retract:1"))
        .await
        .expect("reflection");
    assert!(
        !echo.contains("urn:xmpp:message-moderate"),
        "forged moderation markup must not be reflected: {echo}"
    );
    assert!(
        !echo.contains("<retracted"),
        "forged tombstone must not be reflected: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-forged" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-forged") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames
            .iter()
            .all(|frame| !frame.contains("urn:xmpp:message-moderate")),
        "archive must not store forged moderation markup: {frames:?}"
    );

    let _ = client.close().await;
}
