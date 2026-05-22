//! XEP-0513 explicit mention integration tests over WebSocket.

mod ws_common;

use std::str::FromStr;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0513-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("connect and auth");
    (server, client)
}

async fn join_room(client: &mut WsXmppClient, room: &str) -> String {
    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    let frames = client
        .recv_until(|frame| frame.contains("urn:xmpp:occupant-id:0"))
        .await
        .expect("join responses");
    frames
        .iter()
        .find_map(|frame| occupant_id_from_frame(frame))
        .expect("self presence occupant id")
}

fn occupant_id_from_frame(frame: &str) -> Option<String> {
    let element = Element::from_str(frame).ok()?;
    find_occupant_id(&element)
}

fn find_occupant_id(element: &Element) -> Option<String> {
    if element.name() == "occupant-id" && element.ns() == "urn:xmpp:occupant-id:0" {
        return element.attr("id").map(str::to_string);
    }
    element.children().find_map(find_occupant_id)
}

#[tokio::test]
async fn explicit_mentions_route_and_replay_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mentions-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let occupant_id = join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="mention-1">
                <body>@admin please check this</body>
                <mention xmlns="urn:xmpp:mentions:0" begin="0" end="6" occupantid="{occupant_id}"/>
            </message>"#
        ))
        .await
        .expect("send mention");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:mentions:0"))
        .await
        .expect("mention echo");
    assert!(
        echo.contains(&occupant_id),
        "missing mentioned occupant id: {echo}"
    );
    assert!(
        !echo.contains("jid='admin@localhost") && !echo.contains("jid='admin@localhost"),
        "MUC mention payload leaked a JID despite occupant-id support: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-mention" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-mention") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("urn:xmpp:mentions:0")
                && frame.contains(&occupant_id)
                && !frame.contains("jid='admin@localhost")
                && !frame.contains("jid='admin@localhost")),
        "MAM did not replay mention: {frames:?}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn mention_without_target_attribute_returns_bad_request() {
    // XEP-0513: a '<mention/>' must identify its target via 'jid',
    // 'occupantid', or 'mentions' (group). Decorative mentions with
    // none of these are not interpretable by receivers and are
    // rejected with bad-request.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mention-bad-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let _occupant_id = join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="bad-mention">
                <body>broken mention</body>
                <mention xmlns="urn:xmpp:mentions:0" begin="0" end="6"/>
            </message>"#
        ))
        .await
        .expect("send malformed mention");
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

#[tokio::test]
async fn muc_jid_mention_returns_bad_request_when_occupant_ids_supported() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mention-jid-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let _occupant_id = join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="jid-mention">
                <body>jid mention</body>
                <mention xmlns="urn:xmpp:mentions:0" begin="0" end="3" jid="admin@localhost"/>
            </message>"#
        ))
        .await
        .expect("send jid mention");
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

/// XEP-0513 §295: `<iq type='get' to='room@muc.…'><query
/// xmlns='urn:xmpp:mentions:0'/></iq>` returns a §303 result form
/// carrying the server-internal policy (`mentions#count = 5`,
/// `mentions#individual = participants`, `mentions#channel =
/// moderators`).
#[tokio::test]
async fn mentions_permissions_iq_get_returns_303_form() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("perms-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let _occupant_id = join_room(&mut client, &room).await;

    let iq_id = "perm-get-1";
    client
        .send(&format!(
            r#"<iq type="get" to="{room}" id="{iq_id}"><query xmlns="urn:xmpp:mentions:0"/></iq>"#
        ))
        .await
        .expect("send §295 query");

    let frame = client
        .recv_matching(|frame| {
            frame.contains(&format!("id='{iq_id}'")) || frame.contains(&format!("id=\"{iq_id}\""))
        })
        .await
        .expect("§295 result");

    assert!(
        frame.contains("type='result'") || frame.contains("type=\"result\""),
        "expected iq result, got: {frame}"
    );
    assert!(
        frame.contains("urn:xmpp:mentions:0"),
        "result must echo the namespace, got: {frame}"
    );
    assert!(
        frame.contains("jabber:x:data"),
        "result must carry a data form, got: {frame}"
    );
    assert!(
        frame.contains("FORM_TYPE"),
        "form must include FORM_TYPE, got: {frame}"
    );
    assert!(
        frame.contains("mentions#count"),
        "form must include mentions#count, got: {frame}"
    );
    assert!(
        frame.contains("mentions#individual"),
        "form must include mentions#individual, got: {frame}"
    );
    assert!(
        frame.contains("mentions#channel"),
        "form must include mentions#channel (channel mentions advertised), got: {frame}"
    );
    assert!(
        frame.contains("moderators"),
        "channel field value must default to `moderators`, got: {frame}"
    );

    let _ = client.close().await;
}

