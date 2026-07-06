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
//! - Legacy pre-v1.4 `<c/>` without a `hash` attribute is unverifiable
//!   and MUST NOT trigger resolution (§6.1 legacy format).
//! - An ill-formed reply (duplicate feature var, §5.4 step 2.4) is
//!   discarded whole and the next advert re-resolves.
//! - A reply arriving from a different resource than the one queried
//!   is dropped and never cached.
//! - Re-advertising the same `(hash, ver)` while a resolution is
//!   already in flight MUST NOT queue a second disco#info query.

use waddle_ws_test_support as ws_common;

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

/// XEP-0115 §5.1 with one XEP-0128 form. The wire-test for the
/// software-info form path needs the server's hash to incorporate
/// the form so the verification matches.
fn caps_verification_string_with_softwareinfo_form(
    identity_category: &str,
    identity_type: &str,
    identity_name: &str,
    features: &[&str],
    form_type: &str,
    software: &str,
) -> String {
    use waddle_xmpp::disco::info::{Feature, Identity};
    use waddle_xmpp::xep::xep0115::compute_caps_hash_with_extensions;
    use xmpp_parsers::minidom::Element;

    let identities = vec![Identity::new(
        identity_category,
        identity_type,
        Some(identity_name),
    )];
    let features: Vec<Feature> = features.iter().map(|f| Feature::new(f)).collect();
    let form = Element::builder("x", "jabber:x:data")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("field", "jabber:x:data")
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                .append(
                    Element::builder("value", "jabber:x:data")
                        .append(form_type)
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", "jabber:x:data")
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "software")
                .append(
                    Element::builder("value", "jabber:x:data")
                        .append(software)
                        .build(),
                )
                .build(),
        )
        .build();
    compute_caps_hash_with_extensions(&identities, &features, std::slice::from_ref(&form))
}

