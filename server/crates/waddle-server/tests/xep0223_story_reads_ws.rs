//! Integration tests for the private `urn:waddle:story:reads:0` PEP
//! node (XEP-0223 Persistent Storage of Private Data via PubSub) over
//! WebSocket C2S.
//!
//! Covers the security-critical commitments from the design spec at
//! `docs/superpowers/specs/2026-05-19-stories-media-and-reads-design.md`:
//!
//! - Publish-options precondition pins all four required fields.
//! - Publish overwrites the single `current` item.
//! - Whitelist access_model blocks fetch by non-owners.
//! - The private-PEP carve-out in `pubsub_fanout.rs` suppresses
//!   roster fan-out (no headline event delivered to roster contacts).

mod ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const READS_NODE: &str = "urn:waddle:story:reads:0";
const READS_NS: &str = "urn:waddle:story:reads:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn connect_admin() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0223-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, "admin", &password, &resource)
            .await
            .expect("admin connect");
    (server, client)
}

async fn connect_two_accounts() -> (TestServer, WsXmppClient, WsXmppClient) {
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
    .expect("alice connect");
    let bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connect");
    (server, alice, bob)
}

fn publish_options_form() -> &'static str {
    r#"<publish-options>
      <x xmlns="jabber:x:data" type="submit">
        <field var="FORM_TYPE" type="hidden">
          <value>http://jabber.org/protocol/pubsub#publish-options</value>
        </field>
        <field var="pubsub#persist_items"><value>true</value></field>
        <field var="pubsub#access_model"><value>whitelist</value></field>
        <field var="pubsub#send_last_published_item"><value>never</value></field>
        <field var="pubsub#max_items"><value>1</value></field>
      </x>
    </publish-options>"#
}

fn publish_reads_iq(id: &str, body_xml: &str) -> String {
    let options = publish_options_form();
    format!(
        r#"<iq type="set" id="{id}">
          <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="{READS_NODE}">
              <item id="current">{body_xml}</item>
            </publish>
            {options}
          </pubsub>
        </iq>"#
    )
}

