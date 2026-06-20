//! Integration suite for admin V2 `urn:waddle:admin:channels:*` commands.
//!
//! Covers the eight commands end-to-end over the production WebSocket
//! transport:
//!
//! - owner-only ACL on every command,
//! - `channels:create` mints a managed MUC room with the spec defaults
//!   (public, persistent, not members-only),
//! - `channels:list` reflects the new room,
//! - `channels:update` round-trips name/topic/is_public,
//! - `channels:delete` requires `confirm='yes'`,
//! - `channels:affiliations` exposes the persistent affiliation roster,
//! - `channels:set-affiliation` round-trips owner/admin/member/none/outcast,
//! - `channels:occupants` returns an empty list for a freshly-created room
//!   (no live occupants without a join),
//! - `channels:kick` is a no-op on a room with no occupant matching the
//!   given JID; the wire surface remains stable.
//! - `channels:kick` against a live occupant fires the XEP-0045 §9.1.1
//!   `<status code='307'/>` presence broadcast to every remaining
//!   occupant *and* the kicked occupant, even when the admin V2 caller
//!   is not joined to the room.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA: &str = "jabber:x:data";

const NODE_CHANNELS_LIST: &str = "urn:waddle:admin:channels:list:0";
const NODE_CHANNELS_CREATE: &str = "urn:waddle:admin:channels:create:0";
const NODE_CHANNELS_UPDATE: &str = "urn:waddle:admin:channels:update:0";
const NODE_CHANNELS_DELETE: &str = "urn:waddle:admin:channels:delete:0";
const NODE_CHANNELS_OCCUPANTS: &str = "urn:waddle:admin:channels:occupants:0";
const NODE_CHANNELS_AFFILIATIONS: &str = "urn:waddle:admin:channels:affiliations:0";
const NODE_CHANNELS_SET_AFFILIATION: &str = "urn:waddle:admin:channels:set-affiliation:0";
const NODE_CHANNELS_KICK: &str = "urn:waddle:admin:channels:kick:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn frame_has_iq_id(frame: &str, id: &str) -> bool {
    frame.contains(&format!(r#"id='{id}'"#)) || frame.contains(&format!(r#"id="{id}""#))
}

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ADMIN, &password, resource)
        .await
        .expect("admin connect")
}

async fn alice_client(server: &TestServer, password: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, "alice", password, resource)
        .await
        .expect("alice connect")
}

async fn send_command(client: &mut WsXmppClient, node: &str, id: &str, form_xml: &str) -> String {
    let body = format!(
        r#"<command xmlns="{NS_COMMANDS}" node="{node}" action="execute">{form_xml}</command>"#
    );
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{DOMAIN}">{body}</iq>"#
        ))
        .await
        .expect("send iq");
    client
        .recv_matching(|frame| frame.contains("<iq") && frame_has_iq_id(frame, id))
        .await
        .expect("iq response")
}

async fn disco_items(client: &mut WsXmppClient, to: &str, id: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="get" id="{id}" to="{to}"><query xmlns="http://jabber.org/protocol/disco#items"/></iq>"#
        ))
        .await
        .expect("send disco items");
    client
        .recv_matching(|frame| frame.contains("<iq") && frame_has_iq_id(frame, id))
        .await
        .expect("disco items response")
}

async fn join_muc(client: &mut WsXmppClient, room_jid: &str, nick: &str) -> String {
    client
        .send(&format!(
            r#"<presence to="{room_jid}/{nick}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send muc join presence");
    client
        .recv_matching(|frame| {
            frame.contains("<presence") && frame.contains(&format!("{room_jid}/{nick}"))
        })
        .await
        .expect("muc self-presence")
}

async fn submit_muc_owner_config(
    client: &mut WsXmppClient,
    room_jid: &str,
    id: &str,
    fields_xml: &str,
) -> String {
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{room_jid}">
                <query xmlns="http://jabber.org/protocol/muc#owner">
                    <x xmlns="{NS_DATA}" type="submit">
                        <field var="FORM_TYPE" type="hidden">
                            <value>http://jabber.org/protocol/muc#roomconfig</value>
                        </field>
                        {fields_xml}
                    </x>
                </query>
            </iq>"#
        ))
        .await
        .expect("send owner config iq");
    client
        .recv_matching(|frame| frame.contains("<iq") && frame_has_iq_id(frame, id))
        .await
        .expect("owner config response")
}

