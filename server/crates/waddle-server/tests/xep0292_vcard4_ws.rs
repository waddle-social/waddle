//! XEP-0292 vCard4 wire-conformance integration tests over WebSocket.
//!
//! Pins the §6.1 canonical access model — `open` — for the
//! `urn:xmpp:vcard4` PEP node:
//!
//! 1. Auto-create on the FIRST chat-editor-shaped publish (a bare
//!    `<publish>` with no `<publish-options>`) MUST land an
//!    `open`-access node, so non-roster peers can fetch the vCard
//!    immediately.
//! 2. A node that pre-exists with the wrong (`presence`) access model
//!    MUST be reconciled in-place on the owner's next publish — this
//!    covers users who first published before XEP-0292 §6.1 was wired
//!    through `NodeConfig::pep_for_node`.
//! 3. A second user with no roster relationship to the publisher MUST
//!    be able to fetch the items via `<items>` on the owner's PEP
//!    service.
//!
//! These tests sit alongside `xep0060_pubsub_ws.rs` and exercise the
//! same WebSocket binding the wasm chat client uses.
//!
//! Publish/retrieve integration coverage beyond the access-model
//! tests:
//! 4. Republish to the single-slot node replaces the `current` item —
//!    a peer fetch returns exactly one item carrying the latest
//!    payload (max_items=1 per `NodeConfig::vcard4_defaults`).
//! 5. Fetching the vCard of a user who never published returns an IQ
//!    error, not an empty success (the node does not exist).
//! 6. XEP-0163 §3 fan-out: a roster contact whose cached caps carry
//!    `urn:xmpp:vcard4+notify` receives the vCard4 PEP event on
//!    publish without any explicit pubsub subscription.

use waddle_ws_test_support as ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const NS_VCARD4: &str = "urn:ietf:params:xml:ns:vcard-4.0";
const PEP_NODE_VCARD4: &str = "urn:xmpp:vcard4";

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, resource)
        .await
        .expect("admin connect")
}

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

/// XEP-0292 §3 spec-shape minimal vCard4 publish body — the same
/// shape the chat editor (`waddle-xmpp-client::xep::xep0292`) builds
/// via `build_publish_vcard4_iq`, with no `<publish-options>` (which
/// is what forces the server to apply auto-create defaults).
fn vcard4_publish_body(full_name: &str, note: &str) -> String {
    format!(
        r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{PEP_NODE_VCARD4}"><item id="current"><vcard xmlns="{NS_VCARD4}"><fn><text>{full_name}</text></fn><note><text>{note}</text></note></vcard></item></publish></pubsub>"#
    )
}

/// Build a vCard4 fetch IQ targeting the owner's PEP `urn:xmpp:vcard4`
/// node — the canonical XEP-0292 §4 fetch shape.
fn vcard4_items_body() -> String {
    format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{PEP_NODE_VCARD4}"/></pubsub>"#)
}

