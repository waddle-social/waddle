//! XEP-0115 Entity Capabilities resolution wire-conformance tests.
//!
//! Tests cover the inbound `<c hash ver node/>` presence path:
//! - Cache miss triggers an outbound disco#info IQ to the publishing
//!   resource's full JID with `node="<NODE>#<VER>"` per §6.2.
//! - A response whose recomputed hash matches `ver` populates the
//!   server's cache and records the per-resource caps mapping (§5.4).
//! - A response whose recomputed hash mismatches is rejected and not
//!   cached (§5.4 — cache only on match).
//! - Cache hit short-circuits the disco query.
//! - Disconnect drops the per-resource mapping while the hash-keyed
//!   cache stays warm for cross-session reuse (§6).

mod ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{extract_attr_after, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

const NS_CAPS: &str = "http://jabber.org/protocol/caps";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ADMIN, &password, resource)
        .await
        .expect("admin connect")
}

async fn extra_client(
    server: &TestServer,
    user: &str,
    password: &str,
    resource: &str,
) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, resource)
        .await
        .expect("connect_and_auth")
}

/// Pull the IQ id off a server-issued `<iq type="get" .../>` frame so
/// the test can mirror it into the result reply.
fn extract_iq_id(frame: &str) -> String {
    extract_attr_after(frame, "<iq", "id").expect("iq has id attribute")
}

/// Compute the XEP-0115 §5.1 verification string for a disco#info
/// response with the given identity-category/type/name and feature
/// vars. This MUST match the algorithm the server runs so the server
/// accepts the round-tripped reply as valid.
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

// ============================================================================
// Test 1 — caps_unknown_ver_triggers_disco_info_to_full_jid
// ============================================================================
//
// XEP-0115 §6.2: when a recipient sees a <c/> with an unknown (hash,
// ver), it MUST request the entity's identity and supported features
// via a disco#info request with `node="<NODE>#<VER>"`. Here the server
// is the recipient, so receiving alice's presence with caps must
// drive the server to send disco#info to alice's bound full JID.

#[tokio::test]
async fn caps_unknown_ver_triggers_disco_info_to_full_jid() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "caps-trigger-1").await;
    let admin_full_jid = admin.full_jid.clone().expect("bind populated full_jid");

    let node = "https://example.test/caps";
    let ver = "definitely-not-yet-cached-ver-1";
    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("send presence with caps");

    // The server is required to issue a disco#info IQ get back to the
    // resource that just advertised the unknown ver. The query MUST
    // carry node="<NODE>#<VER>" per §6.2.
    let disco_query = admin
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type="get""#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server sends disco#info IQ to resource on cache miss");

    let expected_node_attr = format!(r#"node="{node}#{ver}""#);
    assert!(
        disco_query.contains(&expected_node_attr),
        "disco#info query MUST carry node=\"<NODE>#<VER>\" per XEP-0115 §6.2: {disco_query}"
    );
    assert!(
        disco_query.contains(&format!(r#"to="{admin_full_jid}""#)),
        "disco#info query MUST be addressed to the resource that advertised caps: {disco_query}"
    );

    let _ = admin.close().await;
    let _ = server;
    let _ = Duration::from_secs(1);
}

// ============================================================================
// Test 2 — caps_verified_reply_populates_cache_and_subsequent_advert_is_hit
// ============================================================================
//
// XEP-0115 §5.4: when the disco#info reply's recomputed hash matches
// the advertised `ver`, the recipient MUST cache the result. This is
// keyed on the (hash algo, ver) tuple, not on the entity. So a second
// resource advertising the same `(hash, ver)` is a cache hit and MUST
// NOT trigger another disco#info round-trip — that's the entire
// efficiency win of XEP-0115.

#[tokio::test]
async fn caps_verified_reply_populates_cache_and_subsequent_advert_is_hit() {
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", &bob_password)]);

    let mut admin = admin_client(&server, "caps-pop-1").await;
    let admin_full_jid = admin.full_jid.clone().expect("admin full_jid");

    let node = "https://example.test/caps#hit";
    let features = ["http://jabber.org/protocol/disco#info", "urn:xmpp:ping"];
    let ver = caps_verification_string("client", "pc", "Test Client", &features);

    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("admin sends caps");

    let disco_query = admin
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type="get""#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries admin");
    let iq_id = extract_iq_id(&disco_query);

    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    admin
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{admin_full_jid}"><query xmlns="{NS_DISCO_INFO}" node="{node}#{ver}"><identity category="client" type="pc" name="Test Client"/>{feature_xml}</query></iq>"#
        ))
        .await
        .expect("admin replies to disco#info");

    // Wait for the server to settle the cache write. There's no wire
    // signal for "cache populated", so we use a short delay before
    // having bob advertise the same ver. If verification works the
    // server will short-circuit on the second presence.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut bob = extra_client(&server, "bob", &bob_password, "caps-hit-1").await;
    bob.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
    ))
    .await
    .expect("bob sends caps with same ver");

    // The server must NOT issue a second disco#info, since the
    // (hash, ver) is now cached. Wait a generous window for the
    // *absence* of a query.
    let timed = tokio::time::timeout(
        Duration::from_millis(500),
        bob.recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type="get""#)
                && frame.contains(NS_DISCO_INFO)
        }),
    )
    .await;

    assert!(
        timed.is_err(),
        "cache hit MUST short-circuit disco#info; got an unexpected query: {timed:?}"
    );

    let _ = admin.close().await;
    let _ = bob.close().await;
}