fn submit_form(node: &str, extra: &str) -> String {
    format!(
        r#"<x xmlns="{NS_DATA}" type="submit"><field var="FORM_TYPE" type="hidden"><value>{node}</value></field>{extra}</x>"#
    )
}

fn text_field(var: &str, value: &str) -> String {
    format!(r#"<field var="{var}" type="text-single"><value>{value}</value></field>"#)
}

fn bool_field(var: &str, value: bool) -> String {
    let value = if value { "1" } else { "0" };
    format!(r#"<field var="{var}" type="boolean"><value>{value}</value></field>"#)
}

fn is_result(frame: &str) -> bool {
    frame.contains(r#"type='result'"#) || frame.contains(r#"type="result""#)
}

fn is_error(frame: &str) -> bool {
    frame.contains(r#"type='error'"#) || frame.contains(r#"type="error""#)
}

fn extract_field(frame: &str, var: &str) -> Option<String> {
    let marker_dq = format!(r#"var='{var}'"#);
    let marker_sq = format!(r#"var='{var}'"#);
    let idx = frame.find(&marker_dq).or_else(|| frame.find(&marker_sq))?;
    let after = &frame[idx..];
    let open = after.find("<value>")?;
    let inner = &after[open + "<value>".len()..];
    let close = inner.find("</value>")?;
    Some(inner[..close].to_string())
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn channels_list_returns_seeded_channels_for_fresh_server() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-list-1").await;
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_LIST,
        "channels-list-seeded",
        &submit_form(NODE_CHANNELS_LIST, ""),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    // Server seeds INITIAL_MANAGED_CHANNELS on startup; the admin list
    // surfaces them so the panel always has rows even before any
    // explicit `channels:create`.
    assert!(
        resp.contains("<item>"),
        "fresh server should surface seeded channels, got: {resp}"
    );
    assert!(
        resp.contains("@muc.localhost"),
        "expected at least one channel JID in muc domain, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_list_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-channels-list-1").await;
    let resp = send_command(
        &mut alice,
        NODE_CHANNELS_LIST,
        "channels-list-forbidden",
        &submit_form(NODE_CHANNELS_LIST, ""),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
    let _ = alice.close().await;
}

// ---------------------------------------------------------------------------
// create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn channels_create_defaults_to_public() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-create-1").await;
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-create-default",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", "general")),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    let channel_jid = extract_field(&resp, "channel_jid").expect("channel_jid in response");
    assert!(
        channel_jid.contains("@"),
        "expected bare JID, got: {channel_jid}"
    );

    // is_public field should be 1 (true)
    let is_public = extract_field(&resp, "is_public").expect("is_public");
    assert_eq!(is_public, "1", "default is_public must be true");
    let members_only = extract_field(&resp, "members_only").expect("members_only");
    assert_eq!(members_only, "0", "default members_only must be false");

    // List should reflect the new channel.
    let list_resp = send_command(
        &mut admin,
        NODE_CHANNELS_LIST,
        "channels-create-list",
        &submit_form(NODE_CHANNELS_LIST, ""),
    )
    .await;
    assert!(
        list_resp.contains(&channel_jid),
        "list missing created channel JID '{channel_jid}', got: {list_resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_create_honours_explicit_private() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-create-2").await;
    let extra = format!(
        "{}{}",
        text_field("name", "secret"),
        text_field("is_public", "false")
    );
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-create-private",
        &submit_form(NODE_CHANNELS_CREATE, &extra),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    let is_public = extract_field(&resp, "is_public").expect("is_public");
    assert_eq!(is_public, "0", "is_public=false honoured");
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_create_public_room_appears_in_muc_disco_items() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-create-disco").await;
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-create-disco",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", "native-visible")),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    let channel_jid = extract_field(&resp, "channel_jid").expect("channel_jid");

    let disco = disco_items(&mut admin, "muc.localhost", "channels-create-muc-disco").await;
    assert!(
        disco.contains(&channel_jid),
        "admin-created public channel must be discoverable via MUC disco#items, got: {disco}"
    );
    assert!(
        disco.contains("native-visible"),
        "MUC disco item should carry the channel name, got: {disco}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_open_managed_room_join_keeps_unaffiliated_user_none() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut admin = admin_client(&server, "admin-channels-open-join-admin").await;
    let mut alice = alice_client(&server, &alice_pass, "alice-channels-open-join").await;
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-open-join-create",
        &submit_form(
            NODE_CHANNELS_CREATE,
            &format!(
                "{}{}",
                text_field("name", "open-join"),
                bool_field("members_only", false)
            ),
        ),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    let channel_jid = extract_field(&resp, "channel_jid").expect("channel_jid");

    let join = join_muc(&mut alice, &channel_jid, "alice").await;
    assert!(
        join.contains("affiliation='none'") || join.contains(r#"affiliation="none""#),
        "unaffiliated open managed-room join must not persist member affiliation, got: {join}"
    );

    let list = send_command(
        &mut admin,
        NODE_CHANNELS_AFFILIATIONS,
        "channels-open-join-affiliations",
        &submit_form(
            NODE_CHANNELS_AFFILIATIONS,
            &text_field("channel_jid", &channel_jid),
        ),
    )
    .await;
    assert!(
        is_result(&list),
        "expected affiliations result, got: {list}"
    );
    assert!(
        !list.contains("alice@localhost"),
        "joining an open managed room must not add alice to the managed affiliation list, got: {list}"
    );

    let _ = alice.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_create_private_room_is_hidden_from_muc_disco_items() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-create-private-disco").await;
    let extra = format!(
        "{}{}",
        text_field("name", "private-native"),
        text_field("is_public", "false")
    );
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-create-private-disco",
        &submit_form(NODE_CHANNELS_CREATE, &extra),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    let channel_jid = extract_field(&resp, "channel_jid").expect("channel_jid");

    let disco = disco_items(
        &mut admin,
        "muc.localhost",
        "channels-create-private-muc-disco",
    )
    .await;
    assert!(
        !disco.contains(&channel_jid),
        "admin-created private channel must not be public in MUC disco#items, got: {disco}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_create_public_members_only_room_appears_in_muc_disco_items() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-create-public-members-disco").await;
    let extra = format!(
        "{}{}{}",
        text_field("name", "public-members"),
        bool_field("is_public", true),
        bool_field("members_only", true)
    );
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-create-public-members-disco",
        &submit_form(NODE_CHANNELS_CREATE, &extra),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    assert_eq!(
        extract_field(&resp, "is_public").as_deref(),
        Some("1"),
        "public_room=true should be reported independently"
    );
    assert_eq!(
        extract_field(&resp, "members_only").as_deref(),
        Some("1"),
        "members_only=true should be reported independently"
    );
    let channel_jid = extract_field(&resp, "channel_jid").expect("channel_jid");

    let disco = disco_items(
        &mut admin,
        "muc.localhost",
        "channels-create-public-members-muc-disco",
    )
    .await;
    assert!(
        disco.contains(&channel_jid),
        "XEP-0045 publicroom=true must be discoverable even when members_only=true, got: {disco}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_create_hidden_open_room_is_hidden_from_muc_disco_items() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-create-hidden-open-disco").await;
    let extra = format!(
        "{}{}{}",
        text_field("name", "hidden-open"),
        bool_field("is_public", false),
        bool_field("members_only", false)
    );
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-create-hidden-open-disco",
        &submit_form(NODE_CHANNELS_CREATE, &extra),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    assert_eq!(
        extract_field(&resp, "is_public").as_deref(),
        Some("0"),
        "public_room=false should be reported independently"
    );
    assert_eq!(
        extract_field(&resp, "members_only").as_deref(),
        Some("0"),
        "members_only=false should be reported independently"
    );
    let channel_jid = extract_field(&resp, "channel_jid").expect("channel_jid");

    let disco = disco_items(
        &mut admin,
        "muc.localhost",
        "channels-create-hidden-open-muc-disco",
    )
    .await;
    assert!(
        !disco.contains(&channel_jid),
        "XEP-0045 publicroom=false must be hidden even when members_only=false, got: {disco}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_create_rejects_invalid_name() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-create-bad").await;
    let long = "a".repeat(81);
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-create-long",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", &long)),
    )
    .await;
    assert!(
        is_error(&resp),
        "expected error for overlong name, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_create_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-channels-create-1").await;
    let resp = send_command(
        &mut alice,
        NODE_CHANNELS_CREATE,
        "channels-create-forbidden",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", "rogue")),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
    let _ = alice.close().await;
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn channels_update_renames() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-update-1").await;
    let create_resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-upd-create",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", "oldname")),
    )
    .await;
    let channel_jid = extract_field(&create_resp, "channel_jid").expect("channel_jid");

    let extra = format!(
        "{}{}",
        text_field("channel_jid", &channel_jid),
        text_field("name", "newname")
    );
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_UPDATE,
        "channels-upd-rename",
        &submit_form(NODE_CHANNELS_UPDATE, &extra),
    )
    .await;
    assert!(is_result(&resp), "expected update result, got: {resp}");
    assert!(
        resp.contains("newname"),
        "renamed channel should reflect new name, got: {resp}"
    );
    let disco = disco_items(&mut admin, "muc.localhost", "channels-upd-disco").await;
    assert!(
        disco.contains(&channel_jid) && disco.contains("newname"),
        "MUC disco#items should reflect updated channel name, got: {disco}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_update_private_then_kick_blocks_non_member_rejoin() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut admin = admin_client(&server, "admin-channels-private-kick").await;
    let mut alice = alice_client(&server, &alice_pass, "alice-channels-private-kick").await;

    let create_resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-private-kick-create",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", "private-kick")),
    )
    .await;
    assert!(
        is_result(&create_resp),
        "expected create result, got: {create_resp}"
    );
    let channel_jid = extract_field(&create_resp, "channel_jid").expect("channel_jid");

    let initial_join = join_muc(&mut alice, &channel_jid, "alice").await;
    assert!(
        initial_join.contains("affiliation='none'")
            || initial_join.contains(r#"affiliation="none""#),
        "public/open room should admit alice without granting membership, got: {initial_join}"
    );

    let update_extra = format!(
        "{}{}{}",
        text_field("channel_jid", &channel_jid),
        bool_field("is_public", false),
        bool_field("members_only", true),
    );
    let update_resp = send_command(
        &mut admin,
        NODE_CHANNELS_UPDATE,
        "channels-private-kick-update",
        &submit_form(NODE_CHANNELS_UPDATE, &update_extra),
    )
    .await;
    assert!(
        is_result(&update_resp),
        "expected update result, got: {update_resp}"
    );
    assert_eq!(
        extract_field(&update_resp, "is_public").as_deref(),
        Some("0")
    );
    assert_eq!(
        extract_field(&update_resp, "members_only").as_deref(),
        Some("1")
    );

    let privacy_ejection = alice
        .recv_matching(|frame| {
            frame.contains("<presence")
                && frame.contains(&channel_jid)
                && frame.contains("alice")
                && frame.contains("322")
        })
        .await
        .expect("members-only conversion ejection presence");
    assert!(
        privacy_ejection.contains("type='unavailable'")
            || privacy_ejection.contains(r#"type="unavailable""#),
        "members-only conversion must remove current non-members, got: {privacy_ejection}"
    );

    let kick_extra = format!(
        "{}{}",
        text_field("channel_jid", &channel_jid),
        text_field("occupant_jid", "alice@localhost"),
    );
    let kick_resp = send_command(
        &mut admin,
        NODE_CHANNELS_KICK,
        "channels-private-kick-go",
        &submit_form(NODE_CHANNELS_KICK, &kick_extra),
    )
    .await;
    assert!(
        is_result(&kick_resp),
        "kick after members-only ejection should be a stable no-op result, got: {kick_resp}"
    );

    let denied_rejoin = join_muc(&mut alice, &channel_jid, "alice").await;
    assert!(
        denied_rejoin.contains("registration-required"),
        "members-only room must deny a kicked non-member rejoin with XEP-0045 registration-required, got: {denied_rejoin}"
    );

    let grant_extra = format!(
        "{}{}{}",
        text_field("channel_jid", &channel_jid),
        text_field("member_jid", "alice@localhost"),
        text_field("affiliation", "member"),
    );
    let grant_resp = send_command(
        &mut admin,
        NODE_CHANNELS_SET_AFFILIATION,
        "channels-private-kick-grant",
        &submit_form(NODE_CHANNELS_SET_AFFILIATION, &grant_extra),
    )
    .await;
    assert!(
        is_result(&grant_resp),
        "expected member grant result, got: {grant_resp}"
    );

    let admitted_join = join_muc(&mut alice, &channel_jid, "alice").await;
    assert!(
        admitted_join.contains("affiliation='member'")
            || admitted_join.contains(r#"affiliation="member""#),
        "explicit member affiliation should admit alice after private conversion, got: {admitted_join}"
    );

    let kick_member_resp = send_command(
        &mut admin,
        NODE_CHANNELS_KICK,
        "channels-private-kick-member",
        &submit_form(NODE_CHANNELS_KICK, &kick_extra),
    )
    .await;
    assert!(
        is_result(&kick_member_resp),
        "kick should succeed for an explicit private-room member, got: {kick_member_resp}"
    );
    let member_kick_presence = alice
        .recv_matching(|frame| {
            frame.contains("<presence")
                && frame.contains(&channel_jid)
                && frame.contains("alice")
                && frame.contains("307")
        })
        .await
        .expect("explicit member kick presence");
    assert!(
        member_kick_presence.contains("type='unavailable'")
            || member_kick_presence.contains(r#"type="unavailable""#),
        "kicking an explicit member should still use XEP-0045 kick presence, got: {member_kick_presence}"
    );

    let denied_member_rejoin = join_muc(&mut alice, &channel_jid, "alice").await;
    assert!(
        denied_member_rejoin.contains("registration-required"),
        "members-only room must deny an explicit member after admin-panel kick revoked membership, got: {denied_member_rejoin}"
    );

    let offline_grant_resp = send_command(
        &mut admin,
        NODE_CHANNELS_SET_AFFILIATION,
        "channels-private-kick-offline-grant",
        &submit_form(NODE_CHANNELS_SET_AFFILIATION, &grant_extra),
    )
    .await;
    assert!(
        is_result(&offline_grant_resp),
        "expected offline member grant result, got: {offline_grant_resp}"
    );
    let offline_kick_resp = send_command(
        &mut admin,
        NODE_CHANNELS_KICK,
        "channels-private-kick-offline-member",
        &submit_form(NODE_CHANNELS_KICK, &kick_extra),
    )
    .await;
    assert!(
        is_result(&offline_kick_resp),
        "offline private member kick should revoke membership as a stable result, got: {offline_kick_resp}"
    );
    let denied_offline_member_rejoin = join_muc(&mut alice, &channel_jid, "alice").await;
    assert!(
        denied_offline_member_rejoin.contains("registration-required"),
        "members-only room must deny rejoin after offline private-member kick revoked membership, got: {denied_offline_member_rejoin}"
    );

    let _ = alice.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_revoke_member_in_private_room_ejects_active_occupant() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut admin = admin_client(&server, "admin-channels-revoke-member").await;
    let mut alice = alice_client(&server, &alice_pass, "alice-channels-revoke-member").await;

    let create_extra = format!(
        "{}{}{}",
        text_field("name", "private-revoke"),
        bool_field("is_public", false),
        bool_field("members_only", true),
    );
    let create_resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-private-revoke-create",
        &submit_form(NODE_CHANNELS_CREATE, &create_extra),
    )
    .await;
    assert!(
        is_result(&create_resp),
        "expected create result, got: {create_resp}"
    );
    let channel_jid = extract_field(&create_resp, "channel_jid").expect("channel_jid");

    let grant_extra = format!(
        "{}{}{}",
        text_field("channel_jid", &channel_jid),
        text_field("member_jid", "alice@localhost"),
        text_field("affiliation", "member"),
    );
    let grant_resp = send_command(
        &mut admin,
        NODE_CHANNELS_SET_AFFILIATION,
        "channels-private-revoke-grant",
        &submit_form(NODE_CHANNELS_SET_AFFILIATION, &grant_extra),
    )
    .await;
    assert!(
        is_result(&grant_resp),
        "expected grant result, got: {grant_resp}"
    );

    let admitted_join = join_muc(&mut alice, &channel_jid, "alice").await;
    assert!(
        admitted_join.contains("affiliation='member'")
            || admitted_join.contains(r#"affiliation="member""#),
        "explicit member affiliation should admit alice, got: {admitted_join}"
    );

    let revoke_extra = format!(
        "{}{}{}",
        text_field("channel_jid", &channel_jid),
        text_field("member_jid", "alice@localhost"),
        text_field("affiliation", "none"),
    );
    let revoke_resp = send_command(
        &mut admin,
        NODE_CHANNELS_SET_AFFILIATION,
        "channels-private-revoke-none",
        &submit_form(NODE_CHANNELS_SET_AFFILIATION, &revoke_extra),
    )
    .await;
    assert!(
        is_result(&revoke_resp),
        "expected revoke result, got: {revoke_resp}"
    );

    let revoked = alice
        .recv_matching(|frame| {
            frame.contains("<presence")
                && frame.contains(&channel_jid)
                && frame.contains("alice")
                && frame.contains("321")
        })
        .await
        .expect("membership revocation ejection presence");
    assert!(
        revoked.contains("type='unavailable'") || revoked.contains(r#"type="unavailable""#),
        "membership revocation must remove active occupants in private rooms, got: {revoked}"
    );

    let denied_rejoin = join_muc(&mut alice, &channel_jid, "alice").await;
    assert!(
        denied_rejoin.contains("registration-required"),
        "revoked member must be denied rejoin to members-only room, got: {denied_rejoin}"
    );

    let _ = alice.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn standard_muc_owner_config_publicroom_false_hides_room_from_muc_disco_items() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-owner-publicroom").await;
    let channel_jid = "owner-hidden@muc.localhost";
    let join = join_muc(&mut admin, channel_jid, "admin").await;
    assert!(
        join.contains("affiliation='owner'") || join.contains(r#"affiliation="owner""#),
        "first occupant should become MUC owner before config SET, got: {join}"
    );

    let owner_visible_resp = submit_muc_owner_config(
        &mut admin,
        channel_jid,
        "channels-owner-publicroom-visible-set",
        &format!(
            "{}{}{}",
            text_field("muc#roomconfig_roomname", "Owner Hidden"),
            bool_field("muc#roomconfig_publicroom", true),
            bool_field("muc#roomconfig_membersonly", false)
        ),
    )
    .await;
    assert!(
        is_result(&owner_visible_resp),
        "expected owner config result, got: {owner_visible_resp}"
    );
    let visible = disco_items(
        &mut admin,
        "muc.localhost",
        "channels-owner-publicroom-before-disco",
    )
    .await;
    assert!(
        visible.contains(channel_jid),
        "precondition: newly created public room should be visible, got: {visible}"
    );

    let owner_resp = submit_muc_owner_config(
        &mut admin,
        channel_jid,
        "channels-owner-publicroom-set",
        &bool_field("muc#roomconfig_publicroom", false),
    )
    .await;
    assert!(
        is_result(&owner_resp),
        "expected owner config result, got: {owner_resp}"
    );

    let hidden = disco_items(
        &mut admin,
        "muc.localhost",
        "channels-owner-publicroom-after-disco",
    )
    .await;
    assert!(
        !hidden.contains(channel_jid),
        "standard XEP-0045 muc#roomconfig_publicroom=0 must hide room from MUC disco#items, got: {hidden}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_update_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-channels-update-1").await;
    let extra = format!(
        "{}{}",
        text_field("channel_jid", "general@muc.localhost"),
        text_field("name", "hijack")
    );
    let resp = send_command(
        &mut alice,
        NODE_CHANNELS_UPDATE,
        "channels-upd-forbidden",
        &submit_form(NODE_CHANNELS_UPDATE, &extra),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
    let _ = alice.close().await;
}

// ---------------------------------------------------------------------------
// delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn channels_delete_requires_confirm_yes() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-del-1").await;
    let extra = format!(
        "{}{}",
        text_field("channel_jid", "general@muc.localhost"),
        text_field("confirm", "no")
    );
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_DELETE,
        "channels-del-noconfirm",
        &submit_form(NODE_CHANNELS_DELETE, &extra),
    )
    .await;
    assert!(
        is_error(&resp),
        "expected error without confirm, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_delete_round_trips() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-del-2").await;
    let create_resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-del2-create",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", "disposable")),
    )
    .await;
    let channel_jid = extract_field(&create_resp, "channel_jid").expect("channel_jid");
    let extra = format!(
        "{}{}",
        text_field("channel_jid", &channel_jid),
        text_field("confirm", "yes")
    );
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_DELETE,
        "channels-del2-go",
        &submit_form(NODE_CHANNELS_DELETE, &extra),
    )
    .await;
    assert!(is_result(&resp), "expected delete success, got: {resp}");

    let list_resp = send_command(
        &mut admin,
        NODE_CHANNELS_LIST,
        "channels-del2-list",
        &submit_form(NODE_CHANNELS_LIST, ""),
    )
    .await;
    assert!(
        !list_resp.contains(&channel_jid),
        "deleted channel should be gone from list, got: {list_resp}"
    );
    let disco = disco_items(&mut admin, "muc.localhost", "channels-del2-disco").await;
    assert!(
        !disco.contains(&channel_jid),
        "deleted channel should be gone from MUC disco#items, got: {disco}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_delete_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-channels-del-1").await;
    let extra = format!(
        "{}{}",
        text_field("channel_jid", "general@muc.localhost"),
        text_field("confirm", "yes")
    );
    let resp = send_command(
        &mut alice,
        NODE_CHANNELS_DELETE,
        "channels-del-forbidden",
        &submit_form(NODE_CHANNELS_DELETE, &extra),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
    let _ = alice.close().await;
}

// ---------------------------------------------------------------------------
// occupants / affiliations / set-affiliation / kick
// ---------------------------------------------------------------------------

#[tokio::test]
async fn channels_occupants_empty_for_fresh_room() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-occ-1").await;
    let create_resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-occ-create",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", "empty")),
    )
    .await;
    let channel_jid = extract_field(&create_resp, "channel_jid").expect("channel_jid");
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_OCCUPANTS,
        "channels-occ-go",
        &submit_form(
            NODE_CHANNELS_OCCUPANTS,
            &text_field("channel_jid", &channel_jid),
        ),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    assert!(
        !resp.contains("<item>"),
        "fresh room should have no occupants, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_occupants_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-channels-occ-1").await;
    let resp = send_command(
        &mut alice,
        NODE_CHANNELS_OCCUPANTS,
        "channels-occ-forbidden",
        &submit_form(
            NODE_CHANNELS_OCCUPANTS,
            &text_field("channel_jid", "general@muc.localhost"),
        ),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
    let _ = alice.close().await;
}

