//! XEP-0045 admin-effect websocket ordering coverage.
//!
//! This suite pins the response ordering guarantees for the room-effect
//! outbox integration path:
//!
//! - a self-kick (`307`) must reach the kickee before the IQ result;
//! - a moderator kicking someone else must receive the IQ result before
//!   their own broadcast copy;
//! - ban (`301`), members-only membership removal (`321`), and voice
//!   changes must likewise return the IQ result first to the moderator.

use std::time::Duration;

use tokio::sync::Mutex;
use waddle_ws_test_support as ws_common;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::{self, Element};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const ALICE: &str = "alice";
const BOB: &str = "bob";
const NS_CLIENT: &str = "jabber:client";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_XDATA: &str = "jabber:x:data";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn connect(server: &TestServer, user: &str, password: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, resource)
        .await
        .expect("connect and auth")
}

fn xml(element: Element) -> String {
    String::from(&element)
}

async fn send_element(client: &mut WsXmppClient, element: Element) {
    client.send(&xml(element)).await.expect("send stanza");
}

async fn join_room(client: &mut WsXmppClient, room: &str, nick: &str) {
    send_element(
        client,
        Element::builder("presence", NS_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                format!("{room}/{nick}"),
            )
            .append(Element::builder("x", NS_MUC).build())
            .build(),
    )
    .await;
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

async fn drain_available(client: &mut WsXmppClient) -> Vec<String> {
    let mut frames = Vec::new();
    loop {
        match client.recv_timeout(Duration::from_millis(150)).await {
            Ok(frame) => frames.push(frame),
            Err(error) if error == "Timeout waiting for message" => return frames,
            Err(error) => panic!("unexpected drain error: {error}"),
        }
    }
}

async fn settle_clients(clients: &mut [&mut WsXmppClient]) {
    for client in clients.iter_mut() {
        let _ = drain_available(client).await;
    }
}

fn data_form_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(Element::builder("value", NS_XDATA).append(value).build())
        .build()
}

fn owner_config_iq(room: &str, id: &str, fields: &[(&str, &str)]) -> Element {
    let mut form = Element::builder("x", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", NS_XDATA)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                .append(
                    Element::builder("value", NS_XDATA)
                        .append("http://jabber.org/protocol/muc#roomconfig")
                        .build(),
                )
                .build(),
        );
    for (var, value) in fields {
        form = form.append(data_form_field(var, value));
    }
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
        .append(
            Element::builder("query", NS_MUC_OWNER)
                .append(form.build())
                .build(),
        )
        .build()
}

fn muc_admin_role_iq(
    room: &str,
    id: &str,
    nick: &str,
    role: &str,
    reason: Option<&str>,
) -> Element {
    let item = if let Some(reason) = reason {
        Element::builder("item", NS_MUC_ADMIN)
            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), nick)
            .attr(minidom::rxml::xml_ncname!("role").to_owned(), role)
            .append(
                Element::builder("reason", NS_MUC_ADMIN)
                    .append(reason)
                    .build(),
            )
            .build()
    } else {
        Element::builder("item", NS_MUC_ADMIN)
            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), nick)
            .attr(minidom::rxml::xml_ncname!("role").to_owned(), role)
            .build()
    };
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
        .append(Element::builder("query", NS_MUC_ADMIN).append(item).build())
        .build()
}

fn muc_admin_affiliation_iq(
    room: &str,
    id: &str,
    jid: &str,
    affiliation: &str,
    reason: Option<&str>,
) -> Element {
    let item = if let Some(reason) = reason {
        Element::builder("item", NS_MUC_ADMIN)
            .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid)
            .attr(
                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                affiliation,
            )
            .append(
                Element::builder("reason", NS_MUC_ADMIN)
                    .append(reason)
                    .build(),
            )
            .build()
    } else {
        Element::builder("item", NS_MUC_ADMIN)
            .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid)
            .attr(
                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                affiliation,
            )
            .build()
    };
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
        .append(Element::builder("query", NS_MUC_ADMIN).append(item).build())
        .build()
}

async fn expect_iq_result(client: &mut WsXmppClient, id: &str) -> String {
    let frame = client.recv().await.expect("IQ result frame");
    assert!(
        frame.contains("<iq") && frame.contains(id),
        "expected IQ result for {id}, got: {frame}"
    );
    assert!(
        frame.contains("type='result'") || frame.contains(r#"type="result""#),
        "expected successful IQ result for {id}, got: {frame}"
    );
    frame
}

fn parse_frame(frame: &str) -> Element {
    frame
        .parse::<Element>()
        .unwrap_or_else(|err| panic!("frame must parse as XML: {err}; frame={frame}"))
}

fn find_descendant<'a>(root: &'a Element, name: &str, ns: &str) -> Option<&'a Element> {
    for child in root.children() {
        if child.name() == name && child.ns() == ns {
            return Some(child);
        }
        if let Some(found) = find_descendant(child, name, ns) {
            return Some(found);
        }
    }
    None
}

