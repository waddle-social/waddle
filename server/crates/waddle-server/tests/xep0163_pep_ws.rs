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
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
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
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
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
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="home@muc.example.com"><conference xmlns="urn:xmpp:bookmarks:1" name="Home"/></item></publish></pubsub>"#
        ),
    )
    .await;

    assert!(
        resp.contains(r#"type='result'"#),
        "auto-create+publish to own PEP node must succeed (XEP-0163 §3): {resp}"
    );
    assert!(
        !resp.contains(r#"type='error'"#),
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
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="home@muc.example.com"><conference xmlns="urn:xmpp:bookmarks:1" name="Home"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        r1.contains(r#"type='result'"#),
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
        r#"<iq type="set" id="bob-pub-1" to="{admin_bare}"><pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="evil@muc.example.com"><conference xmlns="urn:xmpp:bookmarks:1" name="Evil"/></item></publish></pubsub></iq>"#
    ))
    .await
    .expect("send bob publish");

    let bob_resp = bob
        .recv_matching(|frame| frame.contains(r#"id='bob-pub-1'"#) && frame.contains("<iq"))
        .await
        .expect("bob publish response");

    assert!(
        bob_resp.contains(r#"type='error'"#),
        "non-owner publish to PEP node must be forbidden (XEP-0163 §4): {bob_resp}"
    );
    assert!(
        bob_resp.contains("<error"),
        "expected <error> element in response: {bob_resp}"
    );
    assert!(
        !bob_resp.contains(r#"type='result'"#),
        "must not return success: {bob_resp}"
    );

    // Drain any remaining frames before dropping.
    let _ = bob.recv_timeout(Duration::from_millis(200)).await;
    let _ = bob.close().await;
    let _ = admin.close().await;
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

    // Publish first item to a generic PEP node (auto-creates with max_items=1).
    let r1 = iq_set_to(
        &mut admin,
        "pep-mi-pub-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="http://jabber.org/protocol/mood"><item id="i1"><mood xmlns="http://jabber.org/protocol/mood"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r1.contains(r#"type='result'"#), "publish i1: {r1}");

    // Publish second item — i1 must be evicted (max_items=1).
    let r2 = iq_set_to(
        &mut admin,
        "pep-mi-pub-2",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="http://jabber.org/protocol/mood"><item id="i2"><mood xmlns="http://jabber.org/protocol/mood"><sad/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r2.contains(r#"type='result'"#), "publish i2: {r2}");

    // Retrieve items — only i2 should remain.
    let items_resp = iq_get_to(
        &mut admin,
        "pep-mi-items-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><items node="http://jabber.org/protocol/mood"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        items_resp.contains(r#"type='result'"#),
        "items get: {items_resp}"
    );
    assert!(
        !items_resp.contains(r#"id='i1'"#),
        "i1 must have been evicted by max_items=1 PEP default: {items_resp}"
    );
    assert!(
        items_resp.contains(r#"id='i2'"#),
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
// XEP-0060 §8.5 (owner namespace) is used to purge a PEP node. We first
// configure max_items=10 so this test controls the staged-item count even if
// node-specific PEP defaults change.

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
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="p1@muc.example.com"><conference xmlns="urn:xmpp:bookmarks:1" name="One"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        r1.contains(r#"type='result'"#),
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
        cfg.contains(r#"type='result'"#),
        "configure max_items=10: {cfg}"
    );

    // Publish two more items so the node holds 3 items total.
    let r2 = iq_set_to(
        &mut admin,
        "pep-purge-pub-2",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="p2@muc.example.com"><conference xmlns="urn:xmpp:bookmarks:1" name="Two"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r2.contains(r#"type='result'"#), "publish p2: {r2}");

    let r3 = iq_set_to(
        &mut admin,
        "pep-purge-pub-3",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="p3@muc.example.com"><conference xmlns="urn:xmpp:bookmarks:1" name="Three"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r3.contains(r#"type='result'"#), "publish p3: {r3}");

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
        purge_resp.contains(r#"type='result'"#),
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
        after.contains(r#"type='result'"#),
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
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="urn:xmpp:bookmarks:1"><item id="p4@muc.example.com"><conference xmlns="urn:xmpp:bookmarks:1" name="Four"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        post_purge.contains(r#"type='result'"#),
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
                    && (frame.contains(&format!(r#"node='{node}'"#))
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
    let node = "http://jabber.org/protocol/mood";

    // Auto-create the PEP node and configure access_model=open so bob can
    // subscribe without presence subscription (presence-driven filtering
    // is out of scope per #238).
    let r1 = iq_set_to(
        &mut admin,
        "pep-fanout-create",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{node}"><item id="seed"><mood xmlns="{node}"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r1.contains(r#"type='result'"#), "create: {r1}");

    let cfg = iq_set_to(
        &mut admin,
        "pep-fanout-cfg",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{node}"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#access_model"><value>open</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(cfg.contains(r#"type='result'"#), "configure open: {cfg}");

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
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="{node}" jid="{bob_bare}"/></pubsub>"#
        ),
    )
    .await;
    assert!(sub.contains(r#"type='result'"#), "subscribe: {sub}");

    let pub_resp = iq_set_to(
        &mut admin,
        "pep-fanout-pub",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{node}"><item id="mood-1"><mood xmlns="{node}"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let event = wait_for_event_message(&mut bob, node, Duration::from_secs(2))
        .await
        .expect("bob must receive PEP event");

    // XEP-0163 §4.3: PEP `from` is the bare account JID.
    assert!(
        event.contains(&format!(r#"from='{admin_bare}'"#))
            || event.contains(&format!(r#"from='{admin_bare}'"#)),
        "from must be the PEP account bare JID: {event}"
    );
    // XEP-0163 §4.3 + XEP-0060 §12.18: PEP MUST be headline.
    assert!(
        event.contains(r#"type='headline'"#) || event.contains(r#"type='headline'"#),
        "PEP event must be type=headline: {event}"
    );
    assert!(
        event.contains(r#"id='mood-1'"#),
        "item id must round-trip: {event}"
    );
    // §7.1.5: publisher == owner here (admin published to own PEP), so
    // no publisher attribute should be emitted.
    assert!(
        !event.contains("publisher="),
        "publisher attr must be omitted on PEP self-publish: {event}"
    );

    let _ = bob.close().await;
    let _ = admin.close().await;
}

// ============================================================================
// XEP-0163 §3 — presence-driven roster + CAPS fan-out (RFC 363 PR 2)
// ============================================================================
//
// The §3 contract: when a user publishes a PEP item, the server
// delivers <message><event> to every entity that has roster
// `subscription = from` or `both` to the publisher AND advertises
// `<node>+notify` in cached CAPS. There is NO explicit pubsub
// <subscribe/> involved — roster + CAPS is the filter.

const NS_CAPS: &str = "http://jabber.org/protocol/caps";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";

fn caps_verification_string(
    identity_category: &str,
    identity_type: &str,
    identity_name: &str,
    features: &[&str],
) -> String {
    use waddle_xmpp::disco::info::{Feature, Identity};
    use waddle_xmpp::xep::xep0115::compute_caps_hash;
    let identities = vec![Identity::new(
        identity_category,
        identity_type,
        Some(identity_name),
    )];
    let features: Vec<Feature> = features.iter().map(|f| Feature::new(f)).collect();
    compute_caps_hash(&identities, &features)
}

fn extract_iq_id(frame: &str) -> String {
    use ws_common::extract_attr_after;
    extract_attr_after(frame, "<iq", "id").expect("iq has id attribute")
}

/// Send a ping IQ and wait for its result. Used as a deterministic
/// FIFO anchor in place of `tokio::time::sleep` for "the prior frame
/// has been processed" assertions: anything the server emitted before
/// the ping reply (e.g. caps-disco round-trip completion, fan-out
/// dispatch) is already in the client's recv queue when the ping
/// result lands. Mirrors the anchor pattern in
/// `tests/xep0115_caps_ws.rs`.
async fn ping_anchor(client: &mut WsXmppClient, id: &str) {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}"><ping xmlns="urn:xmpp:ping"/></iq>"#
        ))
        .await
        .expect("send ping");
    let _ = client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("ping result");
}

/// Establish bob -> alice presence subscription so alice's roster
/// has bob with `subscription = from` (alice's PEP fan-out target).
async fn establish_bob_subscribes_to_alice(
    alice: &mut WsXmppClient,
    bob: &mut WsXmppClient,
    alice_bare: &str,
    bob_bare: &str,
) {
    alice
        .send(r#"<iq xmlns="jabber:client" type="get" id="roster-init-a"><query xmlns="jabber:iq:roster"/></iq>"#)
        .await
        .expect("alice roster get");
    let _ = alice
        .recv_matching(|f| f.contains("roster-init-a"))
        .await
        .expect("alice roster result");
    bob.send(r#"<iq xmlns="jabber:client" type="get" id="roster-init-b"><query xmlns="jabber:iq:roster"/></iq>"#)
        .await
        .expect("bob roster get");
    let _ = bob
        .recv_matching(|f| f.contains("roster-init-b"))
        .await
        .expect("bob roster result");

    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice presence");
    bob.send(&format!(
        r#"<presence xmlns="jabber:client" type="subscribe" to="{alice_bare}"/>"#
    ))
    .await
    .expect("bob subscribes");
    let _subscribe = alice
        .recv_matching(|f| f.contains(r#"type='subscribe'"#))
        .await
        .expect("alice receives subscribe");
    alice
        .send(&format!(
            r#"<presence xmlns="jabber:client" type="subscribed" to="{bob_bare}"/>"#
        ))
        .await
        .expect("alice approves");
    let _subscribed = bob
        .recv_matching(|f| f.contains(r#"type='subscribed'"#))
        .await
        .expect("bob receives approval");
}

#[tokio::test]
async fn pep_publish_fans_to_roster_contacts_with_matching_caps_notify() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-r1",
    )
    .await
    .expect("alice connect");
    let mut bob =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, "bob", &bob_password, "bob-r1")
            .await
            .expect("bob connect");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let bob_full = bob.full_jid.clone().expect("bob full jid");

    establish_bob_subscribes_to_alice(&mut alice, &mut bob, &alice_bare, &bob_bare).await;

    let node = "http://jabber.org/protocol/mood";
    let notify_var = format!("{node}+notify");
    let features = ["http://jabber.org/protocol/disco#info", notify_var.as_str()];
    let caps_node = "https://bob.example/caps";
    let ver = caps_verification_string("client", "pc", "Bob's Client", &features);

    bob.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
    ))
    .await
    .expect("bob presence with caps");
    let disco_query = bob
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries bob caps");
    let iq_id = extract_iq_id(&disco_query);
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Bob's Client"/>{feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob disco#info reply");

    ping_anchor(
        &mut bob,
        &format!("pep-bob-anchor-{}", uuid::Uuid::new_v4()),
    )
    .await;

    let pub_resp = iq_set_to(
        &mut alice,
        "pep-roster-pub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{node}"><item id="mood-roster-1"><mood xmlns="{node}"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let event = wait_for_event_message(&mut bob, node, Duration::from_secs(2))
        .await
        .expect(
            "bob in alice's roster (subscription=from) and advertising +notify MUST receive the PEP event without explicit pubsub <subscribe/> per XEP-0163 §3",
        );
    assert!(
        event.contains(&format!(r#"from='{alice_bare}'"#))
            || event.contains(&format!(r#"from='{alice_bare}'"#)),
        "fan-out from MUST be alice's bare JID per XEP-0163 §4.3: {event}"
    );
    assert!(
        event.contains(r#"id='mood-roster-1'"#),
        "item id must round-trip: {event}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn pep_bookmark_publish_skips_roster_contacts_even_with_matching_caps_notify() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-bookmark-private-1",
    )
    .await
    .expect("alice connect");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "bob-bookmark-private-1",
    )
    .await
    .expect("bob connect");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let bob_full = bob.full_jid.clone().expect("bob full jid");

    establish_bob_subscribes_to_alice(&mut alice, &mut bob, &alice_bare, &bob_bare).await;

    let node = "urn:xmpp:bookmarks:1";
    let notify_var = format!("{node}+notify");
    let features = ["http://jabber.org/protocol/disco#info", notify_var.as_str()];
    let caps_node = "https://bob.example/bookmark-private-caps";
    let ver = caps_verification_string("client", "pc", "Bob's Client", &features);

    bob.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
    ))
    .await
    .expect("bob presence with caps");
    let disco_query = bob
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries bob caps");
    let iq_id = extract_iq_id(&disco_query);
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Bob's Client"/>{feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob disco#info reply");

    ping_anchor(
        &mut bob,
        &format!("pep-bookmark-private-anchor-{}", uuid::Uuid::new_v4()),
    )
    .await;

    let pub_resp = iq_set_to(
        &mut alice,
        "pep-bookmark-private-pub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{node}"><item id="private-room@muc.example.com"><conference xmlns="{node}" name="Private"><extensions><notify xmlns="urn:xmpp:notification-settings:1"><never/></notify></extensions></conference></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let event = wait_for_event_message(&mut bob, node, Duration::from_millis(700)).await;
    assert!(
        event.is_none(),
        "bookmarks are private PEP state and MUST NOT fan out to roster contacts: {event:?}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn pep_bookmark_member_affiliation_grants_no_event_delivery() {
    // XEP-0402 bookmarks are owner-only regardless of affiliation: the
    // `can_subscribe` carve-out denies a member-affiliated non-owner,
    // and the fan-out entitlement must apply the SAME rule. An owner
    // subscriptions-set (§8.8) can force a subscription row without a
    // can_subscribe check, so fan-out must not treat "subscription row
    // exists + member affiliation" as authorization on this node.
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-bookmark-member-1",
    )
    .await
    .expect("alice connect");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "bob-bookmark-member-1",
    )
    .await
    .expect("bob connect");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let node = "urn:xmpp:bookmarks:1";

    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice presence");
    bob.send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("bob presence");

    let seed = iq_set_to(
        &mut alice,
        "pep-bookmark-member-seed",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{node}"><item id="seed-member@muc.example.com"><conference xmlns="{node}" name="Seed"><extensions><notify xmlns="urn:xmpp:notification-settings:1"><never/></notify></extensions></conference></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(seed.contains(r#"type='result'"#), "seed publish: {seed}");

    // Owner affiliates bob as member (§8.9.4) and force-subscribes him
    // via the owner subscriptions-set (§8.8), which bypasses
    // can_subscribe.
    let aff = iq_set_to(
        &mut alice,
        "pep-bookmark-member-aff",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><affiliations node="{node}"><affiliation jid="{bob_bare}" affiliation="member"/></affiliations></pubsub>"#
        ),
    )
    .await;
    assert!(aff.contains(r#"type='result'"#), "affiliations set: {aff}");
    let subs = iq_set_to(
        &mut alice,
        "pep-bookmark-member-subs",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><subscriptions node="{node}"><subscription jid="{bob_bare}" subscription="subscribed"/></subscriptions></pubsub>"#
        ),
    )
    .await;
    assert!(
        subs.contains(r#"type='result'"#),
        "owner subscriptions set: {subs}"
    );

    let pub_resp = iq_set_to(
        &mut alice,
        "pep-bookmark-member-pub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{node}"><item id="private-member@muc.example.com"><conference xmlns="{node}" name="Private"><extensions><notify xmlns="urn:xmpp:notification-settings:1"><never/></notify></extensions></conference></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let event = wait_for_event_message(&mut bob, node, Duration::from_millis(700)).await;
    assert!(
        event.is_none(),
        "bookmarks are owner-only even for member-affiliated subscribers; leaked: {event:?}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn pep_bookmark_open_config_does_not_grant_non_owner_access() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-bookmark-explicit-1",
    )
    .await
    .expect("alice connect");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "bob-bookmark-explicit-1",
    )
    .await
    .expect("bob connect");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let node = "urn:xmpp:bookmarks:1";

    let create = iq_set_to(
        &mut alice,
        "pep-bookmark-explicit-create",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{node}"><item id="seed-explicit@muc.example.com"><conference xmlns="{node}" name="Seed"><extensions><notify xmlns="urn:xmpp:notification-settings:1"><never/></notify></extensions></conference></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        create.contains(r#"type='result'"#),
        "create bookmark node: {create}"
    );
    let cfg = iq_set_to(
        &mut alice,
        "pep-bookmark-explicit-cfg",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{node}"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#access_model"><value>open</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(cfg.contains(r#"type='result'"#), "configure: {cfg}");

    let sub = iq_set_to(
        &mut bob,
        "pep-bookmark-explicit-sub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="{node}" jid="{bob_bare}"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        sub.contains(r#"type='error'"#),
        "bookmark node must stay private even after open config request: {sub}"
    );
    assert!(
        sub.contains("forbidden"),
        "non-owner bookmark subscribe should fail as forbidden: {sub}"
    );

    let read = iq_get_to(
        &mut bob,
        "pep-bookmark-explicit-read",
        &alice_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{node}"/></pubsub>"#),
    )
    .await;
    assert!(
        read.contains(r#"type='error'"#),
        "bookmark items must stay private even after open config request: {read}"
    );
    assert!(
        read.contains("forbidden"),
        "non-owner bookmark items read should fail as forbidden: {read}"
    );
    assert!(
        !read.contains("seed-explicit@muc.example.com")
            && !read.contains("urn:xmpp:notification-settings:1"),
        "private bookmark item id and notification settings must not leak: {read}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn pep_bookmark_items_reject_non_owner_reads() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-bookmark-read-1",
    )
    .await
    .expect("alice connect");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "bob-bookmark-read-1",
    )
    .await
    .expect("bob connect");
    let alice_bare = format!("alice@{DOMAIN}");
    let node = "urn:xmpp:bookmarks:1";

    let publish = iq_set_to(
        &mut alice,
        "pep-bookmark-read-create",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{node}"><item id="private-read@muc.example.com"><conference xmlns="{node}" name="Private"><extensions><notify xmlns="urn:xmpp:notification-settings:1"><never/></notify></extensions></conference></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(publish.contains(r#"type='result'"#), "publish: {publish}");

    let read = iq_get_to(
        &mut bob,
        "pep-bookmark-read-bob",
        &alice_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{node}"/></pubsub>"#),
    )
    .await;
    assert!(
        read.contains(r#"type='error'"#),
        "non-owner bookmark read must be rejected: {read}"
    );
    assert!(
        read.contains("forbidden"),
        "non-owner bookmark read should fail as forbidden: {read}"
    );
    assert!(
        !read.contains("private-read@muc.example.com")
            && !read.contains("urn:xmpp:notification-settings:1"),
        "private bookmark and notification settings must not leak: {read}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

// ============================================================================
// XEP-0163 §3 — roster contact without `+notify` MUST NOT receive
// ============================================================================
//
// Roster `subscription = from/both` is necessary but not sufficient.
// A contact whose cached CAPS do not include `<node>+notify` MUST NOT
// receive the event. Without this filter the server would spam every
// roster contact for every PEP node, defeating the entire point of
// XEP-0115's selective subscription model.

#[tokio::test]
async fn pep_publish_skips_roster_contact_without_caps_notify_filter() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-no-notify-1",
    )
    .await
    .expect("alice");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "bob-no-notify-1",
    )
    .await
    .expect("bob");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let bob_full = bob.full_jid.clone().expect("bob full jid");

    establish_bob_subscribes_to_alice(&mut alice, &mut bob, &alice_bare, &bob_bare).await;

    // Bob advertises caps, but with NO `+notify` filter for the
    // node alice will publish to.
    let publish_node = "http://jabber.org/protocol/mood";
    let features = [
        "http://jabber.org/protocol/disco#info",
        // Notice: no http://jabber.org/protocol/mood+notify
        "urn:xmpp:ping",
    ];
    let caps_node = "https://bob.example/caps-no-notify";
    let ver = caps_verification_string("client", "pc", "Bob's Client", &features);

    bob.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
    ))
    .await
    .expect("bob presence with caps");
    let disco_query = bob
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries bob caps");
    let iq_id = extract_iq_id(&disco_query);
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Bob's Client"/>{feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob disco#info reply");

    ping_anchor(
        &mut bob,
        &format!("pep-bob-anchor-{}", uuid::Uuid::new_v4()),
    )
    .await;

    let pub_resp = iq_set_to(
        &mut alice,
        "pep-no-notify-pub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{publish_node}"><item id="silent-1"><mood xmlns="{publish_node}"><sad/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let event = wait_for_event_message(&mut bob, publish_node, Duration::from_millis(700)).await;
    assert!(
        event.is_none(),
        "bob without `<node>+notify` MUST NOT receive the PEP event; got: {event:?}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

// ============================================================================
// XEP-0163 §3 — per-resource semantics: only resources with the
// matching +notify receive
// ============================================================================
//
// A user with multiple resources MUST receive the event only on the
// resources whose cached CAPS include the matching `+notify`. The
// non-advertising resource is skipped, not delivered-to-and-ignored.
// This matters because resources may legitimately advertise different
// feature sets (web vs mobile vs CLI) — fan-out must respect each
// resource's CAPS independently.

#[tokio::test]
async fn pep_publish_targets_only_resources_advertising_notify_filter() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-multi-1",
    )
    .await
    .expect("alice");
    let mut bob_a = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "bob-resource-A",
    )
    .await
    .expect("bob-A");
    let mut bob_b = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "bob-resource-B",
    )
    .await
    .expect("bob-B");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let bob_a_full = bob_a.full_jid.clone().expect("bob-A jid");
    let bob_b_full = bob_b.full_jid.clone().expect("bob-B jid");

    // Subscription handshake from bob-A (any resource is sufficient
    // to make alice's roster have bob with subscription=from).
    establish_bob_subscribes_to_alice(&mut alice, &mut bob_a, &alice_bare, &bob_bare).await;
    bob_b
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("bob-B presence");

    let publish_node = "http://jabber.org/protocol/mood";
    let notify_var = format!("{publish_node}+notify");

    // Resource A: advertises +notify
    let a_features = ["http://jabber.org/protocol/disco#info", notify_var.as_str()];
    let a_caps_node = "https://bob.example/A";
    let a_ver = caps_verification_string("client", "pc", "Bob A", &a_features);
    bob_a
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{a_caps_node}" ver="{a_ver}"/></presence>"#
        ))
        .await
        .expect("bob-A presence with caps");
    let a_query = bob_a
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("disco#info to bob-A");
    let a_id = extract_iq_id(&a_query);
    let a_features_xml: String = a_features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob_a
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{a_id}" from="{bob_a_full}"><query xmlns="{NS_DISCO_INFO}" node="{a_caps_node}#{a_ver}"><identity category="client" type="pc" name="Bob A"/>{a_features_xml}</query></iq>"#
        ))
        .await
        .expect("bob-A disco#info reply");

    // Resource B: NO +notify
    let b_features = ["http://jabber.org/protocol/disco#info", "urn:xmpp:ping"];
    let b_caps_node = "https://bob.example/B";
    let b_ver = caps_verification_string("client", "pc", "Bob B", &b_features);
    bob_b
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{b_caps_node}" ver="{b_ver}"/></presence>"#
        ))
        .await
        .expect("bob-B presence with caps");
    let b_query = bob_b
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("disco#info to bob-B");
    let b_id = extract_iq_id(&b_query);
    let b_features_xml: String = b_features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob_b
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{b_id}" from="{bob_b_full}"><query xmlns="{NS_DISCO_INFO}" node="{b_caps_node}#{b_ver}"><identity category="client" type="pc" name="Bob B"/>{b_features_xml}</query></iq>"#
        ))
        .await
        .expect("bob-B disco#info reply");

    ping_anchor(
        &mut bob_a,
        &format!("pep-multi-anchor-a-{}", uuid::Uuid::new_v4()),
    )
    .await;
    ping_anchor(
        &mut bob_b,
        &format!("pep-multi-anchor-b-{}", uuid::Uuid::new_v4()),
    )
    .await;

    let pub_resp = iq_set_to(
        &mut alice,
        "pep-multi-pub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{publish_node}"><item id="multi-1"><mood xmlns="{publish_node}"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let a_event = wait_for_event_message(&mut bob_a, publish_node, Duration::from_secs(2))
        .await
        .expect("resource A advertising +notify MUST receive the event");
    assert!(
        a_event.contains(r#"id='multi-1'"#),
        "item id must round-trip on A: {a_event}"
    );

    let b_event =
        wait_for_event_message(&mut bob_b, publish_node, Duration::from_millis(700)).await;
    assert!(
        b_event.is_none(),
        "resource B without +notify MUST NOT receive the event (per-resource §3 semantics): {b_event:?}"
    );

    let _ = bob_a.close().await;
    let _ = bob_b.close().await;
    let _ = alice.close().await;
}

// ============================================================================
// XEP-0163 §3 — overlap dedup: explicit subscriber + roster + +notify
// ============================================================================
//
// A roster contact who is also an explicit pubsub subscriber AND
// advertises +notify must receive exactly ONE event, not two. The
// already_delivered HashSet inside fan_out_publish is the dedup seam.

#[tokio::test]
async fn pep_publish_delivers_exactly_once_when_subscriber_also_in_roster() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-dedup-1",
    )
    .await
    .expect("alice");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "bob-dedup-1",
    )
    .await
    .expect("bob");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let bob_full = bob.full_jid.clone().expect("bob full jid");

    // Alice opens an open-access PEP node so bob can both subscribe AND
    // satisfy presence-driven §3 fan-out.
    let publish_node = "http://jabber.org/protocol/mood";
    let r1 = iq_set_to(
        &mut alice,
        "pep-dedup-create",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{publish_node}"><item id="seed"><mood xmlns="{publish_node}"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(r1.contains(r#"type='result'"#), "create: {r1}");
    let cfg = iq_set_to(
        &mut alice,
        "pep-dedup-cfg",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{publish_node}"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#access_model"><value>open</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(cfg.contains(r#"type='result'"#), "configure: {cfg}");

    establish_bob_subscribes_to_alice(&mut alice, &mut bob, &alice_bare, &bob_bare).await;

    // Bob explicitly subscribes via XEP-0060.
    let sub = iq_set_to(
        &mut bob,
        "pep-dedup-sub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="{publish_node}" jid="{bob_bare}"/></pubsub>"#
        ),
    )
    .await;
    assert!(sub.contains(r#"type='result'"#), "subscribe: {sub}");

    // Bob also advertises +notify in CAPS.
    let notify_var = format!("{publish_node}+notify");
    let features = ["http://jabber.org/protocol/disco#info", notify_var.as_str()];
    let caps_node = "https://bob.example/dedup";
    let ver = caps_verification_string("client", "pc", "Bob's Client", &features);
    bob.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
    ))
    .await
    .expect("bob caps presence");
    let disco_query = bob
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("disco#info to bob");
    let iq_id = extract_iq_id(&disco_query);
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Bob's Client"/>{feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob disco#info reply");

    ping_anchor(
        &mut bob,
        &format!("pep-bob-anchor-{}", uuid::Uuid::new_v4()),
    )
    .await;

    let pub_resp = iq_set_to(
        &mut alice,
        "pep-dedup-pub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{publish_node}"><item id="dedup-1"><mood xmlns="{publish_node}"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let first = wait_for_event_message(&mut bob, publish_node, Duration::from_secs(2))
        .await
        .expect("bob (subscriber+roster+notify) MUST receive at least one event");
    assert!(
        first.contains(r#"id='dedup-1'"#),
        "expected dedup-1: {first}"
    );

    let second = wait_for_event_message(&mut bob, publish_node, Duration::from_millis(700)).await;
    assert!(
        second.is_none(),
        "MUST receive exactly one event when both subscriber AND roster paths apply; got duplicate: {second:?}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

// ============================================================================
// XEP-0163 §3.4 / PEP §4.2 — owner's other resources receive too
// ============================================================================
//
// "Sending the Last Published Item" reaches "all appropriate
// subscribers", which includes the account owner itself: when alice
// publishes from /r1, alice/r2 must also receive the event so it can
// keep its UI in sync with the publishing resource.

#[tokio::test]
async fn pep_publish_fans_to_owner_other_resources_with_caps_notify() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let mut alice_a = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-self-A",
    )
    .await
    .expect("alice-A");
    let mut alice_b = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-self-B",
    )
    .await
    .expect("alice-B");
    let alice_bare = format!("alice@{DOMAIN}");
    let alice_b_full = alice_b.full_jid.clone().expect("alice-B full jid");

    // Both resources go presence-available.
    alice_a
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice-A presence");
    alice_b
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice-B presence");

    let publish_node = "http://jabber.org/protocol/mood";
    let notify_var = format!("{publish_node}+notify");
    let features = ["http://jabber.org/protocol/disco#info", notify_var.as_str()];
    let caps_node = "https://alice.example/caps";
    let ver = caps_verification_string("client", "pc", "Alice B", &features);

    alice_b
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("alice-B presence with caps");
    let disco_query = alice_b
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("disco#info to alice-B");
    let iq_id = extract_iq_id(&disco_query);
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    alice_b
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{alice_b_full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Alice B"/>{feature_xml}</query></iq>"#
        ))
        .await
        .expect("alice-B disco#info reply");

    ping_anchor(
        &mut alice_b,
        &format!("pep-self-anchor-{}", uuid::Uuid::new_v4()),
    )
    .await;

    let pub_resp = iq_set_to(
        &mut alice_a,
        "pep-self-pub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{publish_node}"><item id="self-1@muc.example.com"><conference xmlns="{publish_node}" name="Self Sync"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let event = wait_for_event_message(&mut alice_b, publish_node, Duration::from_secs(2))
        .await
        .expect(
            "alice-B advertising +notify MUST receive PEP event from alice-A's publish per §3.4",
        );
    assert!(
        event.contains(r#"id='self-1@muc.example.com'"#),
        "item id: {event}"
    );

    let _ = alice_a.close().await;
    let _ = alice_b.close().await;
}

// ============================================================================
// XEP-0163 §3 + XEP-0191 — blocking guards on PEP fan-out
// ============================================================================
//
// XEP-0191 §2 + XEP-0163 §3.3: a PEP service MUST NOT deliver a
// notification when either party has blocked the other. The fan-out
// honors the block in BOTH directions.
//
// PR #439 review issue #4: the fixup commit added the both-direction
// blocking check, but the test suite had no coverage for it.
//
// Helper: drive an XEP-0191 block from `blocker` (already connected
// + roster-interested) targeting `target`. Returns once the IQ-set
// result lands.
async fn xep0191_block(client: &mut WsXmppClient, target_bare: &str, id: &str) {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="{id}"><block xmlns="urn:xmpp:blocking"><item jid="{target_bare}"/></block></iq>"#
        ))
        .await
        .expect("send block IQ");
    let _ = client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("block IQ result");
}

#[tokio::test]
async fn pep_publish_skips_blocked_roster_contact() {
    // alice publishes; bob is in alice's roster (subscription=from)
    // and advertises +notify, but alice has explicitly blocked bob.
    // §3.3 + XEP-0191 §2: bob MUST NOT receive the event.
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-block-1",
    )
    .await
    .expect("alice");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "bob-block-1",
    )
    .await
    .expect("bob");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let bob_full = bob.full_jid.clone().expect("bob full");

    establish_bob_subscribes_to_alice(&mut alice, &mut bob, &alice_bare, &bob_bare).await;

    // Bob advertises +notify so the only path that COULD deliver to
    // him is the §3 roster pass — which the block must veto.
    let publish_node = "http://jabber.org/protocol/mood";
    let notify_var = format!("{publish_node}+notify");
    let features = ["http://jabber.org/protocol/disco#info", notify_var.as_str()];
    let caps_node = "https://bob.example/blocked";
    let ver = caps_verification_string("client", "pc", "Bob's Client", &features);
    bob.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
    ))
    .await
    .expect("bob caps");
    let q = bob
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries bob");
    let qid = extract_iq_id(&q);
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{qid}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Bob's Client"/>{feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob disco reply");
    ping_anchor(&mut bob, "pep-block-anchor-bob-caps").await;

    // Alice blocks bob.
    xep0191_block(&mut alice, &bob_bare, "pep-block-1").await;

    let pub_resp = iq_set_to(
        &mut alice,
        "pep-block-pub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{publish_node}"><item id="blocked-1"><mood xmlns="{publish_node}"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    // Use bob's own ping reply as the FIFO anchor: by the time the
    // ping result arrives, the fan-out for the publish has already
    // been emitted (or skipped).
    ping_anchor(&mut bob, "pep-block-anchor-pub").await;
    let event = wait_for_event_message(&mut bob, publish_node, Duration::from_millis(200)).await;
    assert!(
        event.is_none(),
        "alice blocked bob: bob MUST NOT receive PEP event per §3.3 + XEP-0191 §2; got: {event:?}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn pep_publish_skips_when_contact_blocked_publisher() {
    // bob blocks alice. Alice publishes. bob is in alice's roster
    // (subscription=from) and advertises +notify. §3.3 + XEP-0191 §2:
    // bob MUST NOT receive the event because the contact->publisher
    // direction is also a block.
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-block-2",
    )
    .await
    .expect("alice");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "bob-block-2",
    )
    .await
    .expect("bob");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let bob_full = bob.full_jid.clone().expect("bob full");

    establish_bob_subscribes_to_alice(&mut alice, &mut bob, &alice_bare, &bob_bare).await;

    let publish_node = "http://jabber.org/protocol/mood";
    let notify_var = format!("{publish_node}+notify");
    let features = ["http://jabber.org/protocol/disco#info", notify_var.as_str()];
    let caps_node = "https://bob.example/contact-blocks";
    let ver = caps_verification_string("client", "pc", "Bob's Client", &features);
    bob.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
    ))
    .await
    .expect("bob caps");
    let q = bob
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries bob");
    let qid = extract_iq_id(&q);
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{qid}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Bob's Client"/>{feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob disco reply");
    ping_anchor(&mut bob, "pep-block2-anchor-bob-caps").await;

    // Bob blocks alice (the publisher).
    xep0191_block(&mut bob, &alice_bare, "pep-block-2-rev").await;

    let pub_resp = iq_set_to(
        &mut alice,
        "pep-block2-pub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{publish_node}"><item id="contact-blocks-1"><mood xmlns="{publish_node}"><happy/></mood></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    ping_anchor(&mut bob, "pep-block2-anchor-pub").await;
    let event = wait_for_event_message(&mut bob, publish_node, Duration::from_millis(200)).await;
    assert!(
        event.is_none(),
        "bob blocked alice: §3.3 + XEP-0191 §2 require skipping; got: {event:?}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

// ============================================================================
// PR #439 review issue #3 — publishing resource MUST NOT receive a self-echo
// ============================================================================
#[tokio::test]
async fn pep_publish_does_not_echo_to_publishing_resource() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "alice-noecho-1",
    )
    .await
    .expect("alice");
    let alice_bare = format!("alice@{DOMAIN}");
    let alice_full = alice.full_jid.clone().expect("alice full jid");

    // Alice advertises +notify on her publishing resource.
    let publish_node = "urn:xmpp:bookmarks:1";
    let notify_var = format!("{publish_node}+notify");
    let features = ["http://jabber.org/protocol/disco#info", notify_var.as_str()];
    let caps_node = "https://alice.example/noecho";
    let ver = caps_verification_string("client", "pc", "Alice", &features);

    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice presence");
    alice
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("alice caps presence");
    let q = alice
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries alice");
    let qid = extract_iq_id(&q);
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    alice
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{qid}" from="{alice_full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Alice"/>{feature_xml}</query></iq>"#
        ))
        .await
        .expect("alice disco reply");
    ping_anchor(&mut alice, "pep-noecho-anchor-1").await;

    let pub_resp = iq_set_to(
        &mut alice,
        "pep-noecho-pub",
        &alice_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{publish_node}"><item id="echo-1@muc.example.com"><conference xmlns="{publish_node}" name="Echo"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    ping_anchor(&mut alice, "pep-noecho-anchor-2").await;
    let event = wait_for_event_message(&mut alice, publish_node, Duration::from_millis(200)).await;
    assert!(
        event.is_none(),
        "publishing resource MUST NOT receive its own item back as a §3.4 self-echo; got: {event:?}"
    );

    let _ = alice.close().await;
}
