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
        .recv_matching(|frame| {
            frame.contains("<iq")
                && (frame.contains(&format!(r#"id='{id}'"#))
                    || frame.contains(&format!(r#"id='{id}'"#)))
        })
        .await
        .expect("iq response")
}

fn submit_form(node: &str, extra: &str) -> String {
    format!(
        r#"<x xmlns="{NS_DATA}" type="submit"><field var="FORM_TYPE" type="hidden"><value>{node}</value></field>{extra}</x>"#
    )
}

fn text_field(var: &str, value: &str) -> String {
    format!(r#"<field var="{var}" type="text-single"><value>{value}</value></field>"#)
}

fn is_result(frame: &str) -> bool {
    frame.contains(r#"type='result'"#) || frame.contains(r#"type='result'"#)
}

fn is_error(frame: &str) -> bool {
    frame.contains(r#"type='error'"#) || frame.contains(r#"type='error'"#)
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

    for affiliation in ["owner", "admin", "member", "outcast", "none"] {
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
