//! XEP-0425 moderated retraction integration tests over WebSocket.

mod ws_common;

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
        .recv_until(|frame| frame.contains("moderate-1") && frame.contains("type=\"result\""))
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
    let replay = frames
        .iter()
        .find(|frame| frame.contains("urn:xmpp:message-moderate:1"))
        .unwrap_or_else(|| panic!("MAM did not replay moderation: {frames:?}"));
    assert_moderation_shape(replay, &target);

    client.close().await;
}
