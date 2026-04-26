//! XEP-0060 PubSub wire-conformance integration tests over WebSocket.
//!
//! Each test starts its own isolated waddle-server on dynamic ports.

mod ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, resource)
        .await
        .expect("admin connect")
}

/// Send a `<iq type="set" to=to_jid>` and wait for the matching response frame.
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

/// Send a `<iq type="get" to=to_jid>` and wait for the matching response frame.
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

/// Count occurrences of `needle` in `haystack`.
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
// Test 1 — subscribe_returns_subid
// ============================================================================

#[tokio::test]
async fn subscribe_returns_subid() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0060-sub-1").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");

    // Auto-create the node by publishing an item (PEP, to=admin bare JID).
    let resp = iq_set_to(
        &mut admin,
        "pub-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="bookmark-test"><item id="i1"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        resp.contains(r#"type="result""#),
        "auto-create+publish should succeed: {resp}"
    );

    // Subscribe to the newly created node (PEP subscribe, to=admin).
    let resp = iq_set_to(
        &mut admin,
        "sub-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="bookmark-test" jid="{admin_bare}"/></pubsub>"#
        ),
    )
    .await;

    assert!(
        resp.contains(r#"type="result""#),
        "subscribe should succeed: {resp}"
    );
    let subid = extract_attr_after(&resp, "subscription", "subid");
    assert!(
        subid.as_ref().is_some_and(|s| !s.is_empty()),
        "expected non-empty subid in subscribe result: {resp}"
    );
}

// ============================================================================
// Test 2 — unsubscribe_with_subid_succeeds
// ============================================================================