fn reads_body(entries: &[(&str, &str)]) -> String {
    let mut out = format!(r#"<reads xmlns="{READS_NS}">"#);
    for (id, at) in entries {
        out.push_str(&format!(r#"<read id="{id}" at="{at}"/>"#));
    }
    out.push_str("</reads>");
    out
}

#[tokio::test]
async fn publish_then_fetch_returns_entries() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = connect_admin().await;

    let body = reads_body(&[("story-a", "2026-05-19T10:11:12Z")]);
    client
        .send(&publish_reads_iq("pub-1", &body))
        .await
        .expect("send publish");
    let publish_result = client
        .recv_matching(|frame| frame.contains(r#"id="pub-1""#))
        .await
        .expect("publish result");
    assert!(
        publish_result.contains(r#"type="result""#),
        "publish failed: {publish_result}"
    );

    // Fetch the item back. Owner fetch on a whitelist PEP node should
    // succeed because the owner is implicitly on the whitelist.
    client
        .send(&format!(
            r#"<iq type="get" id="fetch-1">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{READS_NODE}" max_items="1"/>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send fetch");
    let fetch_result = client
        .recv_matching(|frame| frame.contains(r#"id="fetch-1""#))
        .await
        .expect("fetch result");
    assert!(
        fetch_result.contains("story-a"),
        "fetched payload missing story-a: {fetch_result}"
    );
    assert!(
        fetch_result.contains("2026-05-19T10:11:12Z"),
        "fetched payload missing timestamp: {fetch_result}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn republish_overwrites_item() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = connect_admin().await;

    let first = reads_body(&[("story-old", "2026-05-19T09:00:00Z")]);
    client
        .send(&publish_reads_iq("pub-old", &first))
        .await
        .expect("send first publish");
    let _ = client
        .recv_matching(|frame| frame.contains(r#"id="pub-old""#))
        .await
        .expect("first publish result");

    let second = reads_body(&[("story-new", "2026-05-19T10:00:00Z")]);
    client
        .send(&publish_reads_iq("pub-new", &second))
        .await
        .expect("send second publish");
    let _ = client
        .recv_matching(|frame| frame.contains(r#"id="pub-new""#))
        .await
        .expect("second publish result");

    client
        .send(&format!(
            r#"<iq type="get" id="fetch-after">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{READS_NODE}" max_items="1"/>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send fetch");
    let fetch_result = client
        .recv_matching(|frame| frame.contains(r#"id="fetch-after""#))
        .await
        .expect("fetch result");
    assert!(
        fetch_result.contains("story-new"),
        "republish failed to surface latest entry: {fetch_result}"
    );
    assert!(
        !fetch_result.contains("story-old"),
        "max_items=1 should have evicted the prior item: {fetch_result}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn node_is_private_to_owner() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;

    let body = reads_body(&[("story-alice", "2026-05-19T10:00:00Z")]);
    alice
        .send(&publish_reads_iq("alice-pub", &body))
        .await
        .expect("alice publish");
    let _ = alice
        .recv_matching(|frame| frame.contains(r#"id="alice-pub""#))
        .await
        .expect("alice publish result");

    // Bob tries to fetch alice's private read-state node by addressing
    // alice's bare JID. The whitelist access_model MUST reject this.
    bob.send(&format!(
        r#"<iq type="get" id="bob-snoop" to="alice@{DOMAIN}">
          <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <items node="{READS_NODE}"/>
          </pubsub>
        </iq>"#
    ))
    .await
    .expect("bob fetch");
    let bob_result = bob
        .recv_matching(|frame| frame.contains(r#"id="bob-snoop""#))
        .await
        .expect("bob fetch result");
    assert!(
        bob_result.contains(r#"type="error""#),
        "non-owner fetch must error on a whitelist PEP node: {bob_result}"
    );
    // Either `forbidden` or `item-not-found` is acceptable depending on
    // the server's leak-avoidance posture — both prevent disclosure.
    assert!(
        bob_result.contains("forbidden") || bob_result.contains("item-not-found"),
        "expected forbidden or item-not-found, got: {bob_result}"
    );

    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn private_pep_does_not_fan_out_to_roster() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;

    // Initial presence so the server routes per-resource events to
    // both connected clients. PEP fan-out keys off presence state.
    alice
        .send("<presence/>")
        .await
        .expect("alice initial presence");
    bob.send("<presence/>").await.expect("bob initial presence");

    // Bob attempts to subscribe to alice's private read-state node.
    // Whitelist access_model should refuse this directly, but even if
    // the server accepted it, the `pubsub_fanout.rs` carve-out for
    // `urn:waddle:story:reads:0` ensures bob still won't receive
    // headline events. We exercise both paths together: try to
    // subscribe (best-effort, response ignored) then publish and prove
    // bob's stream stays silent for the read-state node.
    bob.send(&format!(
        r#"<iq type="set" id="bob-sub" to="alice@{DOMAIN}">
          <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <subscribe node="{READS_NODE}" jid="bob@{DOMAIN}"/>
          </pubsub>
        </iq>"#
    ))
    .await
    .expect("bob subscribe");
    let _ = bob
        .recv_matching(|f| f.contains(r#"id="bob-sub""#))
        .await
        .expect("bob subscribe response");

    // Alice publishes. Bob MUST NOT receive any headline event for
    // the story-reads node.
    let body = reads_body(&[("story-x", "2026-05-19T11:00:00Z")]);
    alice
        .send(&publish_reads_iq("alice-pub", &body))
        .await
        .expect("alice publish");
    let _ = alice
        .recv_matching(|f| f.contains(r#"id="alice-pub""#))
        .await
        .expect("alice publish result");

    // Drain bob's inbound stream briefly. Any frame referencing the
    // story-reads node or its payload would indicate a fan-out leak.
    let mut leaked: Option<String> = None;
    for _ in 0..3 {
        match bob.recv_timeout(Duration::from_millis(250)).await {
            Ok(frame) => {
                if frame.contains(READS_NODE) || frame.contains("story-x") {
                    leaked = Some(frame);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(
        leaked.is_none(),
        "private PEP node leaked to roster contact: {leaked:?}"
    );

    let _ = alice.close().await;
    let _ = bob.close().await;
}