/// Send a ping IQ and wait for its result. Used as a deterministic
/// FIFO anchor for "MUST NOT receive frame X" assertions: anything
/// that would have arrived before the ping reply has been emitted
/// by the server when the ping result lands.
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
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server sends disco#info IQ to resource on cache miss");

    let expected_node_attr = format!(r#"node='{node}#{ver}'"#);
    assert!(
        disco_query.contains(&expected_node_attr),
        "disco#info query MUST carry node='<NODE>#<VER>' per XEP-0115 §6.2: {disco_query}"
    );
    assert!(
        disco_query.contains(&format!(r#"to='{admin_full_jid}'"#)),
        "disco#info query MUST be addressed to the resource that advertised caps: {disco_query}"
    );

    let _ = admin.close().await;
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
                && frame.contains(r#"type='get'"#)
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
                && frame.contains(r#"type='get'"#)
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
                && frame.contains(r#"type='get'"#)
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
                && frame.contains(r#"type='get'"#)
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
// Test 3b — caps_iq_error_reply_is_not_cached_and_re_resolves
// ============================================================================
//
// XEP-0115 §5.4: a disco#info IQ-error reply is not a verifiable
// caps assertion. The recipient MUST NOT cache anything and the next
// advertisement of the same `(hash, ver)` MUST re-resolve.

#[tokio::test]
async fn caps_iq_error_reply_is_not_cached_and_re_resolves() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "caps-iqerr-1").await;
    let admin_full_jid = admin.full_jid.clone().expect("full jid");

    let node = "https://example.test/caps#iq-error";
    let features = ["urn:xmpp:ping"];
    let ver = caps_verification_string("client", "pc", "Erroring Client", &features);

    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("send caps presence");

    let disco_query = admin
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries admin");
    let iq_id = extract_iq_id(&disco_query);

    // Reply with an IQ error instead of a result.
    admin
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="error" id="{iq_id}" from="{admin_full_jid}"><error type="cancel"><service-unavailable xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#
        ))
        .await
        .expect("admin replies with IQ error");

    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut admin2 = admin_client(&server, "caps-iqerr-2").await;
    admin2
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("admin2 sends caps");

    let _re_query = admin2
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("IQ error MUST NOT populate cache; next advert MUST re-resolve");

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
                && frame.contains(r#"type='get'"#)
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
                && frame.contains(r#"type='get'"#)
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

// ============================================================================
// Test 5 — caps_reply_with_xep0128_form_verifies_and_caches
// ============================================================================
//
// XEP-0128 / XEP-0115 §5.1: disco#info responses MAY include
// `<x xmlns="jabber:x:data" type="result">` extension forms whose
// FORM_TYPE values participate in the verification string. Real-world
// clients (Gajim, Dino, Conversations) emit a `urn:xmpp:dataforms:softwareinfo`
// form by default. PR #438's adversarial review (issue #1) flagged
// that the previous fix was unit-tested only — this test exercises
// the same path end-to-end over the wire.
#[tokio::test]
async fn caps_reply_with_xep0128_form_verifies_and_caches() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "caps-form-1").await;
    let admin_full_jid = admin.full_jid.clone().expect("full jid");

    let node = "https://example.test/caps#form";
    let features = ["http://jabber.org/protocol/disco#info", "urn:xmpp:ping"];
    let form_type = "urn:xmpp:dataforms:softwareinfo";
    let software = "Waddle Test Client";
    let ver = caps_verification_string_with_softwareinfo_form(
        "client",
        "pc",
        "Form Client",
        &features,
        form_type,
        software,
    );

    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("send presence with caps");
    let disco_query = admin
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries admin");
    let iq_id = extract_iq_id(&disco_query);

    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    let form_xml = format!(
        r#"<x xmlns="jabber:x:data" type="result"><field var="FORM_TYPE" type="hidden"><value>{form_type}</value></field><field var="software"><value>{software}</value></field></x>"#
    );
    admin
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{admin_full_jid}"><query xmlns="{NS_DISCO_INFO}" node="{node}#{ver}"><identity category="client" type="pc" name="Form Client"/>{feature_xml}{form_xml}</query></iq>"#
        ))
        .await
        .expect("admin replies with form-bearing disco#info");

    // Anchor: ping result confirms the disco#info reply was processed
    // by the server's frame loop in FIFO order.
    ping_anchor(&mut admin, "caps-form-anchor-1").await;

    // A second resource advertising the same ver MUST be a cache hit.
    let mut admin2 = admin_client(&server, "caps-form-2").await;
    admin2
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("admin2 sends caps");
    ping_anchor(&mut admin2, "caps-form-anchor-2").await;

    let timed = tokio::time::timeout(
        Duration::from_millis(50),
        admin2.recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        }),
    )
    .await;
    assert!(
        timed.is_err(),
        "form-bearing reply MUST recompute correctly per XEP-0128 + §5.1 \
         and populate the cache; second advert should be a hit. Got: {timed:?}"
    );

    let _ = admin.close().await;
    let _ = admin2.close().await;
}

// ============================================================================
// Test 6 — caps_unsupported_hash_algorithm_skips_resolution
// ============================================================================
//
// XEP-0115 §5.4 step 2 + §8.1: the recipient MUST NOT cache a
// result it cannot verify. SHA-1 is the only mandatory-to-implement
// algorithm; for any other `hash` attribute the server skips the
// disco#info round-trip rather than produce an unverifiable cache
// entry.
#[tokio::test]
async fn caps_unsupported_hash_algorithm_skips_resolution() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "caps-unsupp-1").await;

    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-256" node="https://example.test/caps#sha256" ver="some-base64-string"/></presence>"#
        ))
        .await
        .expect("send presence with sha-256 caps");
    ping_anchor(&mut admin, "caps-unsupp-anchor-1").await;

    let timed = tokio::time::timeout(
        Duration::from_millis(50),
        admin.recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        }),
    )
    .await;
    assert!(
        timed.is_err(),
        "advertised sha-256 hash MUST NOT trigger a disco#info round-trip; \
         the server has no way to verify the reply per §5.4 step 2. Got: {timed:?}"
    );

    let _ = admin.close().await;
}

