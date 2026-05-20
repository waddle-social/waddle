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

mod ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

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
