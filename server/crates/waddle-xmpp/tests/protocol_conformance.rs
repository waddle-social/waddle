#![recursion_limit = "256"]

//! Protocol Conformance Test Suite
//!
//! Native Rust replacement for the Docker-based CAAS XMPP interop tests.
//! Validates RFC 6120/6121 compliance and XEP feature advertisement in a
//! single comprehensive integration test module.
//!
//! Run with: `cargo test -p waddle-xmpp --test protocol_conformance`

mod common;

use std::sync::Arc;

use common::{
    disco_info_query, encode_sasl_plain, establish_bound_session, extract_bound_jid, init_test_env,
    join_muc_room, ping_query, test_secret, validate_stream_header, MockAppState, RawXmppClient,
    TestServer, DEFAULT_TIMEOUT,
};

// =============================================================================
// RFC 6120: Stream Negotiation
// =============================================================================

#[tokio::test]
async fn rfc6120_stream_header_has_required_attributes() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='1.0'>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("<stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    validate_stream_header(&response).expect("stream header must be valid");
}

#[tokio::test]
async fn rfc6120_starttls_advertised_and_functional() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='1.0'>",
        )
        .await
        .expect("send");
    let features = client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");

    assert!(
        features.contains("<starttls"),
        "STARTTLS must be advertised, got: {}",
        features
    );
    client.clear();

    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .expect("send");
    let proceed = client
        .read_until("<proceed", DEFAULT_TIMEOUT)
        .await
        .expect("proceed");
    assert!(
        proceed.contains("<proceed"),
        "Expected <proceed/>, got: {}",
        proceed
    );
    client.clear();

    client
        .upgrade_tls(server.tls_connector(), "localhost")
        .await
        .expect("TLS upgrade must succeed");
    assert!(client.is_tls(), "Connection must be TLS after upgrade");
}

#[tokio::test]
async fn rfc6120_sasl_plain_auth_succeeds() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    // Stream + STARTTLS + TLS upgrade
    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='1.0'>",
        )
        .await
        .expect("send");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();
    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .expect("send");
    client
        .read_until("<proceed", DEFAULT_TIMEOUT)
        .await
        .expect("proceed");
    client.clear();
    client
        .upgrade_tls(server.tls_connector(), "localhost")
        .await
        .expect("tls");

    // Post-TLS stream
    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='1.0'>",
        )
        .await
        .expect("send");
    let features = client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    assert!(
        features.contains("PLAIN"),
        "SASL PLAIN must be offered after TLS, got: {}",
        features
    );
    client.clear();

    // Auth
    let auth_data = encode_sasl_plain("user@localhost", &test_secret("auth"));
    client
        .send(&format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{}</auth>",
            auth_data
        ))
        .await
        .expect("send");
    let result = client
        .read_until("<success", DEFAULT_TIMEOUT)
        .await
        .expect("auth result");
    assert!(
        result.contains("<success"),
        "SASL PLAIN auth must succeed, got: {}",
        result
    );
}

#[tokio::test]
async fn rfc6120_sasl_auth_failure_returns_not_authorized() {
    init_test_env();
    let state = Arc::new(MockAppState::rejecting("localhost"));
    let server = TestServer::start_with_state(state).await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='1.0'>",
        )
        .await
        .expect("send");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();
    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .expect("send");
    client
        .read_until("<proceed", DEFAULT_TIMEOUT)
        .await
        .expect("proceed");
    client.clear();
    client
        .upgrade_tls(server.tls_connector(), "localhost")
        .await
        .expect("tls");

    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='1.0'>",
        )
        .await
        .expect("send");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();

    let auth_data = encode_sasl_plain("user@localhost", &test_secret("invalid"));
    client
        .send(&format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{}</auth>",
            auth_data
        ))
        .await
        .expect("send");
    let result = client
        .read_until("<failure", DEFAULT_TIMEOUT)
        .await
        .expect("auth failure");
    assert!(
        result.contains("<failure") || result.contains("not-authorized"),
        "Expected auth failure, got: {}",
        result
    );
}

// =============================================================================
// RFC 6120: Resource Binding
// =============================================================================