fn muc_user_payload(presence: &Element) -> &Element {
    find_descendant(presence, "x", NS_MUC_USER)
        .unwrap_or_else(|| panic!("presence missing <x xmlns='muc#user'>: {presence:?}"))
}

fn has_status_code(muc_user: &Element, code: &str) -> bool {
    muc_user
        .children()
        .filter(|child| child.name() == "status" && child.ns() == NS_MUC_USER)
        .any(|status| status.attr("code") == Some(code))
}

fn muc_user_item<'a>(muc_user: &'a Element, frame: &str) -> &'a Element {
    muc_user
        .children()
        .find(|child| child.name() == "item" && child.ns() == NS_MUC_USER)
        .unwrap_or_else(|| panic!("presence missing <item> in muc#user: {frame}"))
}

fn assert_unavailable_admin_presence(
    frame: &str,
    expected_from: &str,
    expected_status: &str,
    expected_role: Option<&str>,
    expected_affiliation: Option<&str>,
    is_self: bool,
) {
    let presence = parse_frame(frame);
    assert_eq!(presence.name(), "presence", "expected <presence>: {frame}");
    assert_eq!(
        presence.attr("type"),
        Some("unavailable"),
        "expected unavailable presence: {frame}"
    );
    assert_eq!(
        presence.attr("from"),
        Some(expected_from),
        "unexpected presence sender: {frame}"
    );

    let muc_user = muc_user_payload(&presence);
    assert!(
        has_status_code(muc_user, expected_status),
        "expected status code {expected_status}: {frame}"
    );
    if is_self {
        assert!(
            has_status_code(muc_user, "110"),
            "self-presence must include status 110: {frame}"
        );
    }

    let item = muc_user_item(muc_user, frame);
    if let Some(role) = expected_role {
        assert_eq!(item.attr("role"), Some(role), "unexpected role in: {frame}");
    }
    if let Some(affiliation) = expected_affiliation {
        assert_eq!(
            item.attr("affiliation"),
            Some(affiliation),
            "unexpected affiliation in: {frame}"
        );
    }
}

fn assert_voice_change_presence(
    frame: &str,
    expected_from: &str,
    expected_role: &str,
    is_self: bool,
) {
    let presence = parse_frame(frame);
    assert_eq!(presence.name(), "presence", "expected <presence>: {frame}");
    assert_eq!(
        presence.attr("type"),
        None,
        "voice changes stay available: {frame}"
    );
    assert_eq!(
        presence.attr("from"),
        Some(expected_from),
        "unexpected voice-change sender: {frame}"
    );

    let muc_user = muc_user_payload(&presence);
    if is_self {
        assert!(
            has_status_code(muc_user, "110"),
            "self voice-change reflection must include status 110: {frame}"
        );
    }
    let item = muc_user_item(muc_user, frame);
    assert_eq!(
        item.attr("role"),
        Some(expected_role),
        "unexpected voice-change role in: {frame}"
    );
}

async fn make_members_only(admin: &mut WsXmppClient, room: &str) {
    let cfg_id = format!("cfg-members-only-{}", uuid::Uuid::new_v4());
    send_element(
        admin,
        owner_config_iq(room, &cfg_id, &[("muc#roomconfig_membersonly", "1")]),
    )
    .await;
    let _ = expect_iq_result(admin, &cfg_id).await;
}

async fn make_moderated(admin: &mut WsXmppClient, room: &str) {
    let cfg_id = format!("cfg-moderated-{}", uuid::Uuid::new_v4());
    send_element(
        admin,
        owner_config_iq(room, &cfg_id, &[("muc#roomconfig_moderatedroom", "1")]),
    )
    .await;
    let _ = expect_iq_result(admin, &cfg_id).await;
}

async fn grant_member(admin: &mut WsXmppClient, room: &str, jid: &str) {
    let iq_id = format!("grant-member-{}", uuid::Uuid::new_v4());
    send_element(
        admin,
        muc_admin_affiliation_iq(room, &iq_id, jid, "member", None),
    )
    .await;
    let _ = expect_iq_result(admin, &iq_id).await;
}

async fn grant_owner(admin: &mut WsXmppClient, room: &str, jid: &str) {
    let iq_id = format!("grant-owner-{}", uuid::Uuid::new_v4());
    send_element(
        admin,
        muc_admin_affiliation_iq(room, &iq_id, jid, "owner", None),
    )
    .await;
    let _ = expect_iq_result(admin, &iq_id).await;
}

