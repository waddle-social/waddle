//! Server-targeted `disco#info` `waddle#server_affiliation` field must
//! agree with the admin ACL signal — see PR #697.
//!
//! After PR #696 the admin ACL is "is this JID a PubSub Owner on the
//! Spaces JID? (or in the bootstrap `WADDLE_SERVER_OWNER_LOCALPARTS`)".
//! The chat client gates its admin UI affordance
//! (`canManageCommunity`) off the `waddle#server_affiliation` field in
//! the server's `disco#info` response. Before #697 that field was
//! sourced from the Zanzibar permission graph — a different store than
//! the PubSub affiliation table — so a user promoted dynamically via
//! `spaces:set-role` would gain admin access on the server but the
//! chat would not show the admin UI until the matching Zanzibar tuple
//! was also written. #697 bridges them: the disco field now reads from
//! the same `is_community_owner` signal the ACL uses.
//!
//! Test matrix:
//!
//! 1. Bootstrap owner (env-list `admin`) sees
//!    `waddle#server_affiliation='owner'` in disco — preserves the
//!    pre-existing path.
//! 2. Non-owner (`alice`) without any Zanzibar tuple and without any
//!    PubSub Owner row sees no `waddle#server_affiliation` extension
//!    — there is no implicit tier.
//! 3. After the bootstrap owner promotes `alice` to PubSub Owner on a
//!    Spaces node via `spaces:set-role`, `alice`'s disco#info now
//!    surfaces `waddle#server_affiliation='owner'` — same edge as the
//!    admin ACL.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{disco_info_query, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA: &str = "jabber:x:data";
const NS_WADDLE_SERVER_INFO: &str = "urn:waddle:server-info:0";

const NODE_SPACES_CREATE: &str = "urn:waddle:admin:spaces:create:0";
const NODE_SPACES_SET_ROLE: &str = "urn:waddle:admin:spaces:set-role:0";

