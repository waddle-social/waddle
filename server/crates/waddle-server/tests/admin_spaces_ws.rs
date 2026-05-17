//! Integration suite for admin V2 `urn:waddle:admin:spaces:*` commands.
//!
//! Covers the wire surface end-to-end over the production WebSocket
//! transport:
//!
//! - owner-only ACL on every command,
//! - `spaces:create` mints a space + backing pubsub node,
//! - `spaces:list` paginates the metadata projection,
//! - `spaces:update` patches name/description/icon_url,
//! - `spaces:delete` requires `confirm='yes'`,
//! - `spaces:members` lists pubsub-affiliation membership,
//! - `spaces:set-role` round-trips through pubsub affiliations.
//!
//! Constants are inlined rather than pulled from the server crate so the
//! file documents the wire shape — a regression that flips a node
//! identifier has to flip it here too, which is the point.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA: &str = "jabber:x:data";

const NODE_SPACES_LIST: &str = "urn:waddle:admin:spaces:list:0";
const NODE_SPACES_CREATE: &str = "urn:waddle:admin:spaces:create:0";
const NODE_SPACES_UPDATE: &str = "urn:waddle:admin:spaces:update:0";
const NODE_SPACES_DELETE: &str = "urn:waddle:admin:spaces:delete:0";
const NODE_SPACES_MEMBERS: &str = "urn:waddle:admin:spaces:members:0";
const NODE_SPACES_SET_ROLE: &str = "urn:waddle:admin:spaces:set-role:0";

// Each TestServer spins up an ephemeral cert + listener; serialize the
// suite to avoid filesystem temp-port races (matches the V1 admin test).
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

