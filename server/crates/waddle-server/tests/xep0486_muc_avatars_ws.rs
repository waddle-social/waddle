//! XEP-0486 MUC Avatars over WebSocket.
//!
//! An owner publishes a vcard-temp PHOTO to the room's bare JID; the room
//! answers `forbidden` to anyone else, broadcasts status 104 to its
//! occupants, advertises the XEP-0153 SHA-1 in `muc#roominfo_avatarhash`
//! of its disco#info, and serves the vCard back. An empty vCard
//! unpublishes the avatar.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const BOB: &str = "bob";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NS_DATA: &str = "jabber:x:data";
const NS_VCARD: &str = "vcard-temp";
const FIELD_AVATARHASH: &str = "muc#roominfo_avatarhash";

// The SVG avatar of xep-0486.xml and its published XEP-0153 hash.
const SVG_BINVAL: &str = "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIzMiIgaGVpZ2h0PSIzMiI+CiA8cmVjdCB4PSIwIiB5PSIwIiB3aWR0aD0iMzIiIGhlaWdodD0iMzIiIGZpbGw9InJlZCIvPgo8L3N2Zz4K";
const SVG_HASH: &str = "a31c4bd04de69663cfd7f424a8453f4674da37ff";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

struct Fixture {
    _server: TestServer,
    owner: WsXmppClient,
    bob: WsXmppClient,
    room: String,
}

async fn join(client: &mut WsXmppClient, room: &str, nick: &str) {
    client
        .send(&format!(
            r#"<presence to="{room}/{nick}"><x xmlns="{NS_MUC}"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

async fn fixture() -> Fixture {
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(BOB, &bob_pass)]);
    let admin_pass = server.fixed_account_password().to_string();
    let mut owner = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        ADMIN,
        &admin_pass,
        "avatar-owner",
    )
    .await
    .expect("owner connects");
    let mut bob =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, BOB, &bob_pass, "avatar-bob")
            .await
            .expect("bob connects");
    let room = format!("avatar-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join(&mut owner, &room, ADMIN).await;
    ws_common::iq::unlock_created_room(&mut owner, &room).await;
    join(&mut bob, &room, BOB).await;
    Fixture {
        _server: server,
        owner,
        bob,
        room,
    }
}

fn publish_xml(id: &str, room: &str) -> String {
    format!(
        r#"<iq type="set" id="{id}" to="{room}"><vCard xmlns="{NS_VCARD}"><PHOTO><TYPE>image/svg+xml</TYPE><BINVAL>{SVG_BINVAL}</BINVAL></PHOTO></vCard></iq>"#
    )
}

async fn reply(client: &mut WsXmppClient, id: &str) -> Element {
    let needle = format!("id='{id}'");
    let needle_dq = format!("id=\"{id}\"");
    client
        .recv_matching(|frame| {
            (frame.contains(&needle) || frame.contains(&needle_dq)) && frame.starts_with("<iq")
        })
        .await
        .expect("iq reply")
        .parse()
        .expect("iq xml")
}

fn error_condition(iq: &Element) -> String {
    assert_eq!(iq.attr("type"), Some("error"), "expected error: {iq:?}");
    iq.get_child("error", "jabber:client")
        .expect("error child")
        .children()
        .find(|child| child.ns() == "urn:ietf:params:xml:ns:xmpp-stanzas" && child.name() != "text")
        .expect("condition")
        .name()
        .to_string()
}

