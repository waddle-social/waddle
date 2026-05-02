//! XEP-0461 reply integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0461-{}", uuid::Uuid::new_v4());
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
async fn reply_routes_and_replays_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reply-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-1"><body>question</body></message>"#
        ))
        .await
        .expect("send original");
    let target = stanza_id(
        &client
            .recv_matching(|frame| frame.contains("question"))
            .await
            .expect("original echo"),
    );

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reply-1">
                <body>answer</body>
                <reply xmlns="urn:xmpp:reply:0" to="{room}/{USERNAME}" id="{target}"/>
            </message>"#
        ))
        .await
        .expect("send reply");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:reply:0"))
        .await
        .expect("reply echo");
    assert!(echo.contains(&target), "missing reply target: {echo}");

    client
        .send(&format!(
            r#"<iq type="set" id="mam-reply" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-reply") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("urn:xmpp:reply:0") && frame.contains(&target)),
        "MAM did not replay reply: {frames:?}"
    );

    client.close().await;
}

#[tokio::test]
async fn reply_to_unknown_target_routes_without_error() {
    // XEP-0461 imposes no server-side target-existence requirement
    // ("It is up to receiving entities…"). The previous implementation
    // returned `<item-not-found/>` when the server hadn't archived the
    // referenced message, which would reject legitimate cross-server
    // replies, replies to messages before retention, or replies to
    // client-cached history we never saw. Verify that a well-formed
    // reply to an unknown id is routed normally.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reply-unknown-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reply-orphan">
                <body>orphan reply</body>
                <reply xmlns="urn:xmpp:reply:0" to="{room}/{USERNAME}" id="never-archived-id"/>
            </message>"#
        ))
        .await
        .expect("send reply to unknown target");
    let echo = client
        .recv_matching(|frame| frame.contains("orphan reply"))
        .await
        .expect("reply echo");
    assert!(
        echo.contains("urn:xmpp:reply:0"),
        "reply payload missing: {echo}"
    );
    assert!(
        !echo.contains("<item-not-found"),
        "spec-non-conformant rejection: {echo}"
    );

    client.close().await;
}

#[tokio::test]
async fn reply_with_empty_to_jid_returns_bad_request() {
    // XEP-0461 §Use Cases: if the optional `to` attribute is present it
    // names the author of the referenced message, so it must be a valid JID.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reply-bad-to-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reply-bad-to">
                <body>bad reply</body>
                <reply xmlns="urn:xmpp:reply:0" to=" " id="parent-1"/>
            </message>"#
        ))
        .await
        .expect("send malformed reply");
    let error = client
        .recv_matching(|frame| frame.contains("<bad-request"))
        .await
        .expect("bad-request error");
    assert!(
        error.contains("type=\"error\""),
        "not an error stanza: {error}"
    );

    client.close().await;
}

#[tokio::test]
async fn reply_with_fallback_replays_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reply-fb-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-1"><body>original message</body></message>"#
        ))
        .await
        .expect("send original");
    let target = stanza_id(
        &client
            .recv_matching(|frame| frame.contains("original message"))
            .await
            .expect("original echo"),
    );

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reply-fb-1">
                <body>&gt; original message\nmy reply</body>
                <reply xmlns="urn:xmpp:reply:0" to="{room}/{USERNAME}" id="{target}"/>
                <fallback xmlns="urn:xmpp:fallback:0" for="urn:xmpp:reply:0">
                    <body start="0" end="20"/>
                </fallback>
            </message>"#
        ))
        .await
        .expect("send reply with fallback");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:fallback:0"))
        .await
        .expect("reply fallback echo");
    assert!(
        echo.contains("urn:xmpp:reply:0"),
        "reply echo missing reply element: {echo}"
    );
    assert!(
        echo.contains("urn:xmpp:fallback:0"),
        "reply echo missing fallback element: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-reply-fb" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-reply-fb") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    let reply_mam_frame = frames
        .iter()
        .find(|frame| frame.contains("urn:xmpp:reply:0"))
        .expect("MAM should replay the reply message");
    assert!(
        reply_mam_frame.contains("urn:xmpp:fallback:0"),
        "MAM replay should preserve fallback element: {reply_mam_frame}"
    );
    assert!(
        reply_mam_frame.contains("for=\"urn:xmpp:reply:0\""),
        "MAM replay should preserve fallback 'for' attribute: {reply_mam_frame}"
    );

    client.close().await;
}