// ============================================================================
// Test 3 — caps_mismatched_hash_reply_is_rejected_and_not_cached
// ============================================================================
//
// XEP-0115 §5.4: when the recomputed hash does NOT match the
// advertised `ver`, the recipient MUST NOT cache the result (defense
// against "caps poisoning" — §8). Subsequent advertisements of the
// same (hash, ver) MUST therefore re-resolve.

#[tokio::test]
async fn caps_mismatched_hash_reply_is_rejected_and_not_cached() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "caps-poison-1").await;
    let admin_full_jid = admin.full_jid.clone().expect("admin full_jid");

    let node = "https://example.test/caps#poison";
    // ver advertised — claims one set of features ...
    let advertised_features = ["http://jabber.org/protocol/disco#info", "urn:xmpp:ping"];
    let advertised_ver =
        caps_verification_string("client", "pc", "Honest Client", &advertised_features);
    // ... but the reply will return a *different* feature set. Their
    // recomputed hash MUST NOT match `advertised_ver`.
    let reply_features = ["urn:xmpp:malicious-feature"];

    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{advertised_ver}"/></presence>"#
        ))
        .await
        .expect("send caps presence");

    let disco_query = admin
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type="get""#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries admin");
    let iq_id = extract_iq_id(&disco_query);

    let feature_xml: String = reply_features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    admin
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{admin_full_jid}"><query xmlns="{NS_DISCO_INFO}" node="{node}#{advertised_ver}"><identity category="client" type="pc" name="Honest Client"/>{feature_xml}</query></iq>"#
        ))
        .await
        .expect("admin sends mismatched reply");

    tokio::time::sleep(Duration::from_millis(150)).await;

    // Re-advertise the same ver from a *fresh* resource on the same
    // user. If the reply was correctly rejected, the server's cache
    // is still empty for `advertised_ver` and a new disco#info MUST
    // fire on the second resource.
    let mut admin2 = admin_client(&server, "caps-poison-2").await;
    admin2
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{advertised_ver}"/></presence>"#
        ))
        .await
        .expect("admin2 sends caps");

    let _re_query = admin2
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type="get""#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect(
            "rejected (poisoned) reply MUST NOT be cached; the next advert MUST re-resolve via disco#info per XEP-0115 §5.4",
        );

    let _ = admin.close().await;
    let _ = admin2.close().await;
}

// ============================================================================
// Test 4 — caps_cache_survives_publisher_disconnect_for_cross_session_reuse
// ============================================================================
//
// XEP-0115 §6: caching across presence sessions is RECOMMENDED — that
// is the whole point of the verification string. So when the resource
// that originally taught the server about a (hash, ver) disconnects,
// the per-resource mapping is cleared (we no longer care which JID
// advertises that ver) but the hash-keyed cache stays warm for any
// future session.
//
// Wire-observable check: alice teaches the server about ver, alice
// disconnects, alice reconnects on a fresh resource and re-advertises
// the same ver. The server MUST hit the cache and skip disco#info.

#[tokio::test]
async fn caps_cache_survives_publisher_disconnect_for_cross_session_reuse() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "caps-warm-1").await;
    let admin_full_jid_1 = admin.full_jid.clone().expect("full jid 1");

    let node = "https://example.test/caps#warm";
    let features = ["http://jabber.org/protocol/disco#info"];
    let ver = caps_verification_string("client", "pc", "Warm Client", &features);

    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("send caps presence");

    let disco_query = admin
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type="get""#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries admin");
    let iq_id = extract_iq_id(&disco_query);
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    admin
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{admin_full_jid_1}"><query xmlns="{NS_DISCO_INFO}" node="{node}#{ver}"><identity category="client" type="pc" name="Warm Client"/>{feature_xml}</query></iq>"#
        ))
        .await
        .expect("admin replies");

    tokio::time::sleep(Duration::from_millis(150)).await;

    // Disconnect — exercises the resource→ver cleanup path. The
    // hash-keyed cache MUST persist.
    let _ = admin.close().await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Reconnect on a FRESH resource and re-advertise the same ver.
    let mut admin2 = admin_client(&server, "caps-warm-2").await;
    admin2
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("admin2 sends caps");

    let timed = tokio::time::timeout(
        Duration::from_millis(500),
        admin2.recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type="get""#)
                && frame.contains(NS_DISCO_INFO)
        }),
    )
    .await;
    assert!(
        timed.is_err(),
        "after publisher disconnects, cache MUST stay warm; \
         the next presence with the same ver should be a hit, got: {timed:?}"
    );

    let _ = admin2.close().await;
}