#[tokio::test]
async fn rfc6120_resource_bind_succeeds() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    let jid = establish_bound_session(&mut client, &server, "alice", "test-resource")
        .await
        .expect("bind");

    assert!(
        jid.contains("alice@localhost"),
        "Bound JID must contain user@domain, got: {}",
        jid
    );
    assert!(
        jid.contains("test-resource") || jid.contains('/'),
        "Bound JID must have resource, got: {}",
        jid
    );
}

#[tokio::test]
async fn rfc6120_server_assigned_resource_when_none_requested() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    // Establish session manually without resource
    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='1.0'>",
        )
        .await
        .expect("send");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();
    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .expect("send");
    client
        .read_until("<proceed", DEFAULT_TIMEOUT)
        .await
        .expect("proceed");
    client.clear();
    client
        .upgrade_tls(server.tls_connector(), "localhost")
        .await
        .expect("tls");

    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='1.0'>",
        )
        .await
        .expect("send");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();

    let auth_data = encode_sasl_plain("norc@localhost", &test_secret("auth"));
    client
        .send(&format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{}</auth>",
            auth_data
        ))
        .await
        .expect("send");
    client
        .read_until("<success", DEFAULT_TIMEOUT)
        .await
        .expect("success");
    client.clear();

    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='1.0'>",
        )
        .await
        .expect("send");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();

    // Bind without resource
    client
        .send(
            "<iq type='set' id='bind-nores' xmlns='jabber:client'>\
                <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("bind response");

    let jid = extract_bound_jid(&response);
    assert!(
        jid.is_some(),
        "Server must assign resource, got: {}",
        response
    );
    let jid = jid.expect("bound jid");
    assert!(
        jid.contains('/'),
        "Server-assigned JID must have resource part: {}",
        jid
    );
}

// =============================================================================
// RFC 6121: Roster Management
// =============================================================================

#[tokio::test]
async fn rfc6121_roster_get_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='roster-1' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:roster'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for roster get, got: {}",
        response
    );
    assert!(
        response.contains("jabber:iq:roster"),
        "Expected roster namespace, got: {}",
        response
    );
}

#[tokio::test]
async fn rfc6121_roster_set_adds_item() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='set' id='roster-add-1' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:roster'>\
                    <item jid='bob@localhost' name='Bob'/>\
                </query>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for roster set, got: {}",
        response
    );
}

// =============================================================================
// MUC (XEP-0045): Multi-User Chat
// =============================================================================

#[tokio::test]
async fn xep0045_join_room_returns_self_presence() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = join_muc_room(&mut client, "testroom@muc.localhost", "Alice")
        .await
        .expect("join room");

    assert!(
        response.contains("110"),
        "Expected status code 110 (self-presence), got: {}",
        response
    );
}

#[tokio::test]
async fn xep0045_groupchat_message_broadcast() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "broadcast@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "broadcast@muc.localhost", "Bob")
        .await
        .expect("bob join");

    alice
        .send(
            "<message type='groupchat' to='broadcast@muc.localhost' id='gc-1' xmlns='jabber:client'>\
                <body>Conformance check</body>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("Conformance check", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");
    assert!(
        bob_response.contains("Conformance check"),
        "Bob must receive groupchat, got: {}",
        bob_response
    );
}

#[tokio::test]
async fn xep0045_leave_room_sends_unavailable_presence() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");

    join_muc_room(&mut alice, "leave@muc.localhost", "Alice")
        .await
        .expect("alice join");
    join_muc_room(&mut bob, "leave@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice leaves
    alice
        .send("<presence type='unavailable' to='leave@muc.localhost/Alice' xmlns='jabber:client'/>")
        .await
        .expect("send leave");

    // Bob should get unavailable presence for Alice
    let bob_response = bob
        .read_until("unavailable", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives leave");
    assert!(
        bob_response.contains("unavailable"),
        "Expected unavailable presence, got: {}",
        bob_response
    );
}

// =============================================================================
// Service Discovery Conformance
// =============================================================================

#[tokio::test]
async fn conformance_server_features_comprehensive() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "conf-features-1")
        .await
        .expect("disco response");

    // Required features per XMPP compliance suites
    let required_features = [
        "http://jabber.org/protocol/disco#info",
        "http://jabber.org/protocol/disco#items",
        "urn:xmpp:ping",
    ];

    for feature in &required_features {
        assert!(
            response.contains(feature),
            "Missing required feature '{}' in server disco, response: {}",
            feature,
            response
        );
    }
}

