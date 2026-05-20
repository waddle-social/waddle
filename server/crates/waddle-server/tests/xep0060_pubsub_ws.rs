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
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";

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
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
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
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
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

/// Drain frames until one is a `<message>` carrying an XEP-0060 §7.1 event
/// for `node`. Returns the matching frame or `None` on timeout.
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
                let is_event_msg = frame.contains("<message")
                    && frame.contains(NS_PUBSUB_EVENT)
                    && (frame.contains(&format!(r#"node='{node}'"#))
                        || frame.contains(&format!(r#"node='{node}'"#)));
                if is_event_msg {
                    return Some(frame);
                }
            }
            Err(_) => return None,
        }
    }
}

/// Assert no event-message frame arrives for `node` within `dur`.
async fn assert_no_event_message(client: &mut WsXmppClient, node: &str, dur: Duration) {
    if let Some(frame) = wait_for_event_message(client, node, dur).await {
        panic!("expected no event message for node {node}, got: {frame}");
    }
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
        resp.contains(r#"type='result'"#),
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
        resp.contains(r#"type='result'"#),
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
        resp.contains(r#"type='result'"#),
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
        sub_resp.contains(r#"type='result'"#),
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
        unsub_resp.contains(r#"type='result'"#),
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
        unsub2_resp.contains(r#"type='error'"#),
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
    assert!(r1.contains(r#"type='result'"#), "publish first: {r1}");

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
        cfg.contains(r#"type='result'"#),
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
    assert!(r2.contains(r#"type='result'"#), "publish second: {r2}");

    // Retrieve items and verify "first" appears before "second".
    let items_resp = iq_get_to(
        &mut admin,
        "items-3",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="order-test"/></pubsub>"#),
    )
    .await;
    assert!(
        items_resp.contains(r#"type='result'"#),
        "items get: {items_resp}"
    );

    let first_pos = items_resp.find(r#"id='first'"#);
    let second_pos = items_resp.find(r#"id='second'"#);
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
    assert!(r1.contains(r#"type='result'"#), "publish item-one: {r1}");

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
    assert!(r2.contains(r#"type='result'"#), "publish item-two: {r2}");

    // Retrieve items — only item-two should be present.
    let items_resp = iq_get_to(
        &mut admin,
        "pep-items-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="urn:waddle:test:single"/></pubsub>"#),
    )
    .await;
    assert!(
        items_resp.contains(r#"type='result'"#),
        "items get: {items_resp}"
    );
    assert!(
        !items_resp.contains(r#"id='item-one'"#),
        "item-one should have been evicted (max_items=1): {items_resp}"
    );
    assert!(
        items_resp.contains(r#"id='item-two'"#),
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
    assert!(r1.contains(r#"type='result'"#), "create node: {r1}");

    let cfg = iq_set_to(
        &mut admin,
        "purge-cfg-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="purge-test"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#max_items"><value>100</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(cfg.contains(r#"type='result'"#), "configure: {cfg}");

    let r2 = iq_set_to(
        &mut admin,
        "purge-pub-2",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="purge-test"><item id="pb"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r2.contains(r#"type='result'"#), "publish pb: {r2}");

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
        purge_resp.contains(r#"type='result'"#),
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
        after.contains(r#"type='result'"#),
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
        post_purge_pub.contains(r#"type='result'"#),
        "publish after purge should succeed (node still exists): {post_purge_pub}"
    );
}

// ============================================================================
// Test 6 — outcast_subscriber_cannot_subscribe
// ============================================================================

#[tokio::test]
async fn outcast_subscriber_cannot_subscribe() {
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
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
    assert!(r1.contains(r#"type='result'"#), "create node: {r1}");

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
    assert!(cfg.contains(r#"type='result'"#), "configure open: {cfg}");

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
        aff_resp.contains(r#"type='result'"#),
        "set outcast affiliation: {aff_resp}"
    );

    // Bob connects and attempts to subscribe to admin's node.
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
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
        .recv_matching(|frame| frame.contains(r#"id='bob-sub-1'"#) && frame.contains("<iq"))
        .await
        .expect("bob subscribe response");

    assert!(
        bob_resp.contains(r#"type='error'"#),
        "outcast subscribe should be denied: {bob_resp}"
    );
    assert!(
        bob_resp.contains("<error"),
        "expected <error> element: {bob_resp}"
    );
    assert!(
        !bob_resp.contains(r#"type='result'"#),
        "should not return result: {bob_resp}"
    );

    // Drain any remaining frames before dropping.
    let _ = bob.recv_timeout(Duration::from_millis(200)).await;
    let _ = bob.close().await;
    let _ = admin.close().await;
}

// ============================================================================
// XEP-0060 §7.1 publish-time fan-out (#238)
// ============================================================================

/// Set the access model on a PEP-style node so non-owner subscribers are
/// admitted regardless of presence — presence-driven filtering is out of
/// scope for #238.
async fn configure_open_access(client: &mut WsXmppClient, owner: &str, node: &str, id: &str) {
    let resp = iq_set_to(
        client,
        id,
        owner,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{node}"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#access_model"><value>open</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(
        resp.contains(r#"type='result'"#),
        "configure access_model=open: {resp}"
    );
}

async fn auto_create_node(client: &mut WsXmppClient, owner: &str, node: &str, id: &str) {
    let resp = iq_set_to(
        client,
        id,
        owner,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{node}"><item id="seed"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        resp.contains(r#"type='result'"#),
        "auto-create publish: {resp}"
    );
}

async fn subscribe_with_jid(
    client: &mut WsXmppClient,
    owner: &str,
    node: &str,
    subscriber_jid: &str,
    id: &str,
) {
    let resp = iq_set_to(
        client,
        id,
        owner,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="{node}" jid="{subscriber_jid}"/></pubsub>"#
        ),
    )
    .await;
    assert!(resp.contains(r#"type='result'"#), "subscribe: {resp}");
}

#[tokio::test]
async fn publish_fans_event_with_payload_to_subscriber() {
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let mut admin = admin_client(&server, "fanout-payload-admin").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");

    auto_create_node(&mut admin, &admin_bare, "fanout-payload", "fp-create").await;
    configure_open_access(&mut admin, &admin_bare, "fanout-payload", "fp-cfg").await;

    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "fanout-payload-bob",
    )
    .await
    .expect("bob connect");

    subscribe_with_jid(&mut bob, &admin_bare, "fanout-payload", &bob_bare, "fp-sub").await;

    // Admin publishes — bob should receive a §7.1 event message.
    let pub_resp = iq_set_to(
        &mut admin,
        "fp-pub",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="fanout-payload"><item id="event-1"><nick xmlns="http://jabber.org/protocol/nick">Juliet</nick></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let event = wait_for_event_message(&mut bob, "fanout-payload", Duration::from_secs(2))
        .await
        .expect("bob should receive an event message");

    assert!(
        event.contains(r#"type='headline'"#) || event.contains(r#"type='headline'"#),
        "event must be type=headline (XEP-0060 §12.18 / XEP-0163 §4.3): {event}"
    );
    assert!(
        event.contains(&format!(r#"from='{admin_bare}'"#))
            || event.contains(&format!(r#"from='{admin_bare}'"#)),
        "from must be the node owner (XEP-0060 §7.1.2.1): {event}"
    );
    assert!(
        event.contains(r#"id='event-1'"#) || event.contains(r#"id='event-1'"#),
        "event item id must round-trip: {event}"
    );
    assert!(
        event.contains("<nick"),
        "deliver_payloads=true (PEP default) must include payload: {event}"
    );

    let _ = bob.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn publish_strips_payload_when_deliver_payloads_is_false() {
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let mut admin = admin_client(&server, "fanout-nopayload-admin").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");

    auto_create_node(&mut admin, &admin_bare, "fanout-nopayload", "np-create").await;

    // Set both fields in one form — `parse_configure_form` starts from
    // `NodeConfig::default()`, so any unspecified field reverts to its
    // PEP default (e.g. access_model=presence). Bundling preserves
    // open access alongside deliver_payloads=0 (§7.1.3.5).
    let cfg_resp = iq_set_to(
        &mut admin,
        "np-cfg",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="fanout-nopayload"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#access_model"><value>open</value></field><field var="pubsub#deliver_payloads"><value>0</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(
        cfg_resp.contains(r#"type='result'"#),
        "configure open + deliver_payloads=0: {cfg_resp}"
    );

    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "fanout-nopayload-bob",
    )
    .await
    .expect("bob connect");

    subscribe_with_jid(
        &mut bob,
        &admin_bare,
        "fanout-nopayload",
        &bob_bare,
        "np-sub",
    )
    .await;

    let pub_resp = iq_set_to(
        &mut admin,
        "np-pub",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="fanout-nopayload"><item id="event-2"><nick xmlns="http://jabber.org/protocol/nick">Juliet</nick></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let event = wait_for_event_message(&mut bob, "fanout-nopayload", Duration::from_secs(2))
        .await
        .expect("bob should receive event");

    assert!(
        event.contains(r#"id='event-2'"#) || event.contains(r#"id='event-2'"#),
        "event item id must be present: {event}"
    );
    assert!(
        !event.contains("<nick"),
        "deliver_payloads=false must strip payload (§7.1.3.5): {event}"
    );

    let _ = bob.close().await;
    let _ = admin.close().await;
}

// XEP-0060 §6.1.7 single-resource fan-out (FullJid subscribers) is not yet
// implemented: the SQL store normalises subscriber JIDs to bare on insert
// (`server/crates/waddle-server/src/pubsub.rs` — see "full-JID subscriptions
// are not currently supported in the DB store"). The fan-out helper has the
// `try_as_full()` branch ready, so once the storage layer round-trips full
// JIDs the helper will route §6.1.7 traffic correctly without further
// changes. Tracking ticket should add a `full_jid_subscription_targets_only_that_resource`
// case alongside the storage fix.

#[tokio::test]
async fn bare_jid_subscription_targets_all_resources() {
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let mut admin = admin_client(&server, "fanout-barejid-admin").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");

    auto_create_node(&mut admin, &admin_bare, "fanout-bare", "bj-create").await;
    configure_open_access(&mut admin, &admin_bare, "fanout-bare", "bj-cfg").await;

    let mut bob_r1 = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "fanout-bare-r1",
    )
    .await
    .expect("bob r1 connect");
    let mut bob_r2 = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "fanout-bare-r2",
    )
    .await
    .expect("bob r2 connect");

    // Single subscription using the BARE jid — both resources must be
    // notified (XEP-0060 §6.1.6).
    subscribe_with_jid(
        &mut bob_r1,
        &admin_bare,
        "fanout-bare",
        &bob_bare,
        "bj-sub-bare",
    )
    .await;

    let pub_resp = iq_set_to(
        &mut admin,
        "bj-pub",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="fanout-bare"><item id="event-4"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let r1 = wait_for_event_message(&mut bob_r1, "fanout-bare", Duration::from_secs(2))
        .await
        .expect("r1 must receive event from bare-JID subscription");
    assert!(r1.contains(r#"id='event-4'"#), "{r1}");

    let r2 = wait_for_event_message(&mut bob_r2, "fanout-bare", Duration::from_secs(2))
        .await
        .expect("r2 must also receive event from bare-JID subscription");
    assert!(r2.contains(r#"id='event-4'"#), "{r2}");

    let _ = bob_r1.close().await;
    let _ = bob_r2.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn event_omits_publisher_attribute_when_publisher_equals_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let mut admin = admin_client(&server, "fanout-pub-eq-admin").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");

    auto_create_node(&mut admin, &admin_bare, "fanout-pub-eq", "pe-create").await;
    configure_open_access(&mut admin, &admin_bare, "fanout-pub-eq", "pe-cfg").await;

    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "fanout-pub-eq-bob",
    )
    .await
    .expect("bob connect");

    subscribe_with_jid(&mut bob, &admin_bare, "fanout-pub-eq", &bob_bare, "pe-sub").await;

    // Admin (owner) publishes — XEP-0060 §7.1.5: omit `publisher` attr.
    let pub_resp = iq_set_to(
        &mut admin,
        "pe-pub",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="fanout-pub-eq"><item id="event-5"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let event = wait_for_event_message(&mut bob, "fanout-pub-eq", Duration::from_secs(2))
        .await
        .expect("bob must receive event");

    assert!(
        !event.contains("publisher="),
        "publisher attr must be omitted when publisher == owner (§7.1.5): {event}"
    );

    let _ = bob.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn event_includes_publisher_attribute_when_publisher_differs_from_owner() {
    let _serial = TEST_SERIAL.lock().await;
    let charlie_password = format!("ws-test-charlie-{}", uuid::Uuid::new_v4());
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("charlie", charlie_password.as_str()),
        ("bob", bob_password.as_str()),
    ]);
    let mut admin = admin_client(&server, "fanout-pub-ne-admin").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let charlie_bare = format!("charlie@{DOMAIN}");

    auto_create_node(&mut admin, &admin_bare, "fanout-pub-ne", "pn-create").await;
    configure_open_access(&mut admin, &admin_bare, "fanout-pub-ne", "pn-cfg").await;

    // Grant charlie publisher affiliation on admin's node so charlie can
    // publish on a node owned by admin.
    let aff_resp = iq_set_to(
        &mut admin,
        "pn-aff",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><affiliations node="fanout-pub-ne"><affiliation xmlns="{NS_PUBSUB_OWNER}" jid="{charlie_bare}" affiliation="publisher"/></affiliations></pubsub>"#
        ),
    )
    .await;
    assert!(
        aff_resp.contains(r#"type='result'"#),
        "grant publisher affiliation: {aff_resp}"
    );

    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "fanout-pub-ne-bob",
    )
    .await
    .expect("bob connect");
    subscribe_with_jid(&mut bob, &admin_bare, "fanout-pub-ne", &bob_bare, "pn-sub").await;

    // Charlie connects and publishes to admin's node.
    let mut charlie = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "charlie",
        &charlie_password,
        "fanout-pub-ne-charlie",
    )
    .await
    .expect("charlie connect");
    let pub_resp = iq_set_to(
        &mut charlie,
        "pn-pub",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="fanout-pub-ne"><item id="event-6"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        pub_resp.contains(r#"type='result'"#),
        "charlie publish to admin node: {pub_resp}"
    );

    let event = wait_for_event_message(&mut bob, "fanout-pub-ne", Duration::from_secs(2))
        .await
        .expect("bob must receive event");

    assert!(
        event.contains(&format!(r#"publisher='{charlie_bare}'"#))
            || event.contains(&format!(r#"publisher='{charlie_bare}'"#)),
        "publisher attribute must carry charlie's bare JID (§7.1.5): {event}"
    );

    let _ = bob.close().await;
    let _ = charlie.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn publish_with_no_subscribers_returns_result_and_emits_no_event() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "fanout-empty-admin").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");

    auto_create_node(&mut admin, &admin_bare, "fanout-empty", "em-create").await;

    // Publish with no subscribers — the IQ result must arrive, and admin
    // (the only connected user, not subscribed to its own node) must not
    // receive any event-message frame.
    let pub_resp = iq_set_to(
        &mut admin,
        "em-pub",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="fanout-empty"><item id="event-7"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        pub_resp.contains(r#"type='result'"#),
        "publish without subscribers must still succeed: {pub_resp}"
    );
    assert_no_event_message(&mut admin, "fanout-empty", Duration::from_millis(500)).await;

    let _ = admin.close().await;
}