// ============================================================================
// Test 7 — caps_cache_is_not_poisoned_after_mismatched_reply
// ============================================================================
//
// PR #438 review issue #10 hardening: confirm Test 3's "the cache
// wasn't populated" claim by driving a *third* resource through a
// successful resolution and verifying the cache then carries the
// CORRECT identity/feature set, not the earlier poisoned payload.
#[tokio::test]
async fn caps_cache_is_not_poisoned_after_mismatched_reply() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "caps-poison2-1").await;
    let admin_full_jid = admin.full_jid.clone().expect("full");

    let node = "https://example.test/caps#poison2";
    let features = ["http://jabber.org/protocol/disco#info", "urn:xmpp:ping"];
    let ver = caps_verification_string("client", "pc", "Honest Client", &features);

    // Round 1: poisoned reply — features won't recompute to ver.
    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("send presence");
    let q = admin
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("disco");
    let id = extract_iq_id(&q);
    let bogus_xml = r#"<feature var="urn:xmpp:malicious"/>"#;
    admin
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{id}" from="{admin_full_jid}"><query xmlns="{NS_DISCO_INFO}" node="{node}#{ver}"><identity category="client" type="pc" name="Honest Client"/>{bogus_xml}</query></iq>"#
        ))
        .await
        .expect("send poisoned reply");
    ping_anchor(&mut admin, "caps-poison2-anchor-1").await;

    // Round 2: a fresh resource advertises the same ver. Server MUST
    // re-resolve (cache should be empty for this ver).
    let mut admin2 = admin_client(&server, "caps-poison2-2").await;
    admin2
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("send presence");
    let q2 = admin2
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("re-resolve fired");
    let id2 = extract_iq_id(&q2);

    // This time send the CORRECT reply.
    let admin2_full_jid = admin2.full_jid.clone().expect("admin2 full");
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    admin2
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{id2}" from="{admin2_full_jid}"><query xmlns="{NS_DISCO_INFO}" node="{node}#{ver}"><identity category="client" type="pc" name="Honest Client"/>{feature_xml}</query></iq>"#
        ))
        .await
        .expect("send correct reply");
    ping_anchor(&mut admin2, "caps-poison2-anchor-2").await;

    // Round 3: verify the cache now carries the correct features by
    // having yet another resource hit the cache (no disco#info).
    let mut admin3 = admin_client(&server, "caps-poison2-3").await;
    admin3
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("send presence");
    ping_anchor(&mut admin3, "caps-poison2-anchor-3").await;
    let timed = tokio::time::timeout(
        Duration::from_millis(50),
        admin3.recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        }),
    )
    .await;
    assert!(
        timed.is_err(),
        "after a poisoned reply followed by a CORRECT reply, the cache MUST \
         carry the correct (non-poisoned) entry; subsequent advert is a hit. Got: {timed:?}"
    );

    let _ = admin.close().await;
    let _ = admin2.close().await;
    let _ = admin3.close().await;
}

