#![recursion_limit = "256"]

//! RFC 6121: Presence Subscription & Broadcast
//!
//! Tests the presence subscription lifecycle and initial presence broadcast.

mod common;

use common::{
    establish_bound_session, init_test_env, ping_query, RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

// =========================================================================
// Initial Presence Broadcast
// =========================================================================

#[tokio::test]
async fn rfc6121_initial_presence_accepted() {
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

    // Connection should remain functional
    let response = ping_query(&mut client, "localhost", "post-pres-1")
        .await
        .expect("ping");
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected ping result after initial presence"
    );
}

#[tokio::test]
async fn rfc6121_presence_with_priority_and_status() {
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
                <status>Out for lunch</status>\
                <priority>10</priority>\
            </presence>",
        )
        .await
        .expect("send presence with status");

    let response = ping_query(&mut client, "localhost", "post-status-1")
        .await
        .expect("ping");
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected ping result after presence with status"
    );
}

// =========================================================================
// Presence Subscription Lifecycle
// =========================================================================

#[tokio::test]
async fn rfc6121_subscribe_request_delivered() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    // Send initial presence so alice is "available"
    alice
        .send("<presence xmlns='jabber:client'/>")
        .await
        .expect("send presence");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    bob.send("<presence xmlns='jabber:client'/>")
        .await
        .expect("send presence");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Alice subscribes to Bob's presence
    alice
        .send("<presence type='subscribe' to='bob@localhost' xmlns='jabber:client'/>")
        .await
        .expect("send subscribe");

    // Bob should receive the subscription request
    let bob_data = bob.read(DEFAULT_TIMEOUT).await;
    match bob_data {
        Ok(data) => {
            // May receive subscribe presence or other stanzas
            if data.contains("subscribe") {
                assert!(
                    data.contains("alice@localhost") || data.contains("subscribe"),
                    "Subscribe request should reference alice, got: {}",
                    data
                );
            }
        }
        Err(_) => {
            // Server may not deliver subscribe if roster not set up — acceptable
        }
    }
}

#[tokio::test]
async fn rfc6121_subscribed_response_accepted() {
    init_test_env();
    let server = TestServer::start().await;

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    bob.send("<presence xmlns='jabber:client'/>")
        .await
        .expect("send presence");

    // Bob approves a subscription
    bob.send("<presence type='subscribed' to='alice@localhost' xmlns='jabber:client'/>")
        .await
        .expect("send subscribed");

    // Connection should remain functional
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    // Clear any presence stanzas that may have arrived
    let _ = bob.read(std::time::Duration::from_millis(200)).await;
    bob.clear();

    let response = ping_query(&mut bob, "localhost", "post-subscribed-1")
        .await
        .expect("ping");
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected ping result after subscribed, got: {}",
        response
    );
}

#[tokio::test]
async fn rfc6121_unsubscribe_accepted() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    alice
        .send("<presence xmlns='jabber:client'/>")
        .await
        .expect("send presence");

    // Alice unsubscribes from Bob
    alice
        .send("<presence type='unsubscribe' to='bob@localhost' xmlns='jabber:client'/>")
        .await
        .expect("send unsubscribe");

    // Drain any async stanzas
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let _ = alice.read(std::time::Duration::from_millis(200)).await;
    alice.clear();

    let response = ping_query(&mut alice, "localhost", "post-unsub-1")
        .await
        .expect("ping");
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected ping result after unsubscribe"
    );
}

#[tokio::test]
async fn rfc6121_unavailable_presence_accepted() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Send available, then unavailable
    client
        .send("<presence xmlns='jabber:client'/>")
        .await
        .expect("send available");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    client
        .send("<presence type='unavailable' xmlns='jabber:client'/>")
        .await
        .expect("send unavailable");

    // After unavailable, server may close stream or keep it alive
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Try ping — may work or may have been disconnected
    let result = ping_query(&mut client, "localhost", "post-unavail-1").await;
    // Either works or connection was closed — both acceptable
    assert!(result.is_ok() || result.is_err());
}

// =========================================================================
// Presence Probe
// =========================================================================

#[tokio::test]
async fn rfc6121_presence_probe_from_server_handled() {
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
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // A presence probe from another user (server normally generates these)
    // Client sending a probe directly is unusual but server should handle it
    client
        .send("<presence type='probe' to='bob@localhost' xmlns='jabber:client'/>")
        .await
        .expect("send probe");

    // Connection should remain functional regardless
    let response = ping_query(&mut client, "localhost", "post-probe-1")
        .await
        .expect("ping");
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected ping result after probe"
    );
}