/// XEP-0513 §295: `<iq type='set'/>` to the room with the same query
/// payload returns `<feature-not-implemented/>` — Waddle uses a
/// hardcoded server policy and exposes no owner-config write path for
/// the §303 form. (XEP §295 reserves `<forbidden/>` for "not an
/// owner"; returning that here would misrepresent the cause.)
#[tokio::test]
async fn mentions_permissions_iq_set_returns_feature_not_implemented() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("perms-set-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let _occupant_id = join_room(&mut client, &room).await;

    let iq_id = "perm-set-1";
    client
        .send(&format!(
            r#"<iq type="set" to="{room}" id="{iq_id}">
                <query xmlns="urn:xmpp:mentions:0">
                    <x xmlns="jabber:x:data" type="submit">
                        <field var="FORM_TYPE"><value>urn:xmpp:mentions:0</value></field>
                        <field var="mentions#count"><value>1</value></field>
                        <field var="mentions#individual"><value>participants</value></field>
                        <field var="mentions#channel"><value>moderators</value></field>
                    </x>
                </query>
            </iq>"#
        ))
        .await
        .expect("send §295 set");

    let frame = client
        .recv_matching(|frame| {
            frame.contains(&format!("id='{iq_id}'")) || frame.contains(&format!("id=\"{iq_id}\""))
        })
        .await
        .expect("§295 set response");

    assert!(
        frame.contains("type='error'") || frame.contains("type=\"error\""),
        "expected iq error, got: {frame}"
    );
    assert!(
        frame.contains("<feature-not-implemented"),
        "expected feature-not-implemented condition, got: {frame}"
    );
    // §295 error example echoes the `<query xmlns='urn:xmpp:mentions:0'/>`
    // alongside `<error/>` — verify the response contract matches.
    assert!(
        frame.contains("urn:xmpp:mentions:0"),
        "error envelope must echo §295 <query/>, got: {frame}"
    );

    let _ = client.close().await;
}

/// XEP-0513 §295: bare-room-JID is mandatory in `to`. A full-JID
/// target (`room@muc.…/nick`) is room-meaningless because permissions
/// are room-scoped; the handler returns `<bad-request/>` with the
/// `<query xmlns='urn:xmpp:mentions:0'/>` echoed alongside `<error/>`
/// per the §295 error example.
#[tokio::test]
async fn mentions_permissions_iq_full_jid_target_returns_bad_request_with_query_echo() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("perms-fjid-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let occupant_nick = USERNAME;
    let _occupant_id = join_room(&mut client, &room).await;

    let iq_id = "perm-fjid-1";
    client
        .send(&format!(
            r#"<iq type="get" to="{room}/{occupant_nick}" id="{iq_id}"><query xmlns="urn:xmpp:mentions:0"/></iq>"#
        ))
        .await
        .expect("send §295 query to full JID");

    let frame = client
        .recv_matching(|frame| {
            frame.contains(&format!("id='{iq_id}'")) || frame.contains(&format!("id=\"{iq_id}\""))
        })
        .await
        .expect("§295 bad-request response");

    assert!(
        frame.contains("type='error'") || frame.contains("type=\"error\""),
        "expected iq error, got: {frame}"
    );
    assert!(
        frame.contains("<bad-request"),
        "expected bad-request, got: {frame}"
    );
    // §295 error example echoes the `<query xmlns='urn:xmpp:mentions:0'/>`.
    assert!(
        frame.contains("urn:xmpp:mentions:0"),
        "error envelope must echo the §295 <query/>, got: {frame}"
    );
}

/// XEP-0513 §295: the service JID itself (`to='muc.example'`, no
/// node) is not a room and has no §303 permissions form. Returns
/// `<bad-request/>` with the same `<query/>` echo as the full-JID
/// rejection above.
#[tokio::test]
async fn mentions_permissions_iq_service_jid_target_returns_bad_request_with_query_echo() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let _ = join_room(
        &mut client,
        &format!("perms-svc-warmup-{}@muc.{DOMAIN}", uuid::Uuid::new_v4()),
    )
    .await;

    let iq_id = "perm-svc-1";
    client
        .send(&format!(
            r#"<iq type="get" to="muc.{DOMAIN}" id="{iq_id}"><query xmlns="urn:xmpp:mentions:0"/></iq>"#
        ))
        .await
        .expect("send §295 query to service JID");

    let frame = client
        .recv_matching(|frame| {
            frame.contains(&format!("id='{iq_id}'")) || frame.contains(&format!("id=\"{iq_id}\""))
        })
        .await
        .expect("§295 service-JID response");

    assert!(
        frame.contains("type='error'") || frame.contains("type=\"error\""),
        "expected iq error, got: {frame}"
    );
    assert!(
        frame.contains("<bad-request"),
        "expected bad-request, got: {frame}"
    );
    assert!(
        frame.contains("urn:xmpp:mentions:0"),
        "error envelope must echo the §295 <query/>, got: {frame}"
    );
}