#[tokio::test]
async fn unsubscribe_with_subid_succeeds() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0060-unsub-1").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");

    // Create the node by publishing to admin's own PEP service.
    let resp = iq_set_to(
        &mut admin,
        "pub-2",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="unsub-test"><item id="i1"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        resp.contains(r#"type="result""#),
        "auto-create+publish: {resp}"
    );

    // Subscribe and capture subid.
    let sub_resp = iq_set_to(
        &mut admin,
        "sub-2",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="unsub-test" jid="{admin_bare}"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        sub_resp.contains(r#"type="result""#),
        "subscribe: {sub_resp}"
    );
    let subid = extract_attr_after(&sub_resp, "subscription", "subid")
        .expect("subid in subscribe response");

    // Unsubscribe with the subid — should succeed.
    let unsub_resp = iq_set_to(
        &mut admin,
        "unsub-2a",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><unsubscribe node="unsub-test" jid="{admin_bare}" subid="{subid}"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        unsub_resp.contains(r#"type="result""#),
        "first unsubscribe should succeed: {unsub_resp}"
    );

    // Unsubscribe again with the same (now stale) subid — expect an error.
    let unsub2_resp = iq_set_to(
        &mut admin,
        "unsub-2b",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><unsubscribe node="unsub-test" jid="{admin_bare}" subid="{subid}"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        unsub2_resp.contains(r#"type="error""#),
        "second unsubscribe with stale subid should error: {unsub2_resp}"
    );
    assert!(
        unsub2_resp.contains("<error"),
        "expected <error> element: {unsub2_resp}"
    );
}

// ============================================================================
// Test 3 — publish_then_get_returns_oldest_first
// ============================================================================

#[tokio::test]
async fn publish_then_get_returns_oldest_first() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0060-order-1").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");

    // Create node by publishing item 1 (PEP, to=admin).
    let r1 = iq_set_to(
        &mut admin,
        "pub-3a",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="order-test"><item id="first"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r1.contains(r#"type="result""#), "publish first: {r1}");

    // Because `order-test` is a PEP node with max_items=1, the first item
    // would be evicted when the second is published. Raise max_items before
    // publishing the second item so both survive and order can be verified.
    let cfg = iq_set_to(
        &mut admin,
        "cfg-3",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="order-test"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#max_items"><value>100</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(
        cfg.contains(r#"type="result""#),
        "configure max_items: {cfg}"
    );

    let r2 = iq_set_to(
        &mut admin,
        "pub-3b",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="order-test"><item id="second"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r2.contains(r#"type="result""#), "publish second: {r2}");

    // Retrieve items and verify "first" appears before "second".
    let items_resp = iq_get_to(
        &mut admin,
        "items-3",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="order-test"/></pubsub>"#),
    )
    .await;
    assert!(
        items_resp.contains(r#"type="result""#),
        "items get: {items_resp}"
    );

    let first_pos = items_resp.find(r#"id="first""#);
    let second_pos = items_resp.find(r#"id="second""#);
    assert!(
        first_pos.is_some() && second_pos.is_some(),
        "both items expected in response: {items_resp}"
    );
    assert!(
        first_pos.unwrap() < second_pos.unwrap(),
        "items should be returned oldest-first (XEP-0060 §6.5.7): {items_resp}"
    );
}

// ============================================================================
// Test 4 — pep_default_max_items_one
// ============================================================================

#[tokio::test]
async fn pep_default_max_items_one() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0060-pep-1").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");

    // Publish the first item to a generic PEP node (to=admin).
    // The auto-created node gets max_items=1 (PEP default).
    let r1 = iq_set_to(
        &mut admin,
        "pep-pub-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:waddle:test:single"><item id="item-one"><payload xmlns="urn:waddle:test"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r1.contains(r#"type="result""#), "publish item-one: {r1}");

    // Publish second item — should evict the first (max_items=1).
    let r2 = iq_set_to(
        &mut admin,
        "pep-pub-2",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:waddle:test:single"><item id="item-two"><payload xmlns="urn:waddle:test"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r2.contains(r#"type="result""#), "publish item-two: {r2}");

    // Retrieve items — only item-two should be present.
    let items_resp = iq_get_to(
        &mut admin,
        "pep-items-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="urn:waddle:test:single"/></pubsub>"#),
    )
    .await;
    assert!(
        items_resp.contains(r#"type="result""#),
        "items get: {items_resp}"
    );
    assert!(
        !items_resp.contains(r#"id="item-one""#),
        "item-one should have been evicted (max_items=1): {items_resp}"
    );
    assert!(
        items_resp.contains(r#"id="item-two""#),
        "item-two should be present: {items_resp}"
    );
    // Exactly one <item …> element in the response (not counting <items>).
    assert_eq!(
        count_item_elements(&items_resp),
        1,
        "expected exactly one <item> element: {items_resp}"
    );
}

// ============================================================================
// Test 5 — purge_clears_items_keeps_node
// ============================================================================

#[tokio::test]
async fn purge_clears_items_keeps_node() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0060-purge-1").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");

    // Create a PEP node (to=admin) then raise max_items so both items survive.
    let r1 = iq_set_to(
        &mut admin,
        "purge-pub-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="purge-test"><item id="pa"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r1.contains(r#"type="result""#), "create node: {r1}");

    let cfg = iq_set_to(
        &mut admin,
        "purge-cfg-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="purge-test"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#max_items"><value>100</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(cfg.contains(r#"type="result""#), "configure: {cfg}");

    let r2 = iq_set_to(
        &mut admin,
        "purge-pub-2",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="purge-test"><item id="pb"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r2.contains(r#"type="result""#), "publish pb: {r2}");

    // Confirm two items exist before purge.
    let before = iq_get_to(
        &mut admin,
        "purge-items-before",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="purge-test"/></pubsub>"#),
    )
    .await;
    assert_eq!(
        count_item_elements(&before),
        2,
        "expected 2 items before purge: {before}"
    );

    // Purge via the owner namespace (XEP-0060 §8.5), to=admin.
    let purge_resp = iq_set_to(
        &mut admin,
        "purge-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><purge node="purge-test"/></pubsub>"#),
    )
    .await;
    assert!(
        purge_resp.contains(r#"type="result""#),
        "purge should succeed: {purge_resp}"
    );

    // Node is empty after purge.
    let after = iq_get_to(
        &mut admin,
        "purge-items-after",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="purge-test"/></pubsub>"#),
    )
    .await;
    assert!(
        after.contains(r#"type="result""#),
        "items get after purge: {after}"
    );
    assert_eq!(
        count_item_elements(&after),
        0,
        "expected 0 items after purge: {after}"
    );

    // Node still exists: we can publish to it without getting item-not-found.
    let post_purge_pub = iq_set_to(
        &mut admin,
        "purge-pub-3",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="purge-test"><item id="pc"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        post_purge_pub.contains(r#"type="result""#),
        "publish after purge should succeed (node still exists): {post_purge_pub}"
    );
}

// ============================================================================
// Test 6 — outcast_subscriber_cannot_subscribe
// ============================================================================

#[tokio::test]
async fn outcast_subscriber_cannot_subscribe() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[("bob", "bobpass")]);
    let mut admin = admin_client(&server, "xep0060-outcast-admin").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");

    // Create a PEP node on admin's service.
    let r1 = iq_set_to(
        &mut admin,
        "outcast-pub-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="open-node"><item id="i1"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r1.contains(r#"type="result""#), "create node: {r1}");

    // Configure node as open so any subscriber would normally be allowed.
    let cfg = iq_set_to(
        &mut admin,
        "outcast-cfg-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="open-node"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#access_model"><value>open</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(cfg.contains(r#"type="result""#), "configure open: {cfg}");

    // Set bob as outcast on the node via the owner namespace (XEP-0060 §8.9).
    let aff_resp = iq_set_to(
        &mut admin,
        "outcast-aff-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><affiliations node="open-node"><affiliation xmlns="{NS_PUBSUB_OWNER}" jid="bob@{DOMAIN}" affiliation="outcast"/></affiliations></pubsub>"#
        ),
    )
    .await;
    assert!(
        aff_resp.contains(r#"type="result""#),
        "set outcast affiliation: {aff_resp}"
    );

    // Bob connects and attempts to subscribe to admin's node.
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        "bobpass",
        "xep0060-outcast-bob",
    )
    .await
    .expect("bob connect");

    // Bob's subscribe IQ targets admin's PEP service (to=admin@localhost).
    bob.send(&format!(
        r#"<iq type="set" id="bob-sub-1" to="{admin_bare}"><pubsub xmlns="{NS_PUBSUB}"><subscribe node="open-node" jid="bob@{DOMAIN}"/></pubsub></iq>"#
    ))
    .await
    .expect("send bob subscribe");

    let bob_resp = bob
        .recv_matching(|frame| frame.contains(r#"id="bob-sub-1""#) && frame.contains("<iq"))
        .await
        .expect("bob subscribe response");

    assert!(
        bob_resp.contains(r#"type="error""#),
        "outcast subscribe should be denied: {bob_resp}"
    );
    assert!(
        bob_resp.contains("<error"),
        "expected <error> element: {bob_resp}"
    );
    assert!(
        !bob_resp.contains(r#"type="result""#),
        "should not return result: {bob_resp}"
    );

    // Drain any remaining frames before dropping.
    let _ = bob.recv_timeout(Duration::from_millis(200)).await;
    bob.close().await;
    admin.close().await;
}