// ============================================================================
// Test 8 — caps_multi_resource_independent_tracking
// ============================================================================
//
// PR description (PR 1) and CLAUDE.md hard rule require: per-resource
// caps tracking is independent. Two resources of the same user
// advertising DIFFERENT `ver` values MUST resolve independently and
// neither resolution should pollute the other's cache entry. This is
// the test the original PR description named but did not include.
#[tokio::test]
async fn caps_multi_resource_independent_tracking() {
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", &bob_password)]);

    // Two resources of the SAME user, each advertising a distinct
    // `(node, ver)` so their caps must be tracked separately.
    let mut admin = admin_client(&server, "caps-multi-admin").await;
    let admin_full = admin.full_jid.clone().expect("admin full");
    let mut bob = extra_client(&server, "bob", &bob_password, "caps-multi-bob").await;
    let bob_full = bob.full_jid.clone().expect("bob full");

    let admin_features = ["http://jabber.org/protocol/disco#info", "urn:xmpp:ping"];
    let bob_features = [
        "http://jabber.org/protocol/disco#info",
        "urn:xmpp:carbons:2",
    ];
    let admin_node = "https://example.test/caps#admin";
    let bob_node = "https://example.test/caps#bob";
    let admin_ver = caps_verification_string("client", "pc", "Admin Client", &admin_features);
    let bob_ver = caps_verification_string("client", "phone", "Bob Client", &bob_features);

    // Each resource advertises its own caps. The server MUST issue a
    // separate disco#info to each resource (no cross-pollution).
    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{admin_node}" ver="{admin_ver}"/></presence>"#
        ))
        .await
        .expect("admin presence");
    bob.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{bob_node}" ver="{bob_ver}"/></presence>"#
    ))
    .await
    .expect("bob presence");

    let admin_query = admin
        .recv_matching(|f| {
            f.contains("<iq")
                && f.contains(r#"type='get'"#)
                && f.contains(NS_DISCO_INFO)
                && f.contains(&format!(r#"node='{admin_node}#{admin_ver}'"#))
        })
        .await
        .expect("admin disco");
    let admin_id = extract_iq_id(&admin_query);
    let bob_query = bob
        .recv_matching(|f| {
            f.contains("<iq")
                && f.contains(r#"type='get'"#)
                && f.contains(NS_DISCO_INFO)
                && f.contains(&format!(r#"node='{bob_node}#{bob_ver}'"#))
        })
        .await
        .expect("bob disco");
    let bob_id = extract_iq_id(&bob_query);

    let admin_feature_xml: String = admin_features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    let bob_feature_xml: String = bob_features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    admin
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{admin_id}" from="{admin_full}"><query xmlns="{NS_DISCO_INFO}" node="{admin_node}#{admin_ver}"><identity category="client" type="pc" name="Admin Client"/>{admin_feature_xml}</query></iq>"#
        ))
        .await
        .expect("admin reply");
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{bob_id}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="{bob_node}#{bob_ver}"><identity category="client" type="phone" name="Bob Client"/>{bob_feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob reply");

    ping_anchor(&mut admin, "caps-multi-anchor-1").await;
    ping_anchor(&mut bob, "caps-multi-anchor-2").await;

    // Confirm independent caching: a third resource advertising the
    // admin ver hits the cache (no disco#info), and a fourth resource
    // advertising bob's ver also hits independently.
    let mut admin2 = admin_client(&server, "caps-multi-admin2").await;
    admin2
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{admin_node}" ver="{admin_ver}"/></presence>"#
        ))
        .await
        .expect("admin2 presence");
    ping_anchor(&mut admin2, "caps-multi-anchor-3").await;
    let admin2_timed = tokio::time::timeout(
        Duration::from_millis(50),
        admin2.recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        }),
    )
    .await;
    assert!(
        admin2_timed.is_err(),
        "admin's ver MUST be independently cached (no second disco#info): {admin2_timed:?}"
    );

    let mut bob2 = extra_client(&server, "bob", &bob_password, "caps-multi-bob2").await;
    bob2.send(&format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{bob_node}" ver="{bob_ver}"/></presence>"#
    ))
    .await
    .expect("bob2 presence");
    ping_anchor(&mut bob2, "caps-multi-anchor-4").await;
    let bob2_timed = tokio::time::timeout(
        Duration::from_millis(50),
        bob2.recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        }),
    )
    .await;
    assert!(
        bob2_timed.is_err(),
        "bob's ver MUST be independently cached: {bob2_timed:?}"
    );

    let _ = admin.close().await;
    let _ = admin2.close().await;
    let _ = bob.close().await;
    let _ = bob2.close().await;
}

// ============================================================================
// Test 9 — caps_legacy_advert_without_hash_attribute_is_ignored
// ============================================================================
//
// XEP-0115 §6.1: the pre-v1.4 legacy format omits the `hash`
// attribute (`<c node ver ext/>` where `ver` is a plain version
// string, not a verification hash). There is nothing to verify, so
// the server MUST NOT start a disco#info resolution — an unverifiable
// entry can never be cached (§5.4 step 2).
#[tokio::test]
async fn caps_legacy_advert_without_hash_attribute_is_ignored() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "caps-legacy-1").await;

    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" node="http://legacy.example/client" ver="1.2.3"/></presence>"#
        ))
        .await
        .expect("send legacy caps presence");
    ping_anchor(&mut admin, "caps-legacy-anchor-1").await;

    let timed = tokio::time::timeout(
        Duration::from_millis(50),
        admin.recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        }),
    )
    .await;
    assert!(
        timed.is_err(),
        "legacy (hash-less) caps MUST NOT trigger disco#info resolution: {timed:?}"
    );

    let _ = admin.close().await;
}