#[tokio::test]
async fn channels_set_affiliation_round_trips() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-aff-1").await;
    let create_resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-aff-create",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", "aff-test")),
    )
    .await;
    let channel_jid = extract_field(&create_resp, "channel_jid").expect("channel_jid");

    for affiliation in ["admin", "member", "outcast", "none", "owner"] {
        let extra = format!(
            "{}{}{}",
            text_field("channel_jid", &channel_jid),
            text_field("member_jid", "alice@localhost"),
            text_field("affiliation", affiliation),
        );
        let resp = send_command(
            &mut admin,
            NODE_CHANNELS_SET_AFFILIATION,
            &format!("channels-aff-{affiliation}"),
            &submit_form(NODE_CHANNELS_SET_AFFILIATION, &extra),
        )
        .await;
        assert!(
            is_result(&resp),
            "expected set-affiliation {affiliation} success, got: {resp}"
        );
    }
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_affiliations_reflects_assignment() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-affls-1").await;
    let create_resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-affls-create",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", "affls")),
    )
    .await;
    let channel_jid = extract_field(&create_resp, "channel_jid").expect("channel_jid");

    // Promote bystander to admin.
    let promote_extra = format!(
        "{}{}{}",
        text_field("channel_jid", &channel_jid),
        text_field("member_jid", "bystander@localhost"),
        text_field("affiliation", "admin"),
    );
    let _ = send_command(
        &mut admin,
        NODE_CHANNELS_SET_AFFILIATION,
        "channels-affls-promote",
        &submit_form(NODE_CHANNELS_SET_AFFILIATION, &promote_extra),
    )
    .await;

    // List affiliations.
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_AFFILIATIONS,
        "channels-affls-list",
        &submit_form(
            NODE_CHANNELS_AFFILIATIONS,
            &text_field("channel_jid", &channel_jid),
        ),
    )
    .await;
    assert!(
        is_result(&resp),
        "expected affiliations result, got: {resp}"
    );
    assert!(
        resp.contains("bystander@localhost"),
        "affiliations list should include promoted user, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_affiliations_filter_narrows() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-filter-1").await;
    let create_resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-filter-create",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", "filter-test")),
    )
    .await;
    let channel_jid = extract_field(&create_resp, "channel_jid").expect("channel_jid");

    // Ban one, promote another.
    for (jid, aff) in [("badguy@localhost", "outcast"), ("hero@localhost", "owner")] {
        let extra = format!(
            "{}{}{}",
            text_field("channel_jid", &channel_jid),
            text_field("member_jid", jid),
            text_field("affiliation", aff),
        );
        let _ = send_command(
            &mut admin,
            NODE_CHANNELS_SET_AFFILIATION,
            &format!("channels-filter-{aff}"),
            &submit_form(NODE_CHANNELS_SET_AFFILIATION, &extra),
        )
        .await;
    }

    // Filter to outcast only.
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_AFFILIATIONS,
        "channels-filter-outcast",
        &submit_form(
            NODE_CHANNELS_AFFILIATIONS,
            &format!(
                "{}{}",
                text_field("channel_jid", &channel_jid),
                text_field("filter", "outcast")
            ),
        ),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    assert!(
        resp.contains("badguy@localhost"),
        "outcast filter should include badguy, got: {resp}"
    );
    assert!(
        !resp.contains("hero@localhost"),
        "outcast filter should exclude owners, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_affiliations_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-channels-affls-1").await;
    let resp = send_command(
        &mut alice,
        NODE_CHANNELS_AFFILIATIONS,
        "channels-affls-forbidden",
        &submit_form(
            NODE_CHANNELS_AFFILIATIONS,
            &text_field("channel_jid", "general@muc.localhost"),
        ),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
    let _ = alice.close().await;
}

