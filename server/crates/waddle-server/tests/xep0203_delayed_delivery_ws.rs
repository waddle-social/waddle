//! XEP-0203 delayed-delivery integration tests over WebSocket.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0203-{}", uuid::Uuid::new_v4());
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

fn contains_nested_message_delay(frame: &str, body_text: &str) -> bool {
    fn find_message<'a>(element: &'a Element, body_text: &str) -> Option<&'a Element> {
        if element.name() == "message"
            && element.ns() == "jabber:client"
            && element
                .get_child("body", "jabber:client")
                .is_some_and(|body| body.text() == body_text)
        {
            return Some(element);
        }
        for child in element.children() {
            if let Some(found) = find_message(child, body_text) {
                return Some(found);
            }
        }
        None
    }

    let element = frame.parse::<Element>().expect("valid XML frame");
    find_message(&element, body_text)
        .and_then(|message| message.get_child("delay", "urn:xmpp:delay"))
        .is_some()
}

#[tokio::test]
async fn client_spoofed_delay_is_stripped_from_groupchat_flow() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("delay-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let body = "forged delay should vanish";
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="delay-1">
                <body>{body}</body>
                <delay xmlns="urn:xmpp:delay" from="evil.example" stamp="2024-06-01T09:30:00Z">forged</delay>
            </message>"#
        ))
        .await
        .expect("send groupchat with forged delay");
    let echo = client
        .recv_matching(|frame| frame.contains(body))
        .await
        .expect("live reflection");
    assert!(
        !contains_nested_message_delay(&echo, body),
        "live reflection leaked client-authored delay: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-delay" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-delay") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    let replay = frames
        .iter()
        .find(|frame| frame.contains(body))
        .unwrap_or_else(|| panic!("MAM did not replay original message: {frames:?}"));
    assert!(
        !contains_nested_message_delay(replay, body),
        "MAM replay leaked client-authored delay: {replay}"
    );

    let _ = client.close().await;
}
