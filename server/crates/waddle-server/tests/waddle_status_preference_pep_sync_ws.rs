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
//!
//! Every stanza is assembled with `minidom::Element` builders and
//! serialized (never `format!`-interpolated XML markup), per the
//! CLAUDE.md XML-generation hard rule.

mod ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const NODE: &str = "urn:waddle:status-preference:0";
const NS: &str = "urn:waddle:status-preference:0";
const CLIENT_NS: &str = "jabber:client";
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

// ── Stanza builders ─────────────────────────────────────────────────
// All XML is built via `minidom::Element` and serialized; only attribute
// *values* (jids, the caps `node#ver`, the `+notify` feature var) are
// computed as plain strings, which minidom escapes on serialization.

fn attr(name: &str) -> minidom::rxml::NcName {
    // The builder API wants an owned NcName; this keeps call sites terse for
    // the (statically valid) attribute names used throughout the file.
    <minidom::rxml::NcName as std::convert::TryFrom<&str>>::try_from(name)
        .expect("static ncname is valid")
}

fn element_to_xml(element: Element) -> String {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize element");
    String::from_utf8(bytes).expect("serializer emits utf-8")
}

/// `<iq>` wrapper around a payload, with optional `to` / `from`.
fn iq(iq_type: &str, id: &str, to: Option<&str>, from: Option<&str>, payload: Element) -> String {
    let mut builder = Element::builder("iq", CLIENT_NS)
        .attr(attr("type"), iq_type)
        .attr(attr("id"), id);
    if let Some(to) = to {
        builder = builder.attr(attr("to"), to);
    }
    if let Some(from) = from {
        builder = builder.attr(attr("from"), from);
    }
    element_to_xml(builder.append(payload).build())
}

/// The `<status-preference>` payload for a mode.
fn status_preference_payload(mode: &str, status: Option<&str>) -> Element {
    let mut builder = Element::builder("status-preference", NS).attr(attr("mode"), mode);
    if let Some(status) = status {
        builder = builder.attr(attr("status"), status);
    }
    builder.build()
}

/// A `<pubsub><publish node=…><item id=…>[payload]</item>…` element.
fn publish_pubsub(item_id: &str, payload: Option<Element>) -> Element {
    let mut item = Element::builder("item", NS_PUBSUB).attr(attr("id"), item_id);
    if let Some(payload) = payload {
        item = item.append(payload);
    }
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr(attr("node"), NODE)
        .append(item)
        .build();
    Element::builder("pubsub", NS_PUBSUB)
        .append(publish)
        .build()
}

/// Self-publish IQ (no `to`, addressed to the account's own PEP service).
fn publish_iq(id: &str, item_id: &str, payload: Element) -> String {
    iq(
        "set",
        id,
        None,
        None,
        publish_pubsub(item_id, Some(payload)),
    )
}

fn items_get_iq(id: &str, to: Option<&str>) -> String {
    let items = Element::builder("items", NS_PUBSUB)
        .attr(attr("node"), NODE)
        .attr(attr("max_items"), "1")
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    iq("get", id, to, None, pubsub)
}

fn bare_presence() -> String {
    element_to_xml(Element::builder("presence", CLIENT_NS).build())
}

/// `<presence type=… to=…/>` (subscription-management presence).
fn presence_typed(presence_type: &str, to: &str) -> String {
    element_to_xml(
        Element::builder("presence", CLIENT_NS)
            .attr(attr("type"), presence_type)
            .attr(attr("to"), to)
            .build(),
    )
}

/// `<iq type='get'><query xmlns='jabber:iq:roster'/></iq>` — priming the
/// roster establishes roster interest so the server later pushes subscription
/// state changes to the client.
fn roster_get_iq(id: &str) -> String {
    let query = Element::builder("query", "jabber:iq:roster").build();
    iq("get", id, None, None, query)
}

