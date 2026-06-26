//! Integration tests for the private `urn:waddle:status-preference:0`
//! PEP node (ADR-010 Phase 4 — cross-device manual-status sync) over
//! WebSocket C2S.
//!
//! The node mirrors the `urn:waddle:dnd:0` server conventions: owner-only
//! publish, `access_model=whitelist`, single item id `current`. Unlike
//! DND there is no server-side projection — the payload is pure
//! owner↔own-devices sync. Covered here:
//!
//! - Publish round-trips; the owner reads it back via `<items/>` get
//!   (item id `current`, payload preserved).
//! - Item id MUST be `current` (`<bad-request/>` + `<invalid-payload/>`).
//! - A malformed `<status-preference>` payload is rejected
//!   (`<bad-request/>` + `<invalid-payload/>`).
//! - A non-owner publish to the node is rejected (`<error/>`).
//! - A non-owner fetch is blocked by the whitelist access model.
//! - The private-PEP carve-out suppresses roster fan-out.
//! - The owner's OTHER resource (advertising `…+notify` caps) DOES
//!   receive the headline event — the live cross-device sync path.

mod ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const NODE: &str = "urn:waddle:status-preference:0";
const NS: &str = "urn:waddle:status-preference:0";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_CAPS: &str = "http://jabber.org/protocol/caps";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NS_PUBSUB_ERRORS: &str = "http://jabber.org/protocol/pubsub#errors";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn connect_account(server: &TestServer, user: &str, password: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        user,
        password,
        &format!("{user}-{}", uuid::Uuid::new_v4()),
    )
    .await
    .unwrap_or_else(|err| panic!("{user} connect: {err}"))
}

async fn connect_admin() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let client = connect_account(&server, "admin", &password).await;
    (server, client)
}

async fn connect_two_accounts() -> (TestServer, WsXmppClient, WsXmppClient) {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let alice = connect_account(&server, "alice", &alice_password).await;
    let bob = connect_account(&server, "bob", &bob_password).await;
    (server, alice, bob)
}

