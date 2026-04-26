//! XEP-0163 PEP wire-conformance integration tests over WebSocket.
//!
//! Tests cover what is distinctive about PEP (XEP-0163) vs general PubSub
//! (XEP-0060): auto-create on first publish, access control (owner-only
//! publish), the default max_items=1 policy, and owner purge.

mod ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";

// ---------------------------------------------------------------------------
// Helpers (mirror xep0060_pubsub_ws.rs; duplicated to keep suites independent)
// ---------------------------------------------------------------------------

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ADMIN, &password, resource)
        .await
        .expect("admin connect")
}

/// Send `<iq type="set" to="{to}">` and wait for the matching response frame.
async fn iq_set_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq set");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id="{id}""#)) && frame.contains("<iq"))
        .await
        .expect("iq set response")
}

/// Send `<iq type="get" to="{to}">` and wait for the matching response frame.
async fn iq_get_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="get" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq get");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id="{id}""#)) && frame.contains("<iq"))
        .await
        .expect("iq get response")
}

/// Count `<item ` and `<item>` tags — not `<items`.
fn count_item_elements(xml: &str) -> usize {
    count_occurrences(xml, "<item ") + count_occurrences(xml, "<item>")
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
}

// ============================================================================
// Test 1 — pep_owner_can_publish_to_self_node_without_explicit_create
// ============================================================================
//
// XEP-0163 §3: a PEP service MUST auto-create the node on first publish when
// no <create/> precedes the <publish/>.  The caller must NOT pre-create the
// node; the server auto-provisions it with PEP defaults.