// Each TestServer spins up an ephemeral cert + listener; serialize the
// suite to avoid filesystem temp-port races (matches other admin
// integration tests in this directory).
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn connect(
    server: &TestServer,
    username: &str,
    password: &str,
    resource: &str,
) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, username, password, resource)
        .await
        .expect("websocket connect + auth")
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
        .expect("send command iq");
    client
        .recv_matching(|frame| {
            frame.contains("<iq")
                && (frame.contains(&format!(r#"id='{id}'"#))
                    || frame.contains(&format!(r#"id='{id}'"#)))
        })
        .await
        .expect("command iq response")
}

fn submit_form(node: &str, extra: &str) -> String {
    format!(
        r#"<x xmlns="{NS_DATA}" type="submit"><field var="FORM_TYPE" type="hidden"><value>{node}</value></field>{extra}</x>"#
    )
}

fn text_field(var: &str, value: &str) -> String {
    format!(r#"<field var="{var}" type="text-single"><value>{value}</value></field>"#)
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

/// Returns `true` iff the disco#info `<query/>` carries an extension
/// `<x type='result'/>` form whose `FORM_TYPE` equals the Waddle
/// server-info namespace AND whose `waddle#server_affiliation` field
/// holds `expected`. We deliberately key on the form-type so the
/// assertion can't be satisfied by a stray field on some other
/// extension form.
fn server_role_form_has_value(frame: &str, expected: &str) -> bool {
    let mut cursor = frame;
    while let Some(form_start) = cursor.find("<x ") {
        let after_open = &cursor[form_start..];
        let Some(form_end_rel) = after_open.find("</x>") else {
            return false;
        };
        let form = &after_open[..form_end_rel + "</x>".len()];
        if form.contains(NS_WADDLE_SERVER_INFO) {
            if let Some(value) = extract_field(form, "waddle#server_affiliation") {
                return value == expected;
            }
            return false;
        }
        cursor = &after_open[form_end_rel + "</x>".len()..];
    }
    false
}

fn server_role_form_present(frame: &str) -> bool {
    let mut cursor = frame;
    while let Some(form_start) = cursor.find("<x ") {
        let after_open = &cursor[form_start..];
        let Some(form_end_rel) = after_open.find("</x>") else {
            return false;
        };
        let form = &after_open[..form_end_rel + "</x>".len()];
        if form.contains(NS_WADDLE_SERVER_INFO) {
            return true;
        }
        cursor = &after_open[form_end_rel + "</x>".len()..];
    }
    false
}

#[tokio::test]
async fn bootstrap_owner_sees_server_affiliation_owner_in_disco() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &password, "admin-disco-bootstrap").await;

    let response = disco_info_query(&mut admin, DOMAIN, "disco-bootstrap-owner")
        .await
        .expect("disco#info response");

    assert!(
        server_role_form_has_value(&response, "owner"),
        "bootstrap owner should see waddle#server_affiliation='owner', got: {response}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn non_owner_omits_server_affiliation_extension() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = connect(&server, "alice", &alice_pass, "alice-disco-baseline").await;

    let response = disco_info_query(&mut alice, DOMAIN, "disco-non-owner")
        .await
        .expect("disco#info response");

    assert!(
        !server_role_form_present(&response),
        "non-owner should not see an `urn:waddle:server-info:0` extension form, got: {response}"
    );
    let _ = alice.close().await;
}

#[tokio::test]
async fn dynamic_pubsub_owner_promotes_server_affiliation_to_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);

    // Step 1 — alice without any promotion has no `owner` form (proven
    // separately above, but re-checked here so a regression that
    // surfaces a spurious 'owner' field in the promotion path is
    // not masked by the previous test running in isolation).
    let mut alice = connect(&server, "alice", &alice_pass, "alice-disco-promo-pre").await;
    let pre = disco_info_query(&mut alice, DOMAIN, "disco-alice-pre")
        .await
        .expect("disco#info response");
    assert!(
        !server_role_form_present(&pre),
        "alice (unpromoted) should not have `urn:waddle:server-info:0`, got: {pre}"
    );

    // Step 2 — bootstrap admin creates a space and promotes alice to
    // owner on that space's pubsub node. This writes an explicit
    // `Affiliation::Owner` row keyed on alice's bare JID on the
    // Spaces JID — the exact signal `is_community_owner` consults.
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "admin-disco-promo").await;
    let create_resp = send_command(
        &mut admin,
        NODE_SPACES_CREATE,
        "promo-create",
        &submit_form(NODE_SPACES_CREATE, &text_field("name", "PromoSpace")),
    )
    .await;
    let space_jid = extract_field(&create_resp, "space_jid").expect("space_jid in create response");
    let promote_extra = format!(
        "{}{}{}",
        text_field("space_jid", &space_jid),
        text_field("member_jid", "alice@localhost"),
        text_field("role", "owner"),
    );
    let set_resp = send_command(
        &mut admin,
        NODE_SPACES_SET_ROLE,
        "promo-set-owner",
        &submit_form(NODE_SPACES_SET_ROLE, &promote_extra),
    )
    .await;
    assert!(
        set_resp.contains(r#"type='result'"#) || set_resp.contains(r#"type='result'"#),
        "expected set-role result, got: {set_resp}"
    );
    let _ = admin.close().await;

    // Step 3 — alice's next disco#info MUST now surface
    // `waddle#server_affiliation='owner'`, even though no Zanzibar
    // tuple was written. This proves the bridge: ACL signal and
    // disco field are now driven by the same source.
    let post = disco_info_query(&mut alice, DOMAIN, "disco-alice-post")
        .await
        .expect("disco#info response after promotion");
    assert!(
        server_role_form_has_value(&post, "owner"),
        "after dynamic promotion alice should see waddle#server_affiliation='owner', got: {post}"
    );
    let _ = alice.close().await;
}
