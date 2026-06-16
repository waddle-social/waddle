//! Integration suite for admin V2 `urn:waddle:admin:spaces:*` commands.
//!
//! Covers the wire surface end-to-end over the production WebSocket
//! transport:
//!
//! - owner-only ACL on every command,
//! - `spaces:create` mints a space + backing pubsub node,
//! - `spaces:list` enumerates the Spaces PubSub nodes with metadata overlay,
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
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA: &str = "jabber:x:data";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const SPACES_JID: &str = "spaces.localhost";

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
        .recv_matching(|frame| iq_attr_equals(frame, "id", id))
        .await
        .expect("iq response")
}

async fn send_iq_set_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq set");
    client
        .recv_matching(|frame| iq_attr_equals(frame, "id", id))
        .await
        .expect("iq set response")
}

fn submit_form(node: &str, extra: &str) -> String {
    format!(
        r#"<x xmlns="{NS_DATA}" type="submit"><field var="FORM_TYPE" type="hidden"><value>{node}</value></field>{extra}</x>"#
    )
}

fn text_field(var: &str, value: &str) -> String {
    format!(r#"<field var="{var}" type="text-single"><value>{value}</value></field>"#)
}

fn parse_iq(frame: &str) -> Option<Element> {
    frame
        .parse::<Element>()
        .ok()
        .filter(|element| element.name() == "iq")
}

fn iq_attr_equals(frame: &str, attr: &str, value: &str) -> bool {
    parse_iq(frame)
        .as_ref()
        .and_then(|iq| iq.attr(attr))
        .is_some_and(|actual| actual == value)
}

fn is_result(frame: &str) -> bool {
    iq_attr_equals(frame, "type", "result")
}

fn is_error(frame: &str) -> bool {
    iq_attr_equals(frame, "type", "error")
}

/// Pull the value of `var=...` text-single field from the result form
/// — used to grab the space JID minted by `spaces:create`.
fn extract_field(frame: &str, var: &str) -> Option<String> {
    let iq = parse_iq(frame)?;
    find_field_value(&iq, var)
}

fn find_field_value(element: &Element, var: &str) -> Option<String> {
    if element.name() == "field" && element.attr("var") == Some(var) {
        return element
            .children()
            .find(|child| child.name() == "value")
            .map(|value| value.text());
    }
    element
        .children()
        .find_map(|child| find_field_value(child, var))
}

// ---------------------------------------------------------------------------
// spaces:list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spaces_list_includes_bootstrapped_general_space() {
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
        resp.contains("general@spaces.localhost"),
        "fresh server should list the bootstrapped General space JID, got: {resp}"
    );
    assert!(
        resp.contains("General"),
        "fresh server should list the bootstrapped General space name, got: {resp}"
    );
    assert!(
        resp.contains("<item>"),
        "fresh server should return at least one space item, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_list_projects_non_jid_pubsub_node_ids_as_escaped_space_jids() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-list-escaped").await;

    let create = send_iq_set_to(
        &mut admin,
        "spaces-escaped-create",
        SPACES_JID,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><create node="music/A"/></pubsub>"#),
    )
    .await;
    assert!(
        is_result(&create),
        "expected native PubSub create result, got: {create}"
    );

    let list_resp = send_command(
        &mut admin,
        NODE_SPACES_LIST,
        "spaces-escaped-list",
        &submit_form(NODE_SPACES_LIST, ""),
    )
    .await;
    assert!(
        list_resp.contains("music\\2fa@spaces.localhost") && list_resp.contains("music/A"),
        "spaces:list should include escaped JID projection for non-JID node ids, got: {list_resp}"
    );

    let update_extra = format!(
        "{}{}{}",
        text_field("space_jid", "music\\2fa@spaces.localhost"),
        text_field("space_node", "music/A"),
        text_field("name", "Music")
    );
    let update_resp = send_command(
        &mut admin,
        NODE_SPACES_UPDATE,
        "spaces-escaped-update",
        &submit_form(NODE_SPACES_UPDATE, &update_extra),
    )
    .await;
    assert!(
        is_result(&update_resp) && update_resp.contains(NODE_SPACES_UPDATE),
        "escaped space_jid update should succeed with update FORM_TYPE, got: {update_resp}"
    );

    let readback = send_command(
        &mut admin,
        NODE_SPACES_LIST,
        "spaces-escaped-readback",
        &submit_form(NODE_SPACES_LIST, ""),
    )
    .await;
    assert!(
        readback.contains("music\\2fa@spaces.localhost")
            && readback.contains("music/A")
            && readback.contains("Music"),
        "metadata overlay should use escaped JID projection, got: {readback}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_list_distinguishes_literal_escape_sequences_from_escaped_slashes() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-list-escape-collision").await;

    for (id, node) in [
        ("spaces-collision-slash-create", "music/a"),
        ("spaces-collision-literal-create", "music\\2fa"),
    ] {
        let create = send_iq_set_to(
            &mut admin,
            id,
            SPACES_JID,
            &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><create node="{node}"/></pubsub>"#),
        )
        .await;
        assert!(
            is_result(&create),
            "expected native PubSub create result for {node}, got: {create}"
        );
    }

    let list_resp = send_command(
        &mut admin,
        NODE_SPACES_LIST,
        "spaces-collision-list",
        &submit_form(NODE_SPACES_LIST, ""),
    )
    .await;
    assert!(
        list_resp.contains("music\\2fa@spaces.localhost")
            && list_resp.contains("music\\5c2fa@spaces.localhost")
            && list_resp.contains("music/a")
            && list_resp.contains("music\\2fa"),
        "spaces:list should keep escape-looking node ids distinct, got: {list_resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_native_create_rejects_nodes_with_boundary_spaces() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-create-boundary-spaces").await;

    for (id, node) in [
        ("spaces-boundary-leading-space", " ops"),
        ("spaces-boundary-trailing-space", "ops "),
    ] {
        let create = send_iq_set_to(
            &mut admin,
            id,
            SPACES_JID,
            &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><create node="{node}"/></pubsub>"#),
        )
        .await;
        assert!(
            is_error(&create) && create.contains("bad-request"),
            "expected bad-request for unprojectable Spaces node {node:?}, got: {create}"
        );
    }
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
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
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
    assert!(
        space_jid.contains("@"),
        "expected bare JID, got: {space_jid}"
    );

    // List should now include the new space.
    let list_resp = send_command(
        &mut admin,
        NODE_SPACES_LIST,
        "spaces-create-list",
        &submit_form(NODE_SPACES_LIST, ""),
    )
    .await;
    assert!(
        is_result(&list_resp),
        "expected list result, got: {list_resp}"
    );
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
    assert!(
        is_error(&resp),
        "expected error for empty name, got: {resp}"
    );

    // 81-char name violates the 80-char ceiling.
    let long = "a".repeat(81);
    let resp = send_command(
        &mut admin,
        NODE_SPACES_CREATE,
        "spaces-create-long",
        &submit_form(NODE_SPACES_CREATE, &text_field("name", &long)),
    )
    .await;
    assert!(
        is_error(&resp),
        "expected error for overlong name, got: {resp}"
    );
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
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
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
async fn spaces_update_bootstrapped_general_space_creates_metadata_overlay() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-update-general").await;

    let update_extra = format!(
        "{}{}{}",
        text_field("space_jid", "general@spaces.localhost"),
        text_field("name", "Community"),
        text_field("description", "Default community space"),
    );
    let resp = send_command(
        &mut admin,
        NODE_SPACES_UPDATE,
        "spaces-upd-general",
        &submit_form(NODE_SPACES_UPDATE, &update_extra),
    )
    .await;
    assert!(is_result(&resp), "expected update result, got: {resp}");
    assert!(
        resp.contains("Community"),
        "updated General space should reflect metadata overlay, got: {resp}"
    );

    let list_resp = send_command(
        &mut admin,
        NODE_SPACES_LIST,
        "spaces-upd-general-list",
        &submit_form(NODE_SPACES_LIST, ""),
    )
    .await;
    assert!(
        list_resp.contains("general@spaces.localhost") && list_resp.contains("Community"),
        "spaces:list should show updated bootstrapped General metadata, got: {list_resp}"
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
    assert!(
        is_error(&resp),
        "expected error for unknown space, got: {resp}"
    );
    assert!(
        resp.contains("item-not-found") || resp.contains("not-found"),
        "expected item-not-found, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_commands_reject_space_jids_outside_spaces_service() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-wrong-domain").await;

    let update_extra = format!(
        "{}{}",
        text_field("space_jid", "general@wrong.localhost"),
        text_field("name", "Wrong Domain")
    );
    let update_resp = send_command(
        &mut admin,
        NODE_SPACES_UPDATE,
        "spaces-wrong-domain-update",
        &submit_form(NODE_SPACES_UPDATE, &update_extra),
    )
    .await;
    assert!(
        is_error(&update_resp) && update_resp.contains("bad-request"),
        "expected bad-request for wrong-domain update, got: {update_resp}"
    );

    let delete_extra = format!(
        "{}{}",
        text_field("space_jid", "general@wrong.localhost"),
        text_field("confirm", "yes"),
    );
    let delete_resp = send_command(
        &mut admin,
        NODE_SPACES_DELETE,
        "spaces-wrong-domain-delete",
        &submit_form(NODE_SPACES_DELETE, &delete_extra),
    )
    .await;
    assert!(
        is_error(&delete_resp) && delete_resp.contains("bad-request"),
        "expected bad-request for wrong-domain delete, got: {delete_resp}"
    );

    let members_resp = send_command(
        &mut admin,
        NODE_SPACES_MEMBERS,
        "spaces-wrong-domain-members",
        &submit_form(
            NODE_SPACES_MEMBERS,
            &text_field("space_jid", "general@wrong.localhost"),
        ),
    )
    .await;
    assert!(
        is_error(&members_resp) && members_resp.contains("bad-request"),
        "expected bad-request for wrong-domain members, got: {members_resp}"
    );

    let role_extra = format!(
        "{}{}{}",
        text_field("space_jid", "general@wrong.localhost"),
        text_field("member_jid", "wrong-domain@localhost"),
        text_field("role", "admin"),
    );
    let role_resp = send_command(
        &mut admin,
        NODE_SPACES_SET_ROLE,
        "spaces-wrong-domain-role",
        &submit_form(NODE_SPACES_SET_ROLE, &role_extra),
    )
    .await;
    assert!(
        is_error(&role_resp) && role_resp.contains("bad-request"),
        "expected bad-request for wrong-domain set-role, got: {role_resp}"
    );

    let list_resp = send_command(
        &mut admin,
        NODE_SPACES_LIST,
        "spaces-wrong-domain-list",
        &submit_form(NODE_SPACES_LIST, ""),
    )
    .await;
    assert!(
        list_resp.contains("general@spaces.localhost")
            && list_resp.contains("General")
            && !list_resp.contains("Wrong Domain"),
        "wrong-domain update/delete should not mutate General, got: {list_resp}"
    );

    let correct_members = send_command(
        &mut admin,
        NODE_SPACES_MEMBERS,
        "spaces-wrong-domain-correct-members",
        &submit_form(
            NODE_SPACES_MEMBERS,
            &text_field("space_jid", "general@spaces.localhost"),
        ),
    )
    .await;
    assert!(
        !correct_members.contains("wrong-domain@localhost"),
        "wrong-domain set-role should not mutate General members, got: {correct_members}"
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
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
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
    assert!(
        is_error(&resp),
        "expected error without confirm=yes, got: {resp}"
    );
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
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
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
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
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
    assert!(
        is_result(&list_resp),
        "expected members list, got: {list_resp}"
    );
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
async fn spaces_set_role_works_for_bootstrapped_general_space() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-spaces-role-general").await;

    let set_extra = format!(
        "{}{}{}",
        text_field("space_jid", "general@spaces.localhost"),
        text_field("member_jid", "bystander@localhost"),
        text_field("role", "admin"),
    );
    let resp = send_command(
        &mut admin,
        NODE_SPACES_SET_ROLE,
        "spaces-role-general-set",
        &submit_form(NODE_SPACES_SET_ROLE, &set_extra),
    )
    .await;
    assert!(
        is_result(&resp),
        "expected set-role on General to succeed, got: {resp}"
    );

    let list_resp = send_command(
        &mut admin,
        NODE_SPACES_MEMBERS,
        "spaces-role-general-list",
        &submit_form(
            NODE_SPACES_MEMBERS,
            &text_field("space_jid", "general@spaces.localhost"),
        ),
    )
    .await;
    assert!(
        list_resp.contains("bystander@localhost") && list_resp.contains(">admin<"),
        "General members list should include promoted user, got: {list_resp}"
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
        assert!(
            is_result(&resp),
            "expected set-role {role} success, got: {resp}"
        );
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
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/>, got: {resp}"
    );
    let _ = alice.close().await;
}