#[tokio::test]
async fn xep0292_first_publish_auto_creates_open_access_node() {
    // §6.1 canonical default: a fresh publish to `urn:xmpp:vcard4`
    // MUST land an `open`-access node so any peer can read the vCard
    // via PEP fetch. This is the path the wasm chat editor takes:
    // a plain `<publish>` with no `<publish-options>` precondition.
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0292-create-1").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");

    let pub_resp = iq_set_to(
        &mut admin,
        "vc-pub-1",
        &admin_bare,
        &vcard4_publish_body("Admin Adminson", "Bio line"),
    )
    .await;
    assert!(
        pub_resp.contains(r#"type='result'"#),
        "auto-create + publish to vcard4 should succeed: {pub_resp}"
    );

    // Read the node config back from the owner's PEP service — the
    // §6.1 contract is `pubsub#access_model=open`.
    let cfg_resp = iq_get_to(
        &mut admin,
        "vc-cfg-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{PEP_NODE_VCARD4}"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        cfg_resp.contains(r#"var='pubsub#access_model'"#)
            || cfg_resp.contains(r#"var='pubsub#access_model'"#),
        "configure response must surface access_model field: {cfg_resp}"
    );
    assert!(
        cfg_resp.contains("<value>open</value>") || cfg_resp.contains("<value>open</value>"),
        "vcard4 PEP node MUST default to access_model=open per XEP-0292 §6.1: {cfg_resp}"
    );

    let _ = admin.close().await;
}

#[tokio::test]
async fn xep0292_non_roster_peer_can_fetch_published_vcard4() {
    // The user-visible bug this PR fixes: user A publishes vcard4
    // via the chat editor; user B (no roster relationship) opens
    // A's profile and gets back fields. Requires §6.1 open access
    // on the auto-created node.
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let mut admin = admin_client(&server, "xep0292-fetch-admin").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");

    // Admin publishes their vcard4 — auto-create path.
    let pub_resp = iq_set_to(
        &mut admin,
        "vc-fetch-pub",
        &admin_bare,
        &vcard4_publish_body("Admin Adminson", "Star-crossed sysop"),
    )
    .await;
    assert!(
        pub_resp.contains(r#"type='result'"#),
        "admin vcard4 publish: {pub_resp}"
    );

    // Bob — no roster relationship — fetches admin's vcard4. With
    // §6.1 open access this MUST succeed.
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "xep0292-fetch-bob",
    )
    .await
    .expect("bob connect");

    let items_resp = iq_get_to(
        &mut bob,
        "vc-fetch-items",
        &admin_bare,
        &vcard4_items_body(),
    )
    .await;
    assert!(
        items_resp.contains(r#"type='result'"#),
        "cross-user vcard4 fetch MUST succeed on an open node (XEP-0292 §6.1): {items_resp}"
    );
    assert!(
        items_resp.contains("Admin Adminson"),
        "fetched item MUST carry the published <fn> text: {items_resp}"
    );
    assert!(
        items_resp.contains("Star-crossed sysop"),
        "fetched item MUST carry the published <note> text: {items_resp}"
    );

    let _ = bob.close().await;
    let _ = admin.close().await;
}

/// Count `<item ` occurrences — not `<items`.
fn count_item_elements(xml: &str) -> usize {
    xml.match_indices("<item ").count()
}

#[tokio::test]
async fn xep0292_republish_replaces_current_item_in_single_slot_node() {
    // `NodeConfig::vcard4_defaults` pins max_items=1 so the vCard4
    // node is single-slot: each publish of `id='current'` replaces
    // the previous item. A peer fetch after two publishes MUST return
    // exactly one item carrying the second payload — two items (or
    // the stale payload) would mean the profile editor can never
    // update a vCard.
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0292-replace-1").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");

    let first = iq_set_to(
        &mut admin,
        "vc-rep-1",
        &admin_bare,
        &vcard4_publish_body("First Draft", "v1"),
    )
    .await;
    assert!(first.contains(r#"type='result'"#), "first publish: {first}");

    let second = iq_set_to(
        &mut admin,
        "vc-rep-2",
        &admin_bare,
        &vcard4_publish_body("Final Version", "v2"),
    )
    .await;
    assert!(
        second.contains(r#"type='result'"#),
        "second publish: {second}"
    );

    let items_resp = iq_get_to(
        &mut admin,
        "vc-rep-items",
        &admin_bare,
        &vcard4_items_body(),
    )
    .await;
    assert!(
        items_resp.contains(r#"type='result'"#),
        "fetch after republish: {items_resp}"
    );
    assert!(
        items_resp.contains("Final Version"),
        "fetch MUST return the replacement payload: {items_resp}"
    );
    assert!(
        !items_resp.contains("First Draft"),
        "the replaced item MUST NOT survive on a max_items=1 node: {items_resp}"
    );
    assert_eq!(
        count_item_elements(&items_resp),
        1,
        "exactly one <item/> on the single-slot vcard4 node: {items_resp}"
    );

    let _ = admin.close().await;
}

#[tokio::test]
async fn xep0292_fetch_from_user_without_vcard_returns_iq_error() {
    // Fetching `urn:xmpp:vcard4` items from an account that never
    // published MUST come back as an IQ error (the node does not
    // exist) — not as an empty `<items/>` success that a client
    // would cache as "this user has an empty profile".
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let mut admin = admin_client(&server, "xep0292-novcard-admin").await;

    // Admin queries bob's (never-published) vCard4 node.
    let bob_bare = format!("bob@{DOMAIN}");
    let resp = iq_get_to(&mut admin, "vc-none-1", &bob_bare, &vcard4_items_body()).await;
    assert!(
        resp.contains(r#"type='error'"#),
        "items fetch on a nonexistent vcard4 node MUST be an IQ error: {resp}"
    );
    assert!(
        resp.contains("item-not-found"),
        "the error SHOULD be item-not-found per XEP-0060 §6.5.9.11: {resp}"
    );

    let _ = admin.close().await;
}

const NS_CAPS: &str = "http://jabber.org/protocol/caps";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";

fn caps_verification_string(features: &[&str]) -> String {
    use waddle_xmpp::disco::info::{Feature, Identity};
    use waddle_xmpp::xep::xep0115::compute_caps_hash;
    let identities = vec![Identity::new("client", "pc", Some("Bob's Client"))];
    let features: Vec<Feature> = features.iter().map(|f| Feature::new(f)).collect();
    compute_caps_hash(&identities, &features)
}

/// Send a ping IQ and wait for its result — a deterministic FIFO
/// anchor confirming the server processed everything sent before it.
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

#[tokio::test]
async fn xep0292_publish_fans_out_to_roster_contact_with_vcard4_notify_caps() {
    // XEP-0163 §3 + XEP-0292: a contact with a presence subscription
    // to the publisher whose cached entity caps advertise
    // `urn:xmpp:vcard4+notify` MUST receive the vCard4 event message
    // when the publisher updates their profile — this is how clients
    // keep displayed names/avatars live without polling.
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
        "vc-fanout-alice",
    )
    .await
    .expect("alice connect");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "vc-fanout-bob",
    )
    .await
    .expect("bob connect");
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
    let bob_full = bob.full_jid.clone().expect("bob full jid");

    // bob → alice presence subscription so alice's roster carries bob
    // with subscription=from (the §3 fan-out filter). Both sessions
    // fetch their roster first so they count as interested resources
    // and receive the subscription pushes (RFC 6121 §2.2).
    alice
        .send(r#"<iq xmlns="jabber:client" type="get" id="vc-roster-a"><query xmlns="jabber:iq:roster"/></iq>"#)
        .await
        .expect("alice roster get");
    let _ = alice
        .recv_matching(|f| f.contains("vc-roster-a"))
        .await
        .expect("alice roster result");
    bob.send(r#"<iq xmlns="jabber:client" type="get" id="vc-roster-b"><query xmlns="jabber:iq:roster"/></iq>"#)
        .await
        .expect("bob roster get");
    let _ = bob
        .recv_matching(|f| f.contains("vc-roster-b"))
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
    let _ = alice
        .recv_matching(|f| f.contains(r#"type='subscribe'"#))
        .await
        .expect("alice receives subscribe");
    alice
        .send(&format!(
            r#"<presence xmlns="jabber:client" type="subscribed" to="{bob_bare}"/>"#
        ))
        .await
        .expect("alice approves");
    let _ = bob
        .recv_matching(|f| f.contains(r#"type='subscribed'"#))
        .await
        .expect("bob receives approval");

    // bob advertises caps carrying `urn:xmpp:vcard4+notify` and
    // completes the XEP-0115 verification round-trip.
    let notify_var = format!("{PEP_NODE_VCARD4}+notify");
    let features = ["http://jabber.org/protocol/disco#info", notify_var.as_str()];
    let ver = caps_verification_string(&features);
    bob.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="https://bob.example/caps" ver="{ver}"/></presence>"#
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
        .expect("server resolves bob's caps");
    let iq_id = extract_attr_after(&disco_query, "<iq", "id").expect("iq id");
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="https://bob.example/caps#{ver}"><identity category="client" type="pc" name="Bob's Client"/>{feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob disco#info reply");
    ping_anchor(&mut bob, "vc-fanout-anchor").await;

    // Alice publishes her vCard4.
    let pub_resp = iq_set_to(
        &mut alice,
        "vc-fanout-pub",
        &alice_bare,
        &vcard4_publish_body("Alice Liddell", "Through the looking glass"),
    )
    .await;
    assert!(
        pub_resp.contains(r#"type='result'"#),
        "alice vcard4 publish: {pub_resp}"
    );

    // Bob receives the PEP event with the vCard payload, from alice's
    // bare JID (XEP-0163 §4.3).
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let event = loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "bob (roster from + vcard4+notify caps) MUST receive the vCard4 PEP event"
        );
        match bob.recv_timeout(remaining).await {
            Ok(frame)
                if frame.contains("<message")
                    && frame.contains(NS_PUBSUB_EVENT)
                    && frame.contains(PEP_NODE_VCARD4) =>
            {
                break frame;
            }
            Ok(_) => continue,
            Err(e) => panic!("waiting for vcard4 event: {e}"),
        }
    };
    assert!(
        event.contains(&format!(r#"from='{alice_bare}'"#)),
        "event from MUST be alice's bare JID per XEP-0163 §4.3: {event}"
    );
    assert!(
        event.contains("Alice Liddell"),
        "event MUST carry the published <fn> payload: {event}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn xep0292_publish_reconciles_legacy_presence_access_node() {
    // Migration case: an earlier Waddle version auto-created the
    // vcard4 PEP node with `presence` access (the bare
    // `pep_default()`). After §6.1 was wired through
    // `pep_for_node` the canonical access model is `open`, but
    // the already-created node sticks with the old config until
    // something reconfigures it. The publish dispatcher reconciles
    // the config in-place on the next owner publish so the next
    // peer fetch sees the spec-conformant node.
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let mut admin = admin_client(&server, "xep0292-migrate-admin").await;
    let admin_bare = format!("{USERNAME}@{DOMAIN}");

    // Step 1: seed a vcard4 node, then EXPLICITLY downgrade its
    // access to `presence` — simulating what a pre-fix Waddle would
    // have done on first publish.
    let pub_resp = iq_set_to(
        &mut admin,
        "vc-mig-seed",
        &admin_bare,
        &vcard4_publish_body("Pre Migration", "old"),
    )
    .await;
    assert!(
        pub_resp.contains(r#"type='result'"#),
        "seed publish: {pub_resp}"
    );

    let downgrade = iq_set_to(
        &mut admin,
        "vc-mig-downgrade",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{PEP_NODE_VCARD4}"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#access_model"><value>presence</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(
        downgrade.contains(r#"type='result'"#),
        "downgrade to presence: {downgrade}"
    );

    // Confirm bob (no roster) currently CANNOT fetch — the
    // `presence` access model gates this.
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "xep0292-migrate-bob",
    )
    .await
    .expect("bob connect");

    let denied_resp = iq_get_to(&mut bob, "vc-mig-denied", &admin_bare, &vcard4_items_body()).await;
    assert!(
        denied_resp.contains(r#"type='error'"#),
        "pre-migration fetch from non-roster peer MUST be denied on presence access: {denied_resp}"
    );

    // Step 2: admin republishes. This is the reconcile point —
    // the publish handler MUST notice the well-known node config
    // diverges from the §6.1 canonical and rewrite it to open
    // before the new item lands.
    let republish = iq_set_to(
        &mut admin,
        "vc-mig-republish",
        &admin_bare,
        &vcard4_publish_body("Post Migration", "new"),
    )
    .await;
    assert!(
        republish.contains(r#"type='result'"#),
        "republish should succeed and trigger reconcile: {republish}"
    );

    // Give the server a beat to process the reconcile + publish.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Step 3: now bob's fetch MUST succeed and surface the new
    // payload — the reconcile flipped the node to open.
    let ok_resp = iq_get_to(&mut bob, "vc-mig-ok", &admin_bare, &vcard4_items_body()).await;
    assert!(
        ok_resp.contains(r#"type='result'"#),
        "post-reconcile fetch MUST succeed (open access per §6.1): {ok_resp}"
    );
    assert!(
        ok_resp.contains("Post Migration"),
        "fetched item MUST be the post-migration payload: {ok_resp}"
    );

    // And the owner-readable config now reports `open`.
    let cfg_resp = iq_get_to(
        &mut admin,
        "vc-mig-cfg",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{PEP_NODE_VCARD4}"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        cfg_resp.contains("<value>open</value>"),
        "reconcile MUST set access_model=open on the existing node: {cfg_resp}"
    );

    let _ = bob.close().await;
    let _ = admin.close().await;
}