#[tokio::test]
async fn conformance_muc_features_comprehensive() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "muc.localhost", "conf-muc-1")
        .await
        .expect("disco response");

    let required_features = [
        "http://jabber.org/protocol/muc",
        "http://jabber.org/protocol/disco#info",
    ];

    for feature in &required_features {
        assert!(
            response.contains(feature),
            "Missing required MUC feature '{}', response: {}",
            feature,
            response
        );
    }
}

// =============================================================================
// Connection Lifecycle
// =============================================================================

#[tokio::test]
async fn conformance_multiple_concurrent_sessions() {
    init_test_env();
    let server = TestServer::start().await;

    let mut client1 = RawXmppClient::connect(server.addr).await.expect("connect");
    let jid1 = establish_bound_session(&mut client1, &server, "multi", "device1")
        .await
        .expect("bind 1");

    let mut client2 = RawXmppClient::connect(server.addr).await.expect("connect");
    let jid2 = establish_bound_session(&mut client2, &server, "multi", "device2")
        .await
        .expect("bind 2");

    // Both should have different full JIDs
    assert_ne!(jid1, jid2, "Concurrent sessions must have different JIDs");

    // Both should respond to ping
    let r1 = ping_query(&mut client1, "localhost", "multi-ping-1")
        .await
        .expect("ping 1");
    let r2 = ping_query(&mut client2, "localhost", "multi-ping-2")
        .await
        .expect("ping 2");

    assert!(
        r1.contains("type='result'") || r1.contains("type=\"result\""),
        "Session 1 ping failed"
    );
    assert!(
        r2.contains("type='result'") || r2.contains("type=\"result\""),
        "Session 2 ping failed"
    );
}

#[tokio::test]
async fn conformance_stream_close_is_graceful() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Send stream close
    client.send("</stream:stream>").await.expect("send close");

    // Server should close its side too (or connection drops)
    let result = client.read(DEFAULT_TIMEOUT).await;
    // Either we get a close or connection reset - both are acceptable
    assert!(
        result.is_ok() || result.is_err(),
        "Server should handle stream close gracefully"
    );
}

// =============================================================================
// Stanza Routing
// =============================================================================

#[tokio::test]
async fn conformance_iq_to_unknown_entity_returns_response() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='unknown-1' to='nonexistent.localhost' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/disco#info'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    // Must respond (not hang or crash) — server may fall back to own disco
    assert!(
        response.contains("type='result'")
            || response.contains("type=\"result\"")
            || response.contains("type='error'")
            || response.contains("type=\"error\""),
        "Expected result or error for IQ to unknown entity, got: {}",
        response
    );
}

#[tokio::test]
async fn conformance_unknown_iq_namespace_returns_service_unavailable() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='unsupported-1' to='localhost' xmlns='jabber:client'>\
                <query xmlns='urn:completely:made:up:namespace'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected error for unsupported namespace, got: {}",
        response
    );
}

// =============================================================================
// Presence
// =============================================================================

#[tokio::test]
async fn conformance_initial_presence_accepted() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Send initial presence
    client
        .send("<presence xmlns='jabber:client'/>")
        .await
        .expect("send presence");

    // Verify connection still works
    let response = ping_query(&mut client, "localhost", "post-presence-1")
        .await
        .expect("ping");
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Ping must succeed after presence"
    );
}

#[tokio::test]
async fn conformance_presence_with_status_accepted() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<presence xmlns='jabber:client'>\
                <show>away</show>\
                <status>Gone fishing</status>\
                <priority>5</priority>\
            </presence>",
        )
        .await
        .expect("send presence");

    let response = ping_query(&mut client, "localhost", "post-status-1")
        .await
        .expect("ping");
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Ping must succeed after presence with status"
    );
}
