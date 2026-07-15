//! Dedicated XEP-style suite for the V1 admin Users panel.
//!
//! Covers `urn:waddle:admin:users:list:0` end-to-end over the
//! production WebSocket transport:
//!
//! - owner can list users
//! - non-owner gets `<forbidden/>`
//! - `prefix` filter narrows results to a specific localpart
//! - seek pagination (`page_size` + `next_cursor`) round-trips and
//!   the final page omits `next_cursor`
//!
//! Constants are inlined rather than pulled from the server crate so
//! this test file documents the wire shape — a regression in the
//! handler that flipped a node identifier would have to flip it
//! here too, which is the point.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const ADMIN_COMMAND_NODE: &str = "urn:waddle:admin:users:list:0";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA: &str = "jabber:x:data";

// XEP-0050 + ws_common spawn one waddle-server per test (each starts
// a fresh ephemeral cert + listener). Serialize the suite so the
// shared filesystem temp-port slot doesn't race.
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

/// Send an `iq type='set'` command request to the server domain and
/// wait for the matching response. The form is interpolated literally
/// so each test can build a different argument set without depending
/// on a typed form builder.
async fn send_command(client: &mut WsXmppClient, id: &str, form_xml: &str) -> String {
    let body = format!(
        r#"<command xmlns="{NS_COMMANDS}" node="{ADMIN_COMMAND_NODE}" action="execute">{form_xml}</command>"#
    );
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{DOMAIN}">{body}</iq>"#
        ))
        .await
        .expect("send iq");
    // Both IQ results and IQ errors are now serialized via the typed
    // `xmpp_parsers`/`minidom` builder (single-quoted attributes). Match
    // the request id so we land on the response frame regardless of
    // result-vs-error.
    client
        .recv_matching(|frame| {
            frame.contains("<iq")
                && (frame.contains(&format!(r#"id='{id}'"#))
                    || frame.contains(&format!(r#"id='{id}'"#)))
        })
        .await
        .expect("iq response")
}

fn submit_form(extra: &str) -> String {
    format!(
        r#"<x xmlns="{NS_DATA}" type="submit"><field var="FORM_TYPE" type="hidden"><value>{ADMIN_COMMAND_NODE}</value></field>{extra}</x>"#
    )
}

fn is_result(frame: &str) -> bool {
    frame.contains(r#"type='result'"#) || frame.contains(r#"type='result'"#)
}

fn is_error(frame: &str) -> bool {
    frame.contains(r#"type='error'"#) || frame.contains(r#"type='error'"#)
}

#[tokio::test]
async fn owner_can_list_users() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server =
        TestServer::start_with_extra_accounts(&[("alice", &alice_pass), ("bob", &bob_pass)]);
    let mut admin = admin_client(&server, "admin-list-1").await;

    let resp = send_command(&mut admin, "list-owner", &submit_form("")).await;

    assert!(is_result(&resp), "expected result for owner, got: {resp}");
    // The result form must report the three standard columns. Cheap
    // substring matches are sufficient: a regression that drops a
    // reported column would also drop the string.
    assert!(
        resp.contains(r#"var='jid'"#) || resp.contains(r#"var='jid'"#),
        "result form must list jid column, got: {resp}"
    );
    assert!(
        resp.contains(r#"var='display_name'"#) || resp.contains(r#"var='display_name'"#),
        "result form must list display_name column, got: {resp}"
    );
    // The admin / alice / bob accounts must all appear as items in
    // the result. The default page size (50) easily fits the seeded
    // set.
    assert!(
        resp.contains("admin@localhost"),
        "expected admin user in list, got: {resp}"
    );
    assert!(
        resp.contains("alice@localhost"),
        "expected alice in list, got: {resp}"
    );
    assert!(
        resp.contains("bob@localhost"),
        "expected bob in list, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn non_owner_is_forbidden() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_pass)]);
    let mut alice = alice_client(&server, &alice_pass, "alice-list-1").await;

    let resp = send_command(&mut alice, "list-non-owner", &submit_form("")).await;

    assert!(is_error(&resp), "expected error for non-owner, got: {resp}");
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/> condition, got: {resp}"
    );
    let _ = alice.close().await;
}

#[tokio::test]
async fn prefix_narrows_results() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server =
        TestServer::start_with_extra_accounts(&[("alice", &alice_pass), ("bob", &bob_pass)]);
    let mut admin = admin_client(&server, "admin-prefix-1").await;

    let form = submit_form(r#"<field var="prefix" type="text-single"><value>ali</value></field>"#);
    let resp = send_command(&mut admin, "list-prefix", &form).await;

    assert!(
        is_result(&resp),
        "expected result for prefix search, got: {resp}"
    );
    assert!(
        resp.contains("alice@localhost"),
        "prefix 'ali' must match alice, got: {resp}"
    );
    assert!(
        !resp.contains("bob@localhost"),
        "prefix 'ali' must NOT match bob, got: {resp}"
    );
    // admin@localhost begins with 'a' but not with 'ali', so the
    // prefix filter must drop it too.
    assert!(
        !resp.contains("admin@localhost"),
        "prefix 'ali' must NOT match admin, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn pagination_cursor_round_trips() {
    let _serial = TEST_SERIAL.lock().await;
    // Seed enough users to span more than one page at page_size=2.
    // 5 total = admin + 4 extras, which gives 3 pages of size 2.
    let pass = format!("page-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("page1", &pass),
        ("page2", &pass),
        ("page3", &pass),
        ("page4", &pass),
    ]);
    let mut admin = admin_client(&server, "admin-page-1").await;

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for step in 0..10 {
        let extra = match cursor.as_deref() {
            Some(c) => format!(
                r#"<field var="page_size" type="text-single"><value>2</value></field><field var="after_cursor" type="text-single"><value>{c}</value></field>"#
            ),
            None => {
                r#"<field var="page_size" type="text-single"><value>2</value></field>"#.to_string()
            }
        };
        let resp = send_command(
            &mut admin,
            &format!("list-page-{step}"),
            &submit_form(&extra),
        )
        .await;
        assert!(
            is_result(&resp),
            "step {step}: expected result, got: {resp}"
        );

        for local in ["admin", "page1", "page2", "page3", "page4"] {
            let jid = format!("{local}@localhost");
            if resp.contains(&jid) && !seen.contains(&jid) {
                seen.push(jid);
            }
        }

        match extract_cursor(&resp) {
            Some(next) => {
                cursor = Some(next);
            }
            None => {
                // Final page reached.
                assert!(
                    step <= 3,
                    "expected at most 3 pages of size 2 (5 users), reached step {step}"
                );
                break;
            }
        }
    }

    // All 5 seeded users must have been observed exactly once across
    // the pagination walk.
    seen.sort();
    let expected = vec![
        "admin@localhost".to_string(),
        "page1@localhost".to_string(),
        "page2@localhost".to_string(),
        "page3@localhost".to_string(),
        "page4@localhost".to_string(),
    ];
    assert_eq!(
        seen, expected,
        "pagination must visit every user exactly once"
    );

    let _ = admin.close().await;
}

/// Cheap regex-free extractor: look for `<field var="next_cursor">`
/// or `var='next_cursor'`, then the next `<value>...</value>`. Good
/// enough for the test surface; if the response shape ever changes
/// this will break loudly.
fn extract_cursor(frame: &str) -> Option<String> {
    const DOUBLE_QUOTED: &str = r#"var="next_cursor""#;
    const SINGLE_QUOTED: &str = r#"var='next_cursor'"#;
    let needle = if let Some(idx) = frame.find(DOUBLE_QUOTED) {
        idx + DOUBLE_QUOTED.len()
    } else {
        let idx = frame.find(SINGLE_QUOTED)?;
        idx + SINGLE_QUOTED.len()
    };
    let rest = &frame[needle..];
    let open = rest.find("<value>")?;
    let after_open = &rest[open + "<value>".len()..];
    let close = after_open.find("</value>")?;
    Some(after_open[..close].to_string())
}
