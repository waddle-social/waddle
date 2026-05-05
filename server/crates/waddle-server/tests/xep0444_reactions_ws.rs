//! XEP-0444 reaction integration tests over WebSocket.

mod ws_common;

use std::str::FromStr;
use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const BOB_USERNAME: &str = "bob";
const BOB_PASSWORD: &str = "bob-password";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let client = connect(&server, USERNAME, &password, "xep0444").await;
    (server, client)
}

async fn connect(
    server: &TestServer,
    username: &str,
    password: &str,
    resource_prefix: &str,
) -> WsXmppClient {
    let resource = format!("{resource_prefix}-{}", uuid::Uuid::new_v4());
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, username, password, &resource)
        .await
        .expect("connect and auth")
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

fn frame_has_direct_message_body(frame: &str) -> bool {
    Element::from_str(frame)
        .ok()
        .is_some_and(|element| element_contains_direct_message_body(&element))
}

fn element_contains_direct_message_body(element: &Element) -> bool {
    let this_element_matches = element.name() == "message"
        && element
            .children()
            .any(|child| child.name() == "body" && child.ns() == element.ns());

    this_element_matches || element.children().any(element_contains_direct_message_body)
}

#[tokio::test]
async fn direct_reaction_replays_from_personal_mam_after_reconnect() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(BOB_USERNAME, BOB_PASSWORD)]);
    let admin_password = server.fixed_account_password().to_string();
    let mut alice = connect(&server, USERNAME, &admin_password, "xep0444-alice").await;
    let mut bob = connect(&server, BOB_USERNAME, BOB_PASSWORD, "xep0444-bob").await;
    let alice_jid = format!("{USERNAME}@{DOMAIN}");
    let bob_jid = format!("{BOB_USERNAME}@{DOMAIN}");

    alice
        .send(&format!(
            r#"<message type="chat" to="{bob_jid}" id="direct-original"><body>direct reaction target</body></message>"#
        ))
        .await
        .expect("send original direct message");
    bob.recv_matching(|frame| frame.contains("direct reaction target"))
        .await
        .expect("receive original direct message");

    bob.send(&format!(
        r#"<message type="chat" to="{alice_jid}" id="direct-reaction">
                <reactions xmlns="urn:xmpp:reactions:0" id="direct-original">
                    <reaction>🔥</reaction>
                </reactions>
                <store xmlns="urn:xmpp:hints"/>
            </message>"#
    ))
    .await
    .expect("send direct reaction");
    alice
        .recv_matching(|frame| {
            frame.contains("urn:xmpp:reactions:0") && frame.contains("direct-original")
        })
        .await
        .expect("receive direct reaction");

    alice.close().await;
    let mut alice = connect(
        &server,
        USERNAME,
        &admin_password,
        "xep0444-alice-reconnect",
    )
    .await;
    alice
        .send(&format!(
            r#"<iq type="set" id="mam-direct-reaction" to="{alice_jid}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send personal MAM query");
    let frames = alice
        .recv_until(|frame| frame.contains("mam-direct-reaction") && frame.contains("<fin"))
        .await
        .expect("personal MAM frames");

    let reaction = frames
        .iter()
        .find(|frame| {
            frame.contains("direct-reaction")
                && frame.contains("urn:xmpp:reactions:0")
                && frame.contains("direct-original")
        })
        .unwrap_or_else(|| panic!("personal MAM did not replay direct reaction: {frames:?}"));
    assert!(
        !frame_has_direct_message_body(reaction),
        "direct reaction replay unexpectedly had a body: {reaction}"
    );

    bob.close().await;
    alice.close().await;
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