#[tokio::test]
async fn channels_set_affiliation_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-channels-aff-1").await;
    let extra = format!(
        "{}{}{}",
        text_field("channel_jid", "general@muc.localhost"),
        text_field("member_jid", "alice@localhost"),
        text_field("affiliation", "owner"),
    );
    let resp = send_command(
        &mut alice,
        NODE_CHANNELS_SET_AFFILIATION,
        "channels-aff-forbidden",
        &submit_form(NODE_CHANNELS_SET_AFFILIATION, &extra),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
    let _ = alice.close().await;
}

#[tokio::test]
async fn channels_kick_returns_result_for_unknown_occupant() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-channels-kick-1").await;
    let create_resp = send_command(
        &mut admin,
        NODE_CHANNELS_CREATE,
        "channels-kick-create",
        &submit_form(NODE_CHANNELS_CREATE, &text_field("name", "kick-test")),
    )
    .await;
    let channel_jid = extract_field(&create_resp, "channel_jid").expect("channel_jid");
    // No one's joined — kick still resolves successfully (best-effort
    // state mutation per the handler doc).
    let extra = format!(
        "{}{}",
        text_field("channel_jid", &channel_jid),
        text_field("occupant_jid", "nobody@localhost"),
    );
    let resp = send_command(
        &mut admin,
        NODE_CHANNELS_KICK,
        "channels-kick-noone",
        &submit_form(NODE_CHANNELS_KICK, &extra),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    assert!(
        resp.contains("nobody@localhost"),
        "kick result must echo occupant_jid, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn channels_kick_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-channels-kick-1").await;
    let extra = format!(
        "{}{}",
        text_field("channel_jid", "general@muc.localhost"),
        text_field("occupant_jid", "alice@localhost"),
    );
    let resp = send_command(
        &mut alice,
        NODE_CHANNELS_KICK,
        "channels-kick-forbidden",
        &submit_form(NODE_CHANNELS_KICK, &extra),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
    let _ = alice.close().await;
}