/// Establish bob → alice presence subscription so alice's roster carries bob
/// with `subscription = from`. That makes bob a target of alice's XEP-0163 §3
/// roster+CAPS PEP fan-out — so a non-private node WOULD deliver the headline to
/// him. The private-node carve-out is what must suppress it. Mirrors
/// `establish_bob_subscribes_to_alice` in `xep0163_pep_ws.rs`.
async fn establish_roster_from(
    alice: &mut WsXmppClient,
    bob: &mut WsXmppClient,
    alice_bare: &str,
    bob_bare: &str,
) {
    // Prime both rosters first (roster interest), or the subscription-state
    // pushes below are not delivered.
    alice
        .send(&roster_get_iq("roster-init-a"))
        .await
        .expect("alice roster get");
    let _ = alice
        .recv_matching(|f| f.contains("roster-init-a"))
        .await
        .expect("alice roster result");
    bob.send(&roster_get_iq("roster-init-b"))
        .await
        .expect("bob roster get");
    let _ = bob
        .recv_matching(|f| f.contains("roster-init-b"))
        .await
        .expect("bob roster result");

    alice.send(&bare_presence()).await.expect("alice presence");
    bob.send(&presence_typed("subscribe", alice_bare))
        .await
        .expect("bob subscribes to alice");
    let _ = alice
        .recv_matching(|f| f.contains(r#"type='subscribe'"#))
        .await
        .expect("alice receives subscribe request");
    alice
        .send(&presence_typed("subscribed", bob_bare))
        .await
        .expect("alice approves subscription");
    let _ = bob
        .recv_matching(|f| f.contains(r#"type='subscribed'"#))
        .await
        .expect("bob receives approval");
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
            status_preference_payload("manual", Some("away")),
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
            status_preference_payload("automatic", None),
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
            status_preference_payload("manual", Some("dnd")),
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
            status_preference_payload("manual", Some("away")),
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

    // A `<status-preference>` payload carrying a stray child element — the
    // strict parser rejects it (the full rejection matrix is in the parser's
    // unit suite; this proves the server publish path runs that parser).
    let stray_child = Element::builder("status-preference", NS)
        .attr(attr("mode"), "automatic")
        .append(Element::builder("x", NS).build())
        .build();
    let cases: Vec<(&str, Element)> = vec![
        ("pub-bad-mode", status_preference_payload("invisible", None)),
        (
            "pub-bad-status-on-auto",
            status_preference_payload("automatic", Some("away")),
        ),
        ("pub-bad-child", stray_child),
    ];
    for (id, payload) in cases {
        client
            .send(&publish_iq(id, "current", payload))
            .await
            .expect("send publish");
        let result = client
            .recv_matching(|frame| frame.contains(&format!("id='{id}'")))
            .await
            .expect("publish result");
        assert_invalid_payload(&result);
    }

    // A publish carrying no `<status-preference>` payload at all is refused.
    client
        .send(&iq(
            "set",
            "pub-empty",
            None,
            None,
            publish_pubsub("current", None),
        ))
        .await
        .expect("send empty publish");
    let empty = client
        .recv_matching(|frame| frame.contains(r#"id='pub-empty'"#))
        .await
        .expect("empty publish result");
    assert!(
        empty.contains(r#"type='error'"#),
        "missing payload must error: {empty}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn non_owner_publish_is_rejected() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut alice, mut bob) = connect_two_accounts().await;
    let alice_jid = format!("alice@{DOMAIN}");

    // Bob addresses alice's PEP service directly and tries to write her
    // status preference. The node owner is alice; the publisher is bob,
    // so the publish MUST be refused.
    bob.send(&iq(
        "set",
        "bob-spoof",
        Some(&alice_jid),
        None,
        publish_pubsub(
            "current",
            Some(status_preference_payload("manual", Some("dnd"))),
        ),
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
            status_preference_payload("manual", Some("away")),
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
    let alice_bare = format!("alice@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");

    // Make bob a genuine fan-out candidate for alice's §3 roster+CAPS pass:
    // (1) bob is a roster contact of alice with `subscription = from`, and
    // (2) bob advertises `urn:waddle:status-preference:0+notify` in his caps.
    // For a NON-private node both conditions together would deliver the headline
    // to bob — so this setup is what makes the carve-out the real thing under
    // test (delete `is_private_pep_node` for this node and this test fails).
    establish_roster_from(&mut alice, &mut bob, &alice_bare, &bob_bare).await;
    announce_status_preference_notify_caps(&mut bob).await;

    alice
        .send(&publish_iq(
            "alice-pub",
            "current",
            status_preference_payload("manual", Some("dnd")),
        ))
        .await
        .expect("alice publish");
    let _ = alice
        .recv_matching(|f| f.contains(r#"id='alice-pub'"#))
        .await
        .expect("alice publish result");

    // Despite being a roster-`from` contact WITH matching `+notify` caps, bob's
    // stream MUST stay silent for the preference node — the picked status is
    // private and never leaks to contacts.
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

    // `<presence><c hash node ver/></presence>`
    let caps = Element::builder("c", NS_CAPS)
        .attr(attr("hash"), "sha-1")
        .attr(attr("node"), caps_node)
        .attr(attr("ver"), ver.as_str())
        .build();
    let presence = element_to_xml(Element::builder("presence", CLIENT_NS).append(caps).build());
    client.send(&presence).await.expect("presence with caps");

    let disco_query = client
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries caps");
    let iq_id = extract_attr_after(&disco_query, "<iq", "id").expect("disco iq id");

    // `<iq type=result from=…><query node=caps#ver><identity/><feature/>…`
    let mut query = Element::builder("query", NS_DISCO_INFO)
        .attr(attr("node"), format!("{caps_node}#{ver}"))
        .append(
            Element::builder("identity", NS_DISCO_INFO)
                .attr(attr("category"), "client")
                .attr(attr("type"), "pc")
                .attr(attr("name"), "Waddle")
                .build(),
        );
    for feature in features {
        query = query.append(
            Element::builder("feature", NS_DISCO_INFO)
                .attr(attr("var"), feature)
                .build(),
        );
    }
    client
        .send(&iq("result", &iq_id, None, Some(&full), query.build()))
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
    r1.send(&bare_presence()).await.expect("r1 presence");
    announce_status_preference_notify_caps(&mut r2).await;

    // r1 publishes a pick. r2 — alice's other resource — MUST receive
    // the headline event (this is the live cross-device sync path).
    r1.send(&publish_iq(
        "r1-pub",
        "current",
        status_preference_payload("manual", Some("away")),
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

#[tokio::test]
async fn self_fanout_skips_owner_resource_without_notify_caps() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let mut r1 = connect_account(&server, "alice", &alice_password).await;
    let mut r2 = connect_account(&server, "alice", &alice_password).await;

    // Both online, but r2 advertises NO caps — so it does not opt into
    // `urn:waddle:status-preference:0+notify`. The §3.4 owner-self pass is
    // caps-gated, so r2 MUST NOT receive the headline: the `+notify` filter is
    // the real gate, not mere co-ownership of the account.
    r1.send(&bare_presence()).await.expect("r1 presence");
    r2.send(&bare_presence()).await.expect("r2 presence");

    r1.send(&publish_iq(
        "noflt-pub",
        "current",
        status_preference_payload("manual", Some("away")),
    ))
    .await
    .expect("r1 publish");
    let _ = r1
        .recv_matching(|f| f.contains(r#"id='noflt-pub'"#))
        .await
        .expect("r1 publish result");

    let mut received: Option<String> = None;
    for _ in 0..3 {
        match r2.recv_timeout(Duration::from_millis(250)).await {
            Ok(frame) => {
                if frame.contains(NODE) {
                    received = Some(frame);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(
        received.is_none(),
        "a resource without +notify caps must NOT receive the headline: {received:?}"
    );

    let _ = r1.close().await;
    let _ = r2.close().await;
}

#[tokio::test]
async fn preference_survives_disconnect_and_fresh_reconnect() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);

    // First session publishes a manual pick, then fully disconnects.
    {
        let mut r1 = connect_account(&server, "alice", &alice_password).await;
        r1.send(&publish_iq(
            "persist-pub",
            "current",
            status_preference_payload("manual", Some("dnd")),
        ))
        .await
        .expect("publish");
        let res = r1
            .recv_matching(|f| f.contains(r#"id='persist-pub'"#))
            .await
            .expect("publish result");
        assert!(res.contains(r#"type='result'"#), "publish: {res}");
        let _ = r1.close().await;
    }

    // A brand-new connection (fresh login, same account) reads the stored pick
    // back — proving the preference persists across sessions, replacing the
    // Phase 1 in-memory store (AC3).
    let mut r2 = connect_account(&server, "alice", &alice_password).await;
    r2.send(&items_get_iq("persist-fetch", None))
        .await
        .expect("fetch");
    let fetched = r2
        .recv_matching(|f| f.contains(r#"id='persist-fetch'"#))
        .await
        .expect("fetch result");
    assert!(
        fetched.contains(r#"id='current'"#),
        "stored item id must be 'current': {fetched}"
    );
    assert!(
        fetched.contains(r#"mode="manual""#) || fetched.contains(r#"mode='manual'"#),
        "persisted mode missing: {fetched}"
    );
    assert!(
        fetched.contains(r#"status="dnd""#) || fetched.contains(r#"status='dnd'"#),
        "persisted status missing: {fetched}"
    );

    let _ = r2.close().await;
}