#[tokio::test]
async fn xep_0045_self_kick_delivers_307_before_iq_result() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);

    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "self-kick-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "self-kick-alice").await;
    let room = format!("self-kick-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;
    settle_clients(&mut [&mut admin, &mut alice]).await;

    let kick_id = format!("self-kick-{}", uuid::Uuid::new_v4());
    send_element(
        &mut admin,
        muc_admin_role_iq(&room, &kick_id, ADMIN, "none", Some("self-kick-ordering")),
    )
    .await;

    let expected_from = format!("{room}/{ADMIN}");
    let self_kick = admin.recv().await.expect("self kick presence");
    assert_unavailable_admin_presence(&self_kick, &expected_from, "307", Some("none"), None, true);

    let _ = expect_iq_result(&mut admin, &kick_id).await;

    let alice_broadcast = alice.recv().await.expect("alice kick broadcast");
    assert_unavailable_admin_presence(
        &alice_broadcast,
        &expected_from,
        "307",
        Some("none"),
        None,
        false,
    );

    let _ = admin.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn xep_0045_kick_returns_iq_result_to_moderator_before_broadcast_copy() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);

    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "kick-order-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "kick-order-alice").await;
    let mut bob = connect(&server, BOB, &bob_pass, "kick-order-bob").await;
    let room = format!("kick-order-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;
    join_room(&mut bob, &room, BOB).await;
    settle_clients(&mut [&mut admin, &mut alice, &mut bob]).await;

    let kick_id = format!("kick-order-{}", uuid::Uuid::new_v4());
    send_element(
        &mut admin,
        muc_admin_role_iq(&room, &kick_id, BOB, "none", Some("spam")),
    )
    .await;

    let _ = expect_iq_result(&mut admin, &kick_id).await;

    let expected_from = format!("{room}/{BOB}");
    let bob_self = bob.recv().await.expect("bob self kick");
    assert_unavailable_admin_presence(&bob_self, &expected_from, "307", Some("none"), None, true);

    let alice_broadcast = alice.recv().await.expect("alice kick broadcast");
    assert_unavailable_admin_presence(
        &alice_broadcast,
        &expected_from,
        "307",
        Some("none"),
        None,
        false,
    );

    let admin_broadcast = admin.recv().await.expect("admin broadcast copy");
    assert_unavailable_admin_presence(
        &admin_broadcast,
        &expected_from,
        "307",
        Some("none"),
        None,
        false,
    );

    let _ = admin.close().await;
    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn xep_0045_ban_returns_iq_result_before_301_presences() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);

    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "ban-order-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "ban-order-alice").await;
    let mut bob = connect(&server, BOB, &bob_pass, "ban-order-bob").await;
    let room = format!("ban-order-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;
    join_room(&mut bob, &room, BOB).await;
    settle_clients(&mut [&mut admin, &mut alice, &mut bob]).await;

    let ban_id = format!("ban-order-{}", uuid::Uuid::new_v4());
    send_element(
        &mut admin,
        muc_admin_affiliation_iq(
            &room,
            &ban_id,
            &format!("{BOB}@{DOMAIN}"),
            "outcast",
            Some("cleanup"),
        ),
    )
    .await;

    let _ = expect_iq_result(&mut admin, &ban_id).await;

    let expected_from = format!("{room}/{BOB}");
    let bob_self = bob.recv().await.expect("bob self ban");
    assert_unavailable_admin_presence(
        &bob_self,
        &expected_from,
        "301",
        Some("none"),
        Some("outcast"),
        true,
    );

    let alice_broadcast = alice.recv().await.expect("alice ban broadcast");
    assert_unavailable_admin_presence(
        &alice_broadcast,
        &expected_from,
        "301",
        Some("none"),
        Some("outcast"),
        false,
    );

    let admin_broadcast = admin.recv().await.expect("admin ban broadcast copy");
    assert_unavailable_admin_presence(
        &admin_broadcast,
        &expected_from,
        "301",
        Some("none"),
        Some("outcast"),
        false,
    );

    let _ = admin.close().await;
    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn xep_0045_membership_loss_returns_iq_result_before_321_presences() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);

    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "member-loss-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "member-loss-alice").await;
    let mut bob = connect(&server, BOB, &bob_pass, "member-loss-bob").await;
    let room = format!("member-loss-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    join_room(&mut admin, &room, ADMIN).await;
    settle_clients(&mut [&mut admin]).await;

    make_members_only(&mut admin, &room).await;
    settle_clients(&mut [&mut admin]).await;

    grant_member(&mut admin, &room, &format!("{ALICE}@{DOMAIN}")).await;
    grant_member(&mut admin, &room, &format!("{BOB}@{DOMAIN}")).await;
    settle_clients(&mut [&mut admin]).await;

    join_room(&mut alice, &room, ALICE).await;
    join_room(&mut bob, &room, BOB).await;
    settle_clients(&mut [&mut admin, &mut alice, &mut bob]).await;

    let remove_id = format!("member-loss-{}", uuid::Uuid::new_v4());
    send_element(
        &mut admin,
        muc_admin_affiliation_iq(&room, &remove_id, &format!("{BOB}@{DOMAIN}"), "none", None),
    )
    .await;

    let _ = expect_iq_result(&mut admin, &remove_id).await;

    let expected_from = format!("{room}/{BOB}");
    let bob_self = bob.recv().await.expect("bob self removal");
    assert_unavailable_admin_presence(
        &bob_self,
        &expected_from,
        "321",
        Some("none"),
        Some("none"),
        true,
    );

    let alice_broadcast = alice.recv().await.expect("alice membership-loss broadcast");
    assert_unavailable_admin_presence(
        &alice_broadcast,
        &expected_from,
        "321",
        Some("none"),
        Some("none"),
        false,
    );

    let admin_broadcast = admin
        .recv()
        .await
        .expect("admin membership-loss broadcast copy");
    assert_unavailable_admin_presence(
        &admin_broadcast,
        &expected_from,
        "321",
        Some("none"),
        Some("none"),
        false,
    );

    let _ = admin.close().await;
    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn xep_0045_self_membership_loss_delivers_321_before_iq_result() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);

    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "self-member-loss-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "self-member-loss-alice").await;
    let room = format!("self-member-loss-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    join_room(&mut admin, &room, ADMIN).await;
    settle_clients(&mut [&mut admin]).await;

    make_members_only(&mut admin, &room).await;
    settle_clients(&mut [&mut admin]).await;

    grant_member(&mut admin, &room, &format!("{ALICE}@{DOMAIN}")).await;
    settle_clients(&mut [&mut admin]).await;

    join_room(&mut alice, &room, ALICE).await;
    settle_clients(&mut [&mut admin, &mut alice]).await;

    grant_owner(&mut admin, &room, &format!("{ALICE}@{DOMAIN}")).await;
    settle_clients(&mut [&mut admin, &mut alice]).await;

    let remove_id = format!("self-member-loss-{}", uuid::Uuid::new_v4());
    send_element(
        &mut admin,
        muc_admin_affiliation_iq(
            &room,
            &remove_id,
            &format!("{ADMIN}@{DOMAIN}"),
            "none",
            None,
        ),
    )
    .await;

    let expected_from = format!("{room}/{ADMIN}");
    let self_removal = admin.recv().await.expect("admin self removal");
    assert_unavailable_admin_presence(
        &self_removal,
        &expected_from,
        "321",
        Some("none"),
        Some("none"),
        true,
    );

    let _ = expect_iq_result(&mut admin, &remove_id).await;

    let alice_broadcast = alice.recv().await.expect("alice self-removal broadcast");
    assert_unavailable_admin_presence(
        &alice_broadcast,
        &expected_from,
        "321",
        Some("none"),
        Some("none"),
        false,
    );

    let _ = admin.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn xep_0045_voice_change_returns_iq_result_before_role_update_presence() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);

    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "voice-order-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "voice-order-alice").await;
    let mut bob = connect(&server, BOB, &bob_pass, "voice-order-bob").await;
    let room = format!("voice-order-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    join_room(&mut admin, &room, ADMIN).await;
    settle_clients(&mut [&mut admin]).await;

    make_moderated(&mut admin, &room).await;
    settle_clients(&mut [&mut admin]).await;

    join_room(&mut alice, &room, ALICE).await;
    join_room(&mut bob, &room, BOB).await;
    settle_clients(&mut [&mut admin, &mut alice, &mut bob]).await;

    let voice_id = format!("voice-order-{}", uuid::Uuid::new_v4());
    send_element(
        &mut admin,
        muc_admin_role_iq(&room, &voice_id, BOB, "participant", None),
    )
    .await;

    let _ = expect_iq_result(&mut admin, &voice_id).await;

    let expected_from = format!("{room}/{BOB}");
    let bob_self = bob.recv().await.expect("bob self voice-change reflection");
    assert_voice_change_presence(&bob_self, &expected_from, "participant", true);

    let alice_broadcast = alice.recv().await.expect("alice voice-change broadcast");
    assert_voice_change_presence(&alice_broadcast, &expected_from, "participant", false);

    let admin_broadcast = admin
        .recv()
        .await
        .expect("admin voice-change broadcast copy");
    assert_voice_change_presence(&admin_broadcast, &expected_from, "participant", false);

    let _ = admin.close().await;
    let _ = alice.close().await;
    let _ = bob.close().await;
}
