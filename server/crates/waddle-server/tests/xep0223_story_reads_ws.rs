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

use waddle_ws_test_support as ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const READS_NODE: &str = "urn:waddle:story:reads:0";
const READS_NS: &str = "urn:waddle:story:reads:0";
const NS_CAPS: &str = "http://jabber.org/protocol/caps";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

/// Compute the XEP-0115 ver hash for the advertised feature set.
/// Copied verbatim from `xep0163_pep_ws.rs`.
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

/// Send a ping IQ and wait for its result — a deterministic FIFO
/// anchor proving all prior frames on this connection were processed.
/// Copied verbatim from `xep0163_pep_ws.rs`.
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

/// Establish bob -> alice presence subscription so alice's roster has
/// bob with `subscription = from` (the XEP-0163 §3 fan-out target).
/// Adapted from `xep0163_pep_ws.rs`.
async fn establish_bob_subscribes_to_alice(alice: &mut WsXmppClient, bob: &mut WsXmppClient) {
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");
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
        .recv_matching(|frame| frame.contains(r#"id='pub-1'"#))
        .await
        .expect("publish result");
    assert!(
        publish_result.contains(r#"type='result'"#),
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
        .recv_matching(|frame| frame.contains(r#"id='fetch-1'"#))
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
        .recv_matching(|frame| frame.contains(r#"id='pub-old'"#))
        .await
        .expect("first publish result");

    let second = reads_body(&[("story-new", "2026-05-19T10:00:00Z")]);
    client
        .send(&publish_reads_iq("pub-new", &second))
        .await
        .expect("send second publish");
    let _ = client
        .recv_matching(|frame| frame.contains(r#"id='pub-new'"#))
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
        .recv_matching(|frame| frame.contains(r#"id='fetch-after'"#))
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
        .recv_matching(|frame| frame.contains(r#"id='alice-pub'"#))
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
        .recv_matching(|frame| frame.contains(r#"id='bob-snoop'"#))
        .await
        .expect("bob fetch result");
    assert!(
        bob_result.contains(r#"type='error'"#),
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
async fn private_pep_does_not_fan_out_to_roster_contact_with_notify_caps() {
    // Issue #1094 regression guard for the access_model-derived fan-out
    // gate: story reads must be whitelist-configured at auto-create so
    // a roster contact (subscription=from) advertising
    // `urn:waddle:story:reads:0+notify` caps never receives the §3
    // roster fan-out. Unlike `private_pep_does_not_fan_out_to_roster`
    // below, this test puts bob on the REAL leak path (roster + caps),
    // which is presence-driven and needs no pubsub <subscribe/>.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;

    establish_bob_subscribes_to_alice(&mut alice, &mut bob).await;

    let notify_var = format!("{READS_NODE}+notify");
    let features = [NS_DISCO_INFO, notify_var.as_str()];
    let caps_node = "https://bob.example/story-reads-caps";
    let ver = caps_verification_string("client", "pc", "Bob's Client", &features);
    let bob_full = bob.full_jid.clone().expect("bob full jid");
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
        .expect("server caps disco to bob");
    let iq_id = ws_common::extract_attr_after(&disco_query, "<iq", "id").expect("iq id");
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Bob's Client"/>{feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob disco reply");
    ping_anchor(&mut bob, "story-reads-caps-anchor").await;

    let body = reads_body(&[("story-leak", "2026-07-03T10:00:00Z")]);
    alice
        .send(&publish_reads_iq("alice-caps-pub", &body))
        .await
        .expect("alice publish");
    let publish_result = alice
        .recv_matching(|f| f.contains(r#"id='alice-caps-pub'"#))
        .await
        .expect("alice publish result");
    assert!(
        publish_result.contains(r#"type='result'"#),
        "publish failed: {publish_result}"
    );

    // Poll the raw stream (no consuming anchor) for a leaked event.
    let mut leaked: Option<String> = None;
    let deadline = std::time::Instant::now() + Duration::from_millis(700);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match bob.recv_timeout(remaining).await {
            Ok(frame) => {
                if frame.contains(READS_NODE) || frame.contains("story-leak") {
                    leaked = Some(frame);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(
        leaked.is_none(),
        "story-reads leaked to roster contact with +notify caps: {leaked:?}"
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
        .recv_matching(|f| f.contains(r#"id='bob-sub'"#))
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
        .recv_matching(|f| f.contains(r#"id='alice-pub'"#))
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