async fn avatar_hashes(client: &mut WsXmppClient, room: &str, id: &str) -> Vec<String> {
    client
        .send(&format!(
            r#"<iq type="get" id="{id}" to="{room}"><query xmlns="{NS_DISCO_INFO}"/></iq>"#
        ))
        .await
        .expect("send disco");
    let response = reply(client, id).await;
    assert_eq!(response.attr("type"), Some("result"), "{response:?}");
    let query = response.get_child("query", NS_DISCO_INFO).expect("query");
    let roominfo = query
        .children()
        .filter(|x| x.is("x", NS_DATA))
        .filter(|form| {
            form.children().any(|field| {
                field.attr("var") == Some("FORM_TYPE")
                    && field
                        .children()
                        .any(|v| v.text() == "http://jabber.org/protocol/muc#roominfo")
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(roominfo.len(), 1, "exactly one muc#roominfo form");
    roominfo[0]
        .children()
        .find(|field| field.attr("var") == Some(FIELD_AVATARHASH))
        .map(|field| {
            assert_eq!(field.attr("type"), Some("text-multi"));
            field
                .children()
                .filter(|v| v.name() == "value")
                .map(Element::text)
                .collect()
        })
        .unwrap_or_default()
}

async fn expect_104(client: &mut WsXmppClient, room: &str) {
    let frame = client
        .recv_matching(|frame| frame.starts_with("<message") && frame.contains("code='104'"))
        .await
        .expect("status 104 broadcast");
    let message: Element = frame.parse().expect("message xml");
    assert_eq!(message.attr("type"), Some("groupchat"));
    assert_eq!(message.attr("from"), Some(room));
    assert!(message
        .get_child("x", NS_MUC_USER)
        .expect("muc#user x")
        .children()
        .any(|status| status.name() == "status" && status.attr("code") == Some("104")));
}

#[tokio::test]
async fn owner_publishes_avatar_and_room_advertises_hash_and_serves_vcard() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    assert!(avatar_hashes(&mut f.bob, &f.room, "d0").await.is_empty());

    f.owner
        .send(&publish_xml("pub1", &f.room))
        .await
        .expect("send");
    let ack = reply(&mut f.owner, "pub1").await;
    assert_eq!(ack.attr("type"), Some("result"), "{ack:?}");
    assert_eq!(ack.attr("from"), Some(f.room.as_str()));

    // "The room should then broadcast a notification that the
    // configuration changed to all users present."
    expect_104(&mut f.bob, &f.room).await;
    expect_104(&mut f.owner, &f.room).await;

    assert_eq!(
        avatar_hashes(&mut f.bob, &f.room, "d1").await,
        vec![SVG_HASH.to_string()]
    );

    f.bob
        .send(&format!(
            r#"<iq type="get" id="get1" to="{}"><vCard xmlns="{NS_VCARD}"/></iq>"#,
            f.room
        ))
        .await
        .expect("send get");
    let vcard = reply(&mut f.bob, "get1").await;
    assert_eq!(vcard.attr("type"), Some("result"));
    let photo = vcard
        .get_child("vCard", NS_VCARD)
        .and_then(|v| v.get_child("PHOTO", NS_VCARD))
        .expect("PHOTO");
    assert_eq!(
        photo
            .get_child("TYPE", NS_VCARD)
            .map(Element::text)
            .as_deref(),
        Some("image/svg+xml")
    );
    assert_eq!(
        photo
            .get_child("BINVAL", NS_VCARD)
            .map(Element::text)
            .as_deref(),
        Some(SVG_BINVAL)
    );
}

#[tokio::test]
async fn non_owner_publish_is_forbidden_and_changes_nothing() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    f.bob
        .send(&publish_xml("deny1", &f.room))
        .await
        .expect("send");
    let denied = reply(&mut f.bob, "deny1").await;
    assert_eq!(error_condition(&denied), "forbidden");
    let error = denied.get_child("error", "jabber:client").expect("error");
    assert_eq!(error.attr("type"), Some("auth"));
    assert!(avatar_hashes(&mut f.owner, &f.room, "d2").await.is_empty());
}

#[tokio::test]
async fn empty_vcard_unpublishes_the_avatar() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    f.owner
        .send(&publish_xml("pub2", &f.room))
        .await
        .expect("send");
    assert_eq!(
        reply(&mut f.owner, "pub2").await.attr("type"),
        Some("result")
    );
    expect_104(&mut f.bob, &f.room).await;

    f.owner
        .send(&format!(
            r#"<iq type="set" id="unpub" to="{}"><vCard xmlns="{NS_VCARD}"/></iq>"#,
            f.room
        ))
        .await
        .expect("send");
    assert_eq!(
        reply(&mut f.owner, "unpub").await.attr("type"),
        Some("result")
    );
    expect_104(&mut f.bob, &f.room).await;

    assert!(avatar_hashes(&mut f.bob, &f.room, "d3").await.is_empty());
    f.bob
        .send(&format!(
            r#"<iq type="get" id="get2" to="{}"><vCard xmlns="{NS_VCARD}"/></iq>"#,
            f.room
        ))
        .await
        .expect("send");
    assert_eq!(
        error_condition(&reply(&mut f.bob, "get2").await),
        "item-not-found"
    );
}

#[tokio::test]
async fn unhashable_vcard_is_rejected_as_bad_request() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    f.owner
        .send(&format!(
            r#"<iq type="set" id="bad1" to="{}"><vCard xmlns="{NS_VCARD}"><FN>Garden</FN></vCard></iq>"#,
            f.room
        ))
        .await
        .expect("send");
    assert_eq!(
        error_condition(&reply(&mut f.owner, "bad1").await),
        "bad-request"
    );
    f.owner
        .send(&format!(
            r#"<iq type="set" id="bad2" to="{}"><vCard xmlns="{NS_VCARD}"><PHOTO><TYPE>image/png</TYPE><BINVAL>***</BINVAL></PHOTO></vCard></iq>"#,
            f.room
        ))
        .await
        .expect("send");
    assert_eq!(
        error_condition(&reply(&mut f.owner, "bad2").await),
        "bad-request"
    );
}

#[tokio::test]
async fn vcard_of_a_missing_room_is_item_not_found() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    let ghost = format!("never-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    f.bob
        .send(&format!(
            r#"<iq type="get" id="ghost" to="{ghost}"><vCard xmlns="{NS_VCARD}"/></iq>"#
        ))
        .await
        .expect("send");
    assert_eq!(
        error_condition(&reply(&mut f.bob, "ghost").await),
        "item-not-found"
    );
}