// ============================================================================
// Test 10 — caps_duplicate_feature_reply_is_ill_formed_and_not_cached
// ============================================================================
//
// XEP-0115 §5.4 step 2.4: if the disco#info response includes more
// than one identical `<feature/>`, the response is ill-formed and the
// entity MUST discard it entirely. Even though the duplicated feature
// would hash to the advertised ver only if the sender crafted it that
// way, the well-formedness check runs FIRST — nothing is cached and a
// later advert of the same ver MUST re-resolve.
#[tokio::test]
async fn caps_duplicate_feature_reply_is_ill_formed_and_not_cached() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "caps-illformed-1").await;
    let admin_full_jid = admin.full_jid.clone().expect("full jid");

    let node = "https://example.test/caps#ill-formed";
    let features = ["urn:xmpp:ping"];
    let ver = caps_verification_string("client", "pc", "Duplicating Client", &features);

    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("send caps presence");
    let disco_query = admin
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries admin");
    let iq_id = extract_iq_id(&disco_query);

    // The reply duplicates the `urn:xmpp:ping` feature — ill-formed
    // per §5.4 step 2.4 regardless of what it recomputes to.
    admin
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{admin_full_jid}"><query xmlns="{NS_DISCO_INFO}" node="{node}#{ver}"><identity category="client" type="pc" name="Duplicating Client"/><feature var="urn:xmpp:ping"/><feature var="urn:xmpp:ping"/></query></iq>"#
        ))
        .await
        .expect("admin sends duplicate-feature reply");
    ping_anchor(&mut admin, "caps-illformed-anchor-1").await;

    // A fresh resource advertising the same ver MUST re-resolve —
    // the ill-formed reply must not have populated the cache.
    let mut admin2 = admin_client(&server, "caps-illformed-2").await;
    admin2
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("admin2 sends caps");
    let _re_query = admin2
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("ill-formed reply MUST NOT be cached; next advert MUST re-resolve (§5.4 step 2.4)");

    let _ = admin.close().await;
    let _ = admin2.close().await;
}

// ============================================================================
// Test 11 — caps_reply_from_wrong_resource_is_dropped_and_not_cached
// ============================================================================
//
// Anti-spoofing: the server queried resource A, so a result carrying
// the same IQ id but arriving over resource B's stream is not a valid
// answer. It MUST be dropped without caching — otherwise any co-tenant
// who can observe/guess IQ ids could poison the shared caps cache.
#[tokio::test]
async fn caps_reply_from_wrong_resource_is_dropped_and_not_cached() {
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", &bob_password)]);

    let mut admin = admin_client(&server, "caps-spoof-1").await;
    let mut bob = extra_client(&server, "bob", &bob_password, "caps-spoof-bob").await;
    let bob_full = bob.full_jid.clone().expect("bob full");

    let node = "https://example.test/caps#spoof";
    let features = ["urn:xmpp:ping"];
    let ver = caps_verification_string("client", "pc", "Spoofed Client", &features);

    admin
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("admin sends caps");
    let disco_query = admin
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries admin");
    let iq_id = extract_iq_id(&disco_query);

    // Bob answers admin's query id over HIS OWN stream with a payload
    // that WOULD verify. The server must ignore it: wrong sender.
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    bob.send(&format!(
        r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{bob_full}"><query xmlns="{NS_DISCO_INFO}" node="{node}#{ver}"><identity category="client" type="pc" name="Spoofed Client"/>{feature_xml}</query></iq>"#
    ))
    .await
    .expect("bob sends spoofed reply");
    ping_anchor(&mut bob, "caps-spoof-anchor-1").await;

    // A fresh resource advertising the same ver MUST re-resolve —
    // the spoofed reply must not have landed in the cache.
    let mut admin2 = admin_client(&server, "caps-spoof-2").await;
    admin2
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("admin2 sends caps");
    let _re_query = admin2
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("a reply from the wrong resource MUST be dropped, not cached");

    let _ = admin.close().await;
    let _ = admin2.close().await;
    let _ = bob.close().await;
}

