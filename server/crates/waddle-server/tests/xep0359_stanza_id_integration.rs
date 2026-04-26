//! XEP-0359 (Unique and Stable Stanza IDs) integration tests over WebSocket.
//!
//! These exercise the wire-stamping behaviour of the message canonicalizer:
//! 1:1 messages get per-archive `<stanza-id>` elements with `by=$archive_owner`,
//! the strip rule removes any inbound `<stanza-id by=$us/>` forged by the
//! sender, and `<origin-id/>` from the client is preserved.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{disco_info_query, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn connect_alice_bob() -> (TestServer, WsXmppClient, WsXmppClient) {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

    let alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice connection");
    let bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");

    (server, alice, bob)
}

/// Pull the value of a `<stanza-id>` element whose `by` attribute equals
/// `expected_by`. Returns `None` when no matching element is present.
fn extract_stanza_id_by(frame: &str, expected_by: &str) -> Option<String> {
    // The XML attribute order we generate is `xmlns="..."`, then `id`, then
    // `by`. Be tolerant of single vs double quotes and arbitrary attribute
    // order — we look for "stanza-id" with both id and by attributes.
    for tag_start in frame
        .match_indices("<stanza-id")
        .map(|(idx, _)| idx)
        .chain(frame.match_indices("<stanza-id ").map(|(idx, _)| idx))
    {
        let after_open = &frame[tag_start..];
        let Some(tag_end) = after_open.find("/>").or_else(|| after_open.find('>')) else {
            continue;
        };
        let tag_text = &after_open[..tag_end];
        let by = attr_value(tag_text, "by")?;
        if by == expected_by {
            return attr_value(tag_text, "id");
        }
    }
    None
}

fn attr_value(text: &str, name: &str) -> Option<String> {
    for delim in ['"', '\''] {
        let needle = format!(r#"{name}={delim}"#);
        if let Some(start) = text.find(&needle) {
            let value_start = start + needle.len();
            if let Some(rel_end) = text[value_start..].find(delim) {
                return Some(text[value_start..value_start + rel_end].to_string());
            }
        }
    }
    None
}

#[tokio::test]
async fn dm_carries_recipient_archive_stanza_id() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    let body_marker = format!("dm-{}", uuid::Uuid::new_v4());
    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@{DOMAIN}" id="msg-1"><body>{body_marker}</body></message>"#
        ))
        .await
        .expect("send dm");

    let delivered = bob
        .recv_matching(|frame| {
            frame.contains(&body_marker)
                && frame.contains("type=\"chat\"")
                && frame.contains("<body>")
        })
        .await
        .expect("dm delivery");

    let stanza_id = extract_stanza_id_by(&delivered, &format!("bob@{DOMAIN}"))
        .unwrap_or_else(|| {
            panic!("recipient-archive <stanza-id by='bob@localhost'/> on delivery; delivered frame was: {delivered}")
        });
    assert!(
        !stanza_id.is_empty(),
        "stanza-id value must be non-empty: {delivered}"
    );

    bob.close().await;
    alice.close().await;
}

#[tokio::test]
async fn dm_strip_rule_removes_forged_recipient_stamp() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    // Alice forges a `<stanza-id>` with `by=bob@localhost` — the spec MUST
    // says we strip this before re-stamping.
    let body_marker = format!("forge-{}", uuid::Uuid::new_v4());
    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@{DOMAIN}" id="msg-2"><body>{body_marker}</body><stanza-id xmlns="urn:xmpp:sid:0" id="forged-by-alice" by="bob@{DOMAIN}"/></message>"#
        ))
        .await
        .expect("send forged");

    let delivered = bob
        .recv_matching(|frame| {
            frame.contains(&body_marker)
                && frame.contains("type=\"chat\"")
                && frame.contains("<body>")
        })
        .await
        .expect("forged dm delivery");

    let stamped =
        extract_stanza_id_by(&delivered, &format!("bob@{DOMAIN}")).expect("our recipient stamp");
    assert_ne!(
        stamped, "forged-by-alice",
        "forged stanza-id must be replaced; saw {delivered}"
    );

    bob.close().await;
    alice.close().await;
}

#[tokio::test]
async fn dm_origin_id_preserved() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    let origin = format!("origin-{}", uuid::Uuid::new_v4());
    let body_marker = format!("origin-test-{}", uuid::Uuid::new_v4());
    alice
        .send(&format!(
            r#"<message xmlns="jabber:client" type="chat" to="bob@{DOMAIN}" id="msg-3"><body>{body_marker}</body><origin-id xmlns="urn:xmpp:sid:0" id="{origin}"/></message>"#
        ))
        .await
        .expect("send with origin-id");

    let delivered = bob
        .recv_matching(|frame| {
            frame.contains(&body_marker)
                && frame.contains("type=\"chat\"")
                && frame.contains("<body>")
        })
        .await
        .expect("dm delivery");

    assert!(
        delivered.contains(&format!(r#"id="{origin}""#))
            || delivered.contains(&format!("id='{origin}'")),
        "origin-id must be preserved: {delivered}"
    );

    bob.close().await;
    alice.close().await;
}

#[tokio::test]
async fn disco_advertises_stanza_id_feature() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, _bob) = connect_alice_bob().await;

    let response = disco_info_query(&mut alice, DOMAIN, "disco-sid-1")
        .await
        .expect("disco#info response");

    assert!(
        response.contains("urn:xmpp:sid:0"),
        "server disco#info must list urn:xmpp:sid:0: {response}"
    );

    alice.close().await;
}