#[tokio::test]
async fn pep_owner_can_publish_to_self_node_without_explicit_create() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "pep163-auto-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    // Publish directly — no preceding <create/> — to a XEP-0402 bookmark URI.
    // The server must auto-create and return type="result".
    let resp = iq_set_to(
        &mut admin,
        "pep-pub-auto-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="home"><conference xmlns="urn:xmpp:bookmarks:1" name="Home"/></item></publish></pubsub>"#
        ),
    )
    .await;

    assert!(
        resp.contains(r#"type="result""#),
        "auto-create+publish to own PEP node must succeed (XEP-0163 §3): {resp}"
    );
    assert!(
        !resp.contains(r#"type="error""#),
        "must not return an error: {resp}"
    );
}

// ============================================================================
// Test 2 — pep_other_user_cannot_publish_to_alice_pep_node
// ============================================================================
//
// XEP-0163 §4: only the account owner may publish to their own PEP node.
// Any other user must receive <forbidden/>.

#[tokio::test]
async fn pep_other_user_cannot_publish_to_alice_pep_node() {
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let mut admin = admin_client(&server, "pep163-authz-admin").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    // Admin creates a node by publishing to their own PEP service.
    let r1 = iq_set_to(
        &mut admin,
        "pep-authz-pub-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="home"><conference xmlns="urn:xmpp:bookmarks:1" name="Home"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        r1.contains(r#"type="result""#),
        "admin must be able to publish to own node: {r1}"
    );

    // Bob connects and attempts to publish to ADMIN's PEP node.
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "pep163-authz-bob",
    )
    .await
    .expect("bob connect");

    bob.send(&format!(
        r#"<iq type="set" id="bob-pub-1" to="{admin_bare}"><pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="evil"><conference xmlns="urn:xmpp:bookmarks:1" name="Evil"/></item></publish></pubsub></iq>"#
    ))
    .await
    .expect("send bob publish");

    let bob_resp = bob
        .recv_matching(|frame| frame.contains(r#"id="bob-pub-1""#) && frame.contains("<iq"))
        .await
        .expect("bob publish response");

    assert!(
        bob_resp.contains(r#"type="error""#),
        "non-owner publish to PEP node must be forbidden (XEP-0163 §4): {bob_resp}"
    );
    assert!(
        bob_resp.contains("<error"),
        "expected <error> element in response: {bob_resp}"
    );
    assert!(
        !bob_resp.contains(r#"type="result""#),
        "must not return success: {bob_resp}"
    );

    // Drain any remaining frames before dropping.
    let _ = bob.recv_timeout(Duration::from_millis(200)).await;
    bob.close().await;
    admin.close().await;
}

// ============================================================================
// Test 3 — pep_max_items_is_one_by_default
// ============================================================================
//
// XEP-0163 §4: the default PEP node configuration has max_items=1.
// Publishing a second item with a different id MUST evict the first.

#[tokio::test]
async fn pep_max_items_is_one_by_default() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "pep163-maxitems-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    // Publish first item (auto-creates the PEP node with max_items=1).
    let r1 = iq_set_to(
        &mut admin,
        "pep-mi-pub-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="i1"><conference xmlns="urn:xmpp:bookmarks:1" name="First"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r1.contains(r#"type="result""#), "publish i1: {r1}");

    // Publish second item — i1 must be evicted (max_items=1).
    let r2 = iq_set_to(
        &mut admin,
        "pep-mi-pub-2",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="i2"><conference xmlns="urn:xmpp:bookmarks:1" name="Second"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r2.contains(r#"type="result""#), "publish i2: {r2}");

    // Retrieve items — only i2 should remain.
    let items_resp = iq_get_to(
        &mut admin,
        "pep-mi-items-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="urn:xmpp:bookmarks:1"/></pubsub>"#),
    )
    .await;
    assert!(
        items_resp.contains(r#"type="result""#),
        "items get: {items_resp}"
    );
    assert!(
        !items_resp.contains(r#"id="i1""#),
        "i1 must have been evicted by max_items=1 PEP default: {items_resp}"
    );
    assert!(
        items_resp.contains(r#"id="i2""#),
        "i2 must be present: {items_resp}"
    );
    assert_eq!(
        count_item_elements(&items_resp),
        1,
        "exactly one <item> expected with PEP max_items=1: {items_resp}"
    );
}

// ============================================================================
// Test 4 — pep_owner_can_purge_self_node
// ============================================================================
//
// XEP-0060 §8.5 (owner namespace) is used to purge a PEP node.  Because PEP
// defaults to max_items=1 we first reconfigure max_items=10 so we can stage
// multiple items, then purge, then verify the node is empty but still exists.

#[tokio::test]
async fn pep_owner_can_purge_self_node() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "pep163-purge-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    // Auto-create the PEP node by publishing item p1.
    let r1 = iq_set_to(
        &mut admin,
        "pep-purge-pub-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="p1"><conference xmlns="urn:xmpp:bookmarks:1" name="One"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        r1.contains(r#"type="result""#),
        "create node via publish: {r1}"
    );

    // Reconfigure max_items=10 so subsequent items are not auto-evicted.
    let cfg = iq_set_to(
        &mut admin,
        "pep-purge-cfg-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="urn:xmpp:bookmarks:1"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#max_items"><value>10</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(
        cfg.contains(r#"type="result""#),
        "configure max_items=10: {cfg}"
    );

    // Publish two more items so the node holds 3 items total.
    let r2 = iq_set_to(
        &mut admin,
        "pep-purge-pub-2",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="p2"><conference xmlns="urn:xmpp:bookmarks:1" name="Two"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r2.contains(r#"type="result""#), "publish p2: {r2}");

    let r3 = iq_set_to(
        &mut admin,
        "pep-purge-pub-3",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="p3"><conference xmlns="urn:xmpp:bookmarks:1" name="Three"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r3.contains(r#"type="result""#), "publish p3: {r3}");

    // Confirm 3 items are present before purge.
    let before = iq_get_to(
        &mut admin,
        "pep-purge-items-before",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="urn:xmpp:bookmarks:1"/></pubsub>"#),
    )
    .await;
    assert_eq!(
        count_item_elements(&before),
        3,
        "expected 3 items before purge: {before}"
    );

    // Purge the node via the owner namespace (XEP-0060 §8.5).
    let purge_resp = iq_set_to(
        &mut admin,
        "pep-purge-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><purge node="urn:xmpp:bookmarks:1"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        purge_resp.contains(r#"type="result""#),
        "PEP owner purge must succeed (XEP-0060 §8.5): {purge_resp}"
    );

    // Node must be empty after purge.
    let after = iq_get_to(
        &mut admin,
        "pep-purge-items-after",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="urn:xmpp:bookmarks:1"/></pubsub>"#),
    )
    .await;
    assert!(
        after.contains(r#"type="result""#),
        "items get after purge: {after}"
    );
    assert_eq!(
        count_item_elements(&after),
        0,
        "node must be empty after purge: {after}"
    );

    // Node still exists — publish should succeed without item-not-found.
    let post_purge = iq_set_to(
        &mut admin,
        "pep-purge-pub-4",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="p4"><conference xmlns="urn:xmpp:bookmarks:1" name="Four"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        post_purge.contains(r#"type="result""#),
        "publish after purge must succeed (node still exists): {post_purge}"
    );
}

// ============================================================================
// XEP-0163 §4.3 PEP fan-out (#238)
// ============================================================================

async fn wait_for_event_message(
    client: &mut WsXmppClient,
    node: &str,
    dur: Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match client.recv_timeout(remaining).await {
            Ok(frame) => {
                if frame.contains("<message")
                    && frame.contains(NS_PUBSUB_EVENT)
                    && (frame.contains(&format!(r#"node="{node}""#))
                        || frame.contains(&format!(r#"node='{node}'"#)))
                {
                    return Some(frame);
                }
            }
            Err(_) => return None,
        }
    }
}

#[tokio::test]
async fn pep_publish_fans_event_with_owner_jid_as_from() {
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let mut admin = admin_client(&server, "pep-fanout-admin").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");

    // Auto-create the PEP node and configure access_model=open so bob can
    // subscribe without presence subscription (presence-driven filtering
    // is out of scope per #238).
    let r1 = iq_set_to(
        &mut admin,
        "pep-fanout-create",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="seed"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r1.contains(r#"type="result""#), "create: {r1}");

    let cfg = iq_set_to(
        &mut admin,
        "pep-fanout-cfg",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="urn:xmpp:bookmarks:1"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#access_model"><value>open</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(cfg.contains(r#"type="result""#), "configure open: {cfg}");

    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "pep-fanout-bob",
    )
    .await
    .expect("bob connect");

    let sub = iq_set_to(
        &mut bob,
        "pep-fanout-sub",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="urn:xmpp:bookmarks:1" jid="{bob_bare}"/></pubsub>"#
        ),
    )
    .await;
    assert!(sub.contains(r#"type="result""#), "subscribe: {sub}");

    let pub_resp = iq_set_to(
        &mut admin,
        "pep-fanout-pub",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="bm-1"><conference xmlns="urn:xmpp:bookmarks:1" name="One"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type="result""#), "publish: {pub_resp}");

    let event = wait_for_event_message(&mut bob, "urn:xmpp:bookmarks:1", Duration::from_secs(2))
        .await
        .expect("bob must receive PEP event");

    // XEP-0163 §4.3: PEP `from` is the bare account JID.
    assert!(
        event.contains(&format!(r#"from="{admin_bare}""#))
            || event.contains(&format!(r#"from='{admin_bare}'"#)),
        "from must be the PEP account bare JID: {event}"
    );
    // XEP-0163 §4.3 + XEP-0060 §12.18: PEP MUST be headline.
    assert!(
        event.contains(r#"type="headline""#) || event.contains(r#"type='headline'"#),
        "PEP event must be type=headline: {event}"
    );
    assert!(
        event.contains(r#"id="bm-1""#),
        "item id must round-trip: {event}"
    );
    // §7.1.5: publisher == owner here (admin published to own PEP), so
    // no publisher attribute should be emitted.
    assert!(
        !event.contains("publisher="),
        "publisher attr must be omitted on PEP self-publish: {event}"
    );

    bob.close().await;
    admin.close().await;
}