async fn send_command(
    client: &mut WsXmppClient,
    node: &str,
    id: &str,
    form_xml: &str,
) -> String {
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
                && (frame.contains(&format!(r#"id="{id}""#))
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
    frame.contains(r#"type="result""#) || frame.contains(r#"type='result'"#)
}

fn is_error(frame: &str) -> bool {
    frame.contains(r#"type="error""#) || frame.contains(r#"type='error'"#)
}

/// Pull the value of `var=...` text-single field from the result form
/// — used to grab the space JID minted by `spaces:create`.
fn extract_field(frame: &str, var: &str) -> Option<String> {
    let marker_dq = format!(r#"var="{var}""#);
    let marker_sq = format!(r#"var='{var}'"#);
    let idx = frame.find(&marker_dq).or_else(|| frame.find(&marker_sq))?;
    let after = &frame[idx..];
    let open = after.find("<value>")?;
    let inner = &after[open + "<value>".len()..];
    let close = inner.find("</value>")?;
    Some(inner[..close].to_string())
}

// ---------------------------------------------------------------------------
// spaces:list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spaces_list_returns_empty_for_fresh_server() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-list-1").await;

    let resp = send_command(
        &mut admin,
        NODE_SPACES_LIST,
        "spaces-list-empty",
        &submit_form(NODE_SPACES_LIST, ""),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    assert!(
        !resp.contains("<item>"),
        "fresh server should have no spaces, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_list_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-spaces-list-1").await;

    let resp = send_command(
        &mut alice,
        NODE_SPACES_LIST,
        "spaces-list-forbidden",
        &submit_form(NODE_SPACES_LIST, ""),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(resp.contains("forbidden"), "expected <forbidden/>, got: {resp}");
    let _ = alice.close().await;
}

// ---------------------------------------------------------------------------
// spaces:create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spaces_create_persists_new_space() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-create-1").await;

    let create_extra = text_field("name", "Engineering");
    let resp = send_command(
        &mut admin,
        NODE_SPACES_CREATE,
        "spaces-create-ok",
        &submit_form(NODE_SPACES_CREATE, &create_extra),
    )
    .await;
    assert!(is_result(&resp), "expected result, got: {resp}");
    let space_jid = extract_field(&resp, "space_jid").expect("space_jid in response");
    assert!(space_jid.contains("@"), "expected bare JID, got: {space_jid}");

    // List should now include the new space.
    let list_resp = send_command(
        &mut admin,
        NODE_SPACES_LIST,
        "spaces-create-list",
        &submit_form(NODE_SPACES_LIST, ""),
    )
    .await;
    assert!(is_result(&list_resp), "expected list result, got: {list_resp}");
    assert!(
        list_resp.contains(&space_jid),
        "list missing the created space JID '{space_jid}', got: {list_resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_create_rejects_invalid_name() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-create-bad").await;

    // Empty name violates the 1-char minimum.
    let resp = send_command(
        &mut admin,
        NODE_SPACES_CREATE,
        "spaces-create-empty",
        &submit_form(NODE_SPACES_CREATE, &text_field("name", "")),
    )
    .await;
    assert!(is_error(&resp), "expected error for empty name, got: {resp}");

    // 81-char name violates the 80-char ceiling.
    let long = "a".repeat(81);
    let resp = send_command(
        &mut admin,
        NODE_SPACES_CREATE,
        "spaces-create-long",
        &submit_form(NODE_SPACES_CREATE, &text_field("name", &long)),
    )
    .await;
    assert!(is_error(&resp), "expected error for overlong name, got: {resp}");
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_create_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-spaces-create-1").await;
    let resp = send_command(
        &mut alice,
        NODE_SPACES_CREATE,
        "spaces-create-non-owner",
        &submit_form(NODE_SPACES_CREATE, &text_field("name", "Marketing")),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(resp.contains("forbidden"), "expected <forbidden/>, got: {resp}");
    let _ = alice.close().await;
}

// ---------------------------------------------------------------------------
// spaces:update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spaces_update_changes_name() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-update-1").await;

    // Seed a space.
    let create_resp = send_command(
        &mut admin,
        NODE_SPACES_CREATE,
        "spaces-upd-create",
        &submit_form(NODE_SPACES_CREATE, &text_field("name", "Eng")),
    )
    .await;
    let space_jid = extract_field(&create_resp, "space_jid").expect("space_jid");

    // Update its name.
    let update_extra = format!(
        "{}{}",
        text_field("space_jid", &space_jid),
        text_field("name", "Engineering Mk II")
    );
    let resp = send_command(
        &mut admin,
        NODE_SPACES_UPDATE,
        "spaces-upd-rename",
        &submit_form(NODE_SPACES_UPDATE, &update_extra),
    )
    .await;
    assert!(is_result(&resp), "expected update result, got: {resp}");
    assert!(
        resp.contains("Engineering Mk II"),
        "renamed space should reflect new name, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_update_rejects_unknown_space() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-update-2").await;
    let extra = format!(
        "{}{}",
        text_field("space_jid", "ghost@spaces.localhost"),
        text_field("name", "Ghost")
    );
    let resp = send_command(
        &mut admin,
        NODE_SPACES_UPDATE,
        "spaces-upd-ghost",
        &submit_form(NODE_SPACES_UPDATE, &extra),
    )
    .await;
    assert!(is_error(&resp), "expected error for unknown space, got: {resp}");
    assert!(
        resp.contains("item-not-found") || resp.contains("not-found"),
        "expected item-not-found, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_update_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-spaces-update-1").await;
    let extra = format!(
        "{}{}",
        text_field("space_jid", "eng@spaces.localhost"),
        text_field("name", "Hijack")
    );
    let resp = send_command(
        &mut alice,
        NODE_SPACES_UPDATE,
        "spaces-update-forbidden",
        &submit_form(NODE_SPACES_UPDATE, &extra),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(resp.contains("forbidden"), "expected <forbidden/>, got: {resp}");
    let _ = alice.close().await;
}

// ---------------------------------------------------------------------------
// spaces:delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spaces_delete_requires_confirm_yes() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-del-1").await;

    // Seed a space.
    let create_resp = send_command(
        &mut admin,
        NODE_SPACES_CREATE,
        "spaces-del-create",
        &submit_form(NODE_SPACES_CREATE, &text_field("name", "Doomed")),
    )
    .await;
    let space_jid = extract_field(&create_resp, "space_jid").expect("space_jid");

    // Delete without confirm.
    let resp = send_command(
        &mut admin,
        NODE_SPACES_DELETE,
        "spaces-del-noconfirm",
        &submit_form(
            NODE_SPACES_DELETE,
            &format!(
                "{}{}",
                text_field("space_jid", &space_jid),
                text_field("confirm", "no")
            ),
        ),
    )
    .await;
    assert!(is_error(&resp), "expected error without confirm=yes, got: {resp}");
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_delete_cascades_to_metadata() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-del-2").await;

    let create_resp = send_command(
        &mut admin,
        NODE_SPACES_CREATE,
        "spaces-del2-create",
        &submit_form(NODE_SPACES_CREATE, &text_field("name", "Disposable")),
    )
    .await;
    let space_jid = extract_field(&create_resp, "space_jid").expect("space_jid");

    let resp = send_command(
        &mut admin,
        NODE_SPACES_DELETE,
        "spaces-del2-go",
        &submit_form(
            NODE_SPACES_DELETE,
            &format!(
                "{}{}",
                text_field("space_jid", &space_jid),
                text_field("confirm", "yes")
            ),
        ),
    )
    .await;
    assert!(is_result(&resp), "expected delete success, got: {resp}");

    // Listing after delete should not include the JID.
    let list_resp = send_command(
        &mut admin,
        NODE_SPACES_LIST,
        "spaces-del2-list",
        &submit_form(NODE_SPACES_LIST, ""),
    )
    .await;
    assert!(
        !list_resp.contains(&space_jid),
        "deleted space should be gone from list, got: {list_resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_delete_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-spaces-del-1").await;
    let resp = send_command(
        &mut alice,
        NODE_SPACES_DELETE,
        "spaces-delete-forbidden",
        &submit_form(
            NODE_SPACES_DELETE,
            &format!(
                "{}{}",
                text_field("space_jid", "eng@spaces.localhost"),
                text_field("confirm", "yes")
            ),
        ),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(resp.contains("forbidden"), "expected <forbidden/>, got: {resp}");
    let _ = alice.close().await;
}

// ---------------------------------------------------------------------------
// spaces:members + set-role
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spaces_members_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-spaces-members-1").await;
    let resp = send_command(
        &mut alice,
        NODE_SPACES_MEMBERS,
        "spaces-members-forbidden",
        &submit_form(
            NODE_SPACES_MEMBERS,
            &text_field("space_jid", "eng@spaces.localhost"),
        ),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(resp.contains("forbidden"), "expected <forbidden/>, got: {resp}");
    let _ = alice.close().await;
}

#[tokio::test]
async fn spaces_set_role_round_trips_through_members() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-role-1").await;

    let create_resp = send_command(
        &mut admin,
        NODE_SPACES_CREATE,
        "spaces-role-create",
        &submit_form(NODE_SPACES_CREATE, &text_field("name", "RoleSpace")),
    )
    .await;
    let space_jid = extract_field(&create_resp, "space_jid").expect("space_jid");

    // Promote a bystander to admin.
    let set_extra = format!(
        "{}{}{}",
        text_field("space_jid", &space_jid),
        text_field("member_jid", "bystander@localhost"),
        text_field("role", "admin"),
    );
    let resp = send_command(
        &mut admin,
        NODE_SPACES_SET_ROLE,
        "spaces-role-set",
        &submit_form(NODE_SPACES_SET_ROLE, &set_extra),
    )
    .await;
    assert!(is_result(&resp), "expected set-role success, got: {resp}");

    // Listing members should include the promoted JID with role admin.
    let list_resp = send_command(
        &mut admin,
        NODE_SPACES_MEMBERS,
        "spaces-role-list",
        &submit_form(NODE_SPACES_MEMBERS, &text_field("space_jid", &space_jid)),
    )
    .await;
    assert!(is_result(&list_resp), "expected members list, got: {list_resp}");
    assert!(
        list_resp.contains("bystander@localhost"),
        "members list missing promoted user, got: {list_resp}"
    );
    assert!(
        list_resp.contains(">admin<"),
        "members list missing 'admin' role marker, got: {list_resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_set_role_removes_with_none() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-role-2").await;
    let create_resp = send_command(
        &mut admin,
        NODE_SPACES_CREATE,
        "spaces-role2-create",
        &submit_form(NODE_SPACES_CREATE, &text_field("name", "RemoveMe")),
    )
    .await;
    let space_jid = extract_field(&create_resp, "space_jid").expect("space_jid");

    // Add then remove.
    for role in ["member", "none"] {
        let extra = format!(
            "{}{}{}",
            text_field("space_jid", &space_jid),
            text_field("member_jid", "ghost@localhost"),
            text_field("role", role),
        );
        let resp = send_command(
            &mut admin,
            NODE_SPACES_SET_ROLE,
            &format!("spaces-role2-{role}"),
            &submit_form(NODE_SPACES_SET_ROLE, &extra),
        )
        .await;
        assert!(is_result(&resp), "expected set-role {role} success, got: {resp}");
    }

    let list_resp = send_command(
        &mut admin,
        NODE_SPACES_MEMBERS,
        "spaces-role2-list",
        &submit_form(NODE_SPACES_MEMBERS, &text_field("space_jid", &space_jid)),
    )
    .await;
    assert!(
        !list_resp.contains("ghost@localhost"),
        "removed member should not appear in list, got: {list_resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_set_role_forbidden_for_non_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-spaces-role-1").await;
    let extra = format!(
        "{}{}{}",
        text_field("space_jid", "eng@spaces.localhost"),
        text_field("member_jid", "alice@localhost"),
        text_field("role", "owner"),
    );
    let resp = send_command(
        &mut alice,
        NODE_SPACES_SET_ROLE,
        "spaces-role-forbidden",
        &submit_form(NODE_SPACES_SET_ROLE, &extra),
    )
    .await;
    assert!(is_error(&resp), "expected error, got: {resp}");
    assert!(resp.contains("forbidden"), "expected <forbidden/>, got: {resp}");
    let _ = alice.close().await;
}