/// Build the `<status-preference>` payload XML for a mode.
fn pref_body(mode: &str, status: Option<&str>) -> String {
    match status {
        Some(status) => {
            format!(r#"<status-preference xmlns="{NS}" mode="{mode}" status="{status}"/>"#)
        }
        None => format!(r#"<status-preference xmlns="{NS}" mode="{mode}"/>"#),
    }
}

/// Self-publish IQ (no `to`, addressed to the account's own PEP service).
fn publish_iq(id: &str, item_id: &str, body_xml: &str) -> String {
    format!(
        r#"<iq type="set" id="{id}">
          <pubsub xmlns="{NS_PUBSUB}">
            <publish node="{NODE}">
              <item id="{item_id}">{body_xml}</item>
            </publish>
          </pubsub>
        </iq>"#
    )
}

fn items_get_iq(id: &str, to: Option<&str>) -> String {
    let to_attr = to.map(|jid| format!(r#" to="{jid}""#)).unwrap_or_default();
    format!(
        r#"<iq type="get" id="{id}"{to_attr}>
          <pubsub xmlns="{NS_PUBSUB}">
            <items node="{NODE}" max_items="1"/>
          </pubsub>
        </iq>"#
    )
}

fn assert_invalid_payload(xml: &str) {
    assert!(xml.contains(r#"type='error'"#), "expected error iq: {xml}");
    assert!(
        xml.contains("bad-request") || xml.contains("not-acceptable"),
        "expected <bad-request/> stanza error: {xml}"
    );
    assert!(
        xml.contains("invalid-payload"),
        "expected XEP-0060 <invalid-payload/> ({NS_PUBSUB_ERRORS}) extension: {xml}"
    );
}

#[tokio::test]
async fn publish_then_owner_fetch_round_trips() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = connect_admin().await;

    client
        .send(&publish_iq(
            "pub-1",
            "current",
            &pref_body("manual", Some("away")),
        ))
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

    client
        .send(&items_get_iq("fetch-1", None))
        .await
        .expect("send fetch");
    let fetch_result = client
        .recv_matching(|frame| frame.contains(r#"id='fetch-1'"#))
        .await
        .expect("fetch result");
    assert!(
        fetch_result.contains(r#"id='current'"#),
        "fetched item id must be 'current': {fetch_result}"
    );
    assert!(
        fetch_result.contains(r#"mode="manual""#) || fetch_result.contains(r#"mode='manual'"#),
        "fetched payload missing mode=manual: {fetch_result}"
    );
    assert!(
        fetch_result.contains(r#"status="away""#) || fetch_result.contains(r#"status='away'"#),
        "fetched payload missing status=away: {fetch_result}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn republish_overwrites_item() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = connect_admin().await;

    client
        .send(&publish_iq(
            "pub-auto",
            "current",
            &pref_body("automatic", None),
        ))
        .await
        .expect("send first publish");
    let _ = client
        .recv_matching(|frame| frame.contains(r#"id='pub-auto'"#))
        .await
        .expect("first publish result");

    client
        .send(&publish_iq(
            "pub-manual",
            "current",
            &pref_body("manual", Some("dnd")),
        ))
        .await
        .expect("send second publish");
    let _ = client
        .recv_matching(|frame| frame.contains(r#"id='pub-manual'"#))
        .await
        .expect("second publish result");

    client
        .send(&items_get_iq("fetch-after", None))
        .await
        .expect("send fetch");
    let fetch_result = client
        .recv_matching(|frame| frame.contains(r#"id='fetch-after'"#))
        .await
        .expect("fetch result");
    assert!(
        fetch_result.contains("dnd"),
        "republish failed to surface latest mode: {fetch_result}"
    );
    assert!(
        !fetch_result.contains("automatic"),
        "max_items=1 should have evicted the prior item: {fetch_result}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn publish_with_wrong_item_id_rejected() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = connect_admin().await;

    client
        .send(&publish_iq(
            "pub-wrong-id",
            "not-current",
            &pref_body("manual", Some("away")),
        ))
        .await
        .expect("send publish");
    let result = client
        .recv_matching(|frame| frame.contains(r#"id='pub-wrong-id'"#))
        .await
        .expect("publish result");
    assert_invalid_payload(&result);

    let _ = client.close().await;
}

#[tokio::test]
async fn publish_malformed_payload_rejected() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = connect_admin().await;

    // Unknown mode value — rejected by the strict parser at the publish
    // boundary, never persisted.
    client
        .send(&publish_iq(
            "pub-bad",
            "current",
            &pref_body("invisible", None),
        ))
        .await
        .expect("send publish");
    let result = client
        .recv_matching(|frame| frame.contains(r#"id='pub-bad'"#))
        .await
        .expect("publish result");
    assert_invalid_payload(&result);

    let _ = client.close().await;
}

#[tokio::test]
async fn non_owner_publish_is_rejected() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;

    // Bob addresses alice's PEP service directly and tries to write her
    // status preference. The node owner is alice; the publisher is bob,
    // so the publish MUST be refused.
    bob.send(&format!(
        r#"<iq type="set" id="bob-spoof" to="alice@{DOMAIN}">
          <pubsub xmlns="{NS_PUBSUB}">
            <publish node="{NODE}">
              <item id="current">{}</item>
            </publish>
          </pubsub>
        </iq>"#,
        pref_body("manual", Some("dnd"))
    ))
    .await
    .expect("bob spoof publish");
    let result = bob
        .recv_matching(|frame| frame.contains(r#"id='bob-spoof'"#))
        .await
        .expect("bob spoof result");
    assert!(
        result.contains(r#"type='error'"#),
        "non-owner publish must be refused: {result}"
    );

    // And alice's node holds nothing bob wrote: a fetch yields no item.
    alice
        .send(&items_get_iq("alice-check", None))
        .await
        .expect("alice fetch");
    let alice_result = alice
        .recv_matching(|frame| frame.contains(r#"id='alice-check'"#))
        .await
        .expect("alice fetch result");
    assert!(
        !alice_result.contains("dnd"),
        "bob's spoofed status must not appear in alice's node: {alice_result}"
    );

    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn node_is_private_to_non_owner() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;

    alice
        .send(&publish_iq(
            "alice-pub",
            "current",
            &pref_body("manual", Some("away")),
        ))
        .await
        .expect("alice publish");
    let _ = alice
        .recv_matching(|frame| frame.contains(r#"id='alice-pub'"#))
        .await
        .expect("alice publish result");

    // Bob tries to fetch alice's private preference node. The whitelist
    // access_model (from the well-known node defaults) MUST reject this.
    bob.send(&items_get_iq("bob-snoop", Some(&format!("alice@{DOMAIN}"))))
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

    alice.send("<presence/>").await.expect("alice presence");
    bob.send("<presence/>").await.expect("bob presence");

    // Bob best-effort subscribes to alice's node (whitelist should
    // refuse, but the fan-out carve-out is the real guard under test).
    bob.send(&format!(
        r#"<iq type="set" id="bob-sub" to="alice@{DOMAIN}">
          <pubsub xmlns="{NS_PUBSUB}">
            <subscribe node="{NODE}" jid="bob@{DOMAIN}"/>
          </pubsub>
        </iq>"#
    ))
    .await
    .expect("bob subscribe");
    let _ = bob
        .recv_matching(|f| f.contains(r#"id='bob-sub'"#))
        .await
        .expect("bob subscribe response");

    alice
        .send(&publish_iq(
            "alice-pub",
            "current",
            &pref_body("manual", Some("dnd")),
        ))
        .await
        .expect("alice publish");
    let _ = alice
        .recv_matching(|f| f.contains(r#"id='alice-pub'"#))
        .await
        .expect("alice publish result");

    // Bob's stream MUST stay silent for the preference node.
    let mut leaked: Option<String> = None;
    for _ in 0..3 {
        match bob.recv_timeout(Duration::from_millis(250)).await {
            Ok(frame) => {
                if frame.contains(NODE) {
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

fn caps_verification_string(features: &[&str]) -> String {
    use waddle_xmpp::disco::info::{Feature, Identity};
    use waddle_xmpp::xep::xep0115::compute_caps_hash;
    let identities = vec![Identity::new("client", "pc", Some("Waddle"))];
    let features: Vec<Feature> = features.iter().map(|f| Feature::new(f)).collect();
    compute_caps_hash(&identities, &features)
}

/// Bring a resource "online" advertising `urn:waddle:status-preference:0+notify`
/// in its XEP-0115 caps, completing the server's disco round-trip so the
/// §3.4 owner-self fan-out will deliver headline events to it.
async fn announce_status_preference_notify_caps(client: &mut WsXmppClient) {
    let notify_var = format!("{NODE}+notify");
    let features = [NS_DISCO_INFO, notify_var.as_str()];
    let caps_node = "https://waddle.example/caps";
    let ver = caps_verification_string(&features);
    let full = client.full_jid.clone().expect("full jid");

    client
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("presence with caps");
    let disco_query = client
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries caps");
    let iq_id = extract_attr_after(&disco_query, "<iq", "id").expect("disco iq id");
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Waddle"/>{feature_xml}</query></iq>"#
        ))
        .await
        .expect("disco#info reply");
}

#[tokio::test]
async fn self_fanout_reaches_owner_other_resource() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);

    // Two resources of the SAME account.
    let mut r1 = connect_account(&server, "alice", &alice_password).await;
    let mut r2 = connect_account(&server, "alice", &alice_password).await;

    // r1 is online (the publisher); r2 advertises +notify caps so the
    // §3.4 owner-self pass delivers the headline to it.
    r1.send("<presence/>").await.expect("r1 presence");
    announce_status_preference_notify_caps(&mut r2).await;

    // r1 publishes a pick. r2 — alice's other resource — MUST receive
    // the headline event (this is the live cross-device sync path).
    r1.send(&publish_iq(
        "r1-pub",
        "current",
        &pref_body("manual", Some("away")),
    ))
    .await
    .expect("r1 publish");
    let _ = r1
        .recv_matching(|f| f.contains(r#"id='r1-pub'"#))
        .await
        .expect("r1 publish result");

    let event = r2
        .recv_matching(|frame| frame.contains("<message") && frame.contains(NODE))
        .await
        .expect("alice's other resource MUST receive the status-preference headline via §3.4 self fan-out");
    assert!(
        event.contains(r#"mode="away""#)
            || event.contains(r#"status="away""#)
            || event.contains("away"),
        "headline must carry the published payload: {event}"
    );

    let _ = r1.close().await;
    let _ = r2.close().await;
}