// ============================================================================
// Test 12 — caps_repeated_advert_while_resolution_pending_sends_one_query
// ============================================================================
//
// Anti-amplification: `CapsResolver::has_pending_for` guards against a
// client spamming the same unknown `(hash, ver)` in rapid presence
// updates. Only ONE outbound disco#info may be in flight per
// (resource, hash, ver); the repeats must not fan out into a query
// storm.
#[tokio::test]
async fn caps_repeated_advert_while_resolution_pending_sends_one_query() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "caps-pending-1").await;

    let node = "https://example.test/caps#pending";
    let ver = "unresolved-ver-pending-dedup";
    let presence = format!(
        r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{node}" ver="{ver}"/></presence>"#
    );
    admin.send(&presence).await.expect("first caps presence");
    admin.send(&presence).await.expect("second caps presence");
    admin.send(&presence).await.expect("third caps presence");

    // The disco#info query is emitted from the async caps-resolution path,
    // so it is NOT FIFO-ordered against a ping anchor sent after the
    // presences (issue #1188 flake). Wait for the query itself first, then
    // anchor and assert no *further* query snuck out for the repeats.
    let first_query = admin
        .recv_matching(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .await
        .expect("one disco#info query for the unresolved (hash, ver)");
    assert!(
        first_query.contains(&format!(r#"node='{node}#{ver}'"#)),
        "query targets the advertised node#ver: {first_query}"
    );

    admin
        .send(r#"<iq xmlns="jabber:client" type="get" id="caps-pending-anchor-1"><ping xmlns="urn:xmpp:ping"/></iq>"#)
        .await
        .expect("send ping anchor");
    let frames = admin
        .recv_until(|frame| {
            frame.contains(r#"id='caps-pending-anchor-1'"#) && frame.contains("<iq")
        })
        .await
        .expect("frames up to ping anchor");

    let extra_queries: Vec<&String> = frames
        .iter()
        .filter(|frame| {
            frame.contains("<iq")
                && frame.contains(r#"type='get'"#)
                && frame.contains(NS_DISCO_INFO)
        })
        .collect();
    assert!(
        extra_queries.is_empty(),
        "exactly one disco#info may be in flight for a repeated (hash, ver); got extras: {extra_queries:?}"
    );

    let _ = admin.close().await;
}

/// Serialize a roster-get `<iq id=…>` via minidom — stanzas with interpolated
/// values are built with the XML builder, never `format!`.
fn roster_get_iq_xml(id: &str) -> String {
    let attr = |name: &str| {
        <minidom::rxml::NcName as std::convert::TryFrom<&str>>::try_from(name)
            .expect("static ncname is valid")
    };
    let element = minidom::Element::builder("iq", "jabber:client")
        .attr(attr("type"), "get")
        .attr(attr("id"), id)
        .append(minidom::Element::builder("query", "jabber:iq:roster").build())
        .build();
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize element");
    String::from_utf8(bytes).expect("serializer emits utf-8")
}

/// XEP-0115 §1: caps describe the *generating entity*. Presence relayed to a
/// subscriber must carry the publishing client's own `<c/>` verbatim — the
/// server must never substitute its own caps element (issue #1101), or
/// subscribers cache a wrong ver hash per contact resource and feature
/// detection (e.g. Jingle) breaks.
#[tokio::test]
async fn caps_element_in_broadcast_presence_is_the_publishing_clients_own() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = extra_client(&server, "alice", &alice_password, "caps-relay-alice").await;
    let mut bob = extra_client(&server, "bob", &bob_password, "caps-relay-bob").await;

    // Roster interest is required before subscription pushes are delivered.
    for (client, id) in [
        (&mut alice, "caps-relay-roster-alice"),
        (&mut bob, "caps-relay-roster-bob"),
    ] {
        client
            .send(&roster_get_iq_xml(id))
            .await
            .expect("send roster get");
        client
            .recv_matching(|frame| frame.contains(id))
            .await
            .expect("roster result");
    }

    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available");
    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("bob subscribes");
    alice
        .recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("alice sees subscribe");
    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribed" to="bob@localhost"/>"#)
        .await
        .expect("alice approves");
    bob.recv_matching(|frame| frame.contains("type='subscribed'"))
        .await
        .expect("bob sees approval");
    bob.send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("bob available");

    alice
        .send(
            r#"<presence xmlns="jabber:client"><c xmlns="http://jabber.org/protocol/caps" hash="sha-1" node="https://client.example/caps" ver="8RovUdtOmiAjzj+xI7SK5BCw3A8="/></presence>"#,
        )
        .await
        .expect("alice advertises her client's caps");
    let relayed = bob
        .recv_matching(|frame| frame.contains("from='alice@localhost/") && frame.contains(NS_CAPS))
        .await
        .expect("bob receives relayed caps presence");
    assert!(
        relayed.contains("https://client.example/caps")
            && relayed.contains("8RovUdtOmiAjzj+xI7SK5BCw3A8="),
        "relayed presence must carry the client's own node/ver: {relayed}"
    );
    assert_eq!(
        relayed
            .matches("<c xmlns='http://jabber.org/protocol/caps'")
            .count(),
        1,
        "relayed presence must carry exactly the client's own caps element: {relayed}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}
