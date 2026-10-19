//! XEP-0372 reference integration tests over WebSocket.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0372-{}", uuid::Uuid::new_v4());
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
async fn references_route_and_replay_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reference-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reference-1">
                <body>Hello @admin</body>
                <reference xmlns="urn:xmpp:reference:0" type="mention" begin="6" end="12" uri="xmpp:admin@localhost"/>
            </message>"#
        ))
        .await
        .expect("send reference");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:reference:0"))
        .await
        .expect("reference echo");
    assert!(
        echo.contains("xmpp:admin@localhost"),
        "missing reference uri: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-reference" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-reference") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("urn:xmpp:reference:0")
                && frame.contains("xmpp:admin@localhost")),
        "MAM did not replay reference: {frames:?}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn data_reference_with_anchor_routes_and_replays_from_mam() {
    // XEP-0372: type="data" references attach a URI annotation to a span of
    // body text — the standard wire shape for clickable links inside chat
    // messages. The server must route them unchanged and MAM must replay them.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reference-data-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reference-data-1">
                <body>see https://example.com</body>
                <reference xmlns="urn:xmpp:reference:0" type="data" begin="4" end="23" uri="https://example.com" anchor="https://example.com"/>
            </message>"#
        ))
        .await
        .expect("send data reference");
    let echo = client
        .recv_matching(|frame| {
            frame.contains("urn:xmpp:reference:0") && frame.contains("type='data'")
        })
        .await
        .expect("data reference echo");
    assert!(
        echo.contains("https://example.com"),
        "missing data reference uri: {echo}"
    );
    assert!(
        echo.contains("anchor='https://example.com'"),
        "missing anchor attribute: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-data-reference" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-data-reference") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames.iter().any(|frame| frame.contains("type='data'")
            && frame.contains("https://example.com")
            && frame.contains("anchor='https://example.com'")),
        "MAM did not replay data reference with anchor: {frames:?}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn reference_without_required_attributes_returns_bad_request() {
    // XEP-0372: '<reference/>' MUST contain a 'type' and a 'uri'.
    // Server should reject malformed references with bad-request so
    // misbehaving clients don't poison archives with junk payloads.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reference-bad-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="bad-reference">
                <body>broken reference</body>
                <reference xmlns="urn:xmpp:reference:0" begin="0" end="6"/>
            </message>"#
        ))
        .await
        .expect("send malformed reference");
    let error = client
        .recv_matching(|frame| frame.contains("<bad-request"))
        .await
        .expect("bad-request error");
    assert!(
        error.contains("type='error'"),
        "not an error stanza: {error}"
    );

    let _ = client.close().await;
}

// ---------------------------------------------------------------------------
// Personal mentions of non-members (XEP-0430 inbox fan-out)
// ---------------------------------------------------------------------------

const MENTION_PEER: &str = "peer";
const MENTION_PEER_PASSWORD: &str = "peer-password-1";
const NS_INBOX: &str = "urn:xmpp:inbox:1";

fn mention_element_to_xml(element: &xmpp_parsers::minidom::Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize XML");
    String::from_utf8(buf).expect("UTF-8")
}

/// `unread` of the caller's XEP-0430 inbox row for `room`, if any.
async fn inbox_unread(client: &mut WsXmppClient, room: &str, id: &str) -> Option<String> {
    use xmpp_parsers::minidom::Element;
    let query = Element::builder("iq", "jabber:client")
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("type").to_owned(),
            "get",
        )
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("id").to_owned(),
            id,
        )
        .append(
            Element::builder("inbox", NS_INBOX)
                .attr(
                    xmpp_parsers::minidom::rxml::xml_ncname!("messages").to_owned(),
                    "false",
                )
                .build(),
        )
        .build();
    client
        .send(&mention_element_to_xml(&query))
        .await
        .expect("send inbox query");
    let frames = client
        .recv_until(|frame| frame.contains("<iq") && frame.contains(id) && frame.contains("<fin"))
        .await
        .expect("inbox response");
    frames
        .iter()
        .filter_map(|frame| frame.parse::<Element>().ok())
        .filter(|message| message.name() == "message" && message.attr("type") != Some("headline"))
        .flat_map(|message| {
            message
                .children()
                .filter(|child| child.name() == "entry" && child.ns() == NS_INBOX)
                .cloned()
                .collect::<Vec<_>>()
        })
        .find(|entry| entry.attr("jid") == Some(room))
        .and_then(|entry| entry.attr("unread").map(str::to_string))
}

/// Poll a fresh session for `user` until its inbox has a row for `room`.
async fn wait_for_inbox_unread(
    server: &TestServer,
    user: &str,
    password: &str,
    room: &str,
) -> Option<String> {
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        user,
        password,
        &format!("inbox-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect inbox client");
    let mut unread = None;
    for attempt in 0..50 {
        unread = inbox_unread(&mut client, room, &format!("inbox-poll-{attempt}")).await;
        if unread.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let _ = client.close().await;
    unread
}

/// XEP-0372 §"Reference type 'mention'": a mention reference to a user
/// who is not in the open room still reaches them — the server projects
/// an inbox row (and notification candidate) for the mentioned offline
/// non-member.
#[tokio::test]
async fn mention_reference_reaches_offline_non_member_inbox() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(MENTION_PEER, MENTION_PEER_PASSWORD)]);
    let password = server.fixed_account_password().to_string();
    let mut client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, "xep0372-nm")
            .await
            .expect("connect and auth");
    let room = format!("reference-nm-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reference-nm-1">
                <body>@peer take a look</body>
                <reference xmlns="urn:xmpp:reference:0" type="mention" begin="0" end="5" uri="xmpp:peer@localhost/phone"/>
            </message>"#
        ))
        .await
        .expect("send reference");
    client
        .recv_matching(|frame| frame.contains("reference-nm-1"))
        .await
        .expect("reference echo");

    assert_eq!(
        wait_for_inbox_unread(&server, MENTION_PEER, MENTION_PEER_PASSWORD, &room).await,
        Some("1".to_string()),
        "the mentioned offline non-member must get one unread inbox row"
    );
    let _ = client.close().await;
}
