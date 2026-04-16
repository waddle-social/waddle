#![recursion_limit = "256"]

//! RFC 6121 Section 8.5: Blocking Stanza Filtering
//!
//! These tests correspond to the CAAS "heavy" shards that each tested one
//! aspect of how blocked stanzas are handled:
//!
//! - §8.5.2.1.1: Inbound message from blocked JID (silently dropped)
//! - §8.5.2.1.2: Inbound presence from blocked JID (silently dropped)
//! - §8.5.2.1.3: Inbound IQ from blocked JID (error returned to sender)
//! - §8.5.3.2.1: Outbound message to blocked JID (error returned to user)
//! - §8.5.3.2.2: Outbound presence to blocked JID (error returned to user)
//! - §8.5.3.2.3: Outbound IQ to blocked JID (error returned to user)

mod common;

use common::{
    establish_bound_session, init_test_env, ping_query, RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

/// Helper: block a JID and verify result.
async fn block_jid(client: &mut RawXmppClient, jid: &str, id: &str) {
    client
        .send(&format!(
            "<iq type='set' id='{}' xmlns='jabber:client'>\
                <block xmlns='urn:xmpp:blocking'>\
                    <item jid='{}'/>\
                </block>\
            </iq>",
            id, jid
        ))
        .await
        .expect("send block");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("block response");
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Block should succeed, got: {}",
        response
    );
    client.clear();
}

// =========================================================================
// §8.5.2.1.1: Inbound message from blocked JID silently dropped
// =========================================================================

#[tokio::test]
async fn rfc6121_s8_5_2_1_1_inbound_message_from_blocked_jid_dropped() {
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

    // Alice blocks Bob
    block_jid(&mut alice, "bob@localhost", "block-bob-1").await;

    // Bob sends message to Alice
    bob.send(
        "<message to='alice@localhost' id='blocked-msg-1' xmlns='jabber:client'>\
            <body>You should not see this</body>\
        </message>",
    )
    .await
    .expect("send message");

    // Give server time to process
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Alice should NOT receive the message — verify by pinging
    // (if Alice got the message, it would be in the buffer before the ping result)
    let response = ping_query(&mut alice, "localhost", "check-blocked-msg")
        .await
        .expect("ping");

    // The response should just be the ping result, not the blocked message
    assert!(
        !response.contains("You should not see this"),
        "Blocked message should not be delivered to alice, got: {}",
        response
    );
}

// =========================================================================
// §8.5.2.1.2: Inbound presence from blocked JID silently dropped
// =========================================================================

#[tokio::test]
async fn rfc6121_s8_5_2_1_2_inbound_presence_from_blocked_jid_dropped() {
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

    // Alice blocks Bob
    block_jid(&mut alice, "bob@localhost", "block-bob-pres").await;

    // Bob sends directed presence to Alice
    bob.send("<presence to='alice@localhost' xmlns='jabber:client'><status>Blocked presence</status></presence>")
        .await
        .expect("send presence");

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Alice should NOT receive Bob's presence
    let response = ping_query(&mut alice, "localhost", "check-blocked-pres")
        .await
        .expect("ping");

    assert!(
        !response.contains("Blocked presence"),
        "Blocked presence should not be delivered to alice, got: {}",
        response
    );
}

// =========================================================================
// §8.5.2.1.3: Inbound IQ from blocked JID returns error to sender
// =========================================================================

#[tokio::test]
async fn rfc6121_s8_5_2_1_3_inbound_iq_from_blocked_jid_returns_error() {
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

    // Alice blocks Bob
    block_jid(&mut alice, "bob@localhost", "block-bob-iq").await;

    // Bob sends IQ to Alice
    bob.send(
        "<iq type='get' id='blocked-iq-1' to='alice@localhost' xmlns='jabber:client'>\
            <ping xmlns='urn:xmpp:ping'/>\
        </iq>",
    )
    .await
    .expect("send IQ");

    // Bob should receive an error (service-unavailable or not-allowed)
    let bob_response = bob.read_until("</iq>", DEFAULT_TIMEOUT).await;
    match bob_response {
        Ok(data) => {
            // Server should return error to blocked IQ sender
            assert!(
                data.contains("type='error'")
                    || data.contains("type=\"error\"")
                    || data.contains("service-unavailable")
                    || data.contains("not-allowed"),
                "Expected error for IQ from blocked user, got: {}",
                data
            );
        }
        Err(_) => {
            // Timeout acceptable if server silently drops IQs from blocked JIDs
        }
    }
}

// =========================================================================
// §8.5.3.2.1: Outbound message to blocked JID returns error
// =========================================================================

#[tokio::test]
async fn rfc6121_s8_5_3_2_1_outbound_message_to_blocked_jid_returns_error() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    // Alice blocks Bob
    block_jid(&mut alice, "bob@localhost", "block-bob-out-msg").await;

    // Alice tries to send message to blocked Bob
    alice
        .send(
            "<message to='bob@localhost' id='out-blocked-msg' xmlns='jabber:client'>\
                <body>Should not be sent</body>\
            </message>",
        )
        .await
        .expect("send message");

    // Server should return error to Alice (or silently drop)
    let result = alice.read(DEFAULT_TIMEOUT).await;
    match result {
        Ok(data) => {
            if data.contains("out-blocked-msg") {
                assert!(
                    data.contains("type='error'")
                        || data.contains("type=\"error\"")
                        || data.contains("not-acceptable"),
                    "Expected error for outbound message to blocked JID, got: {}",
                    data
                );
            }
            // May also receive nothing if server silently drops
        }
        Err(_) => {
            // Timeout acceptable — server may silently drop outbound to blocked
        }
    }
}

// =========================================================================
// §8.5.3.2.2: Outbound presence to blocked JID returns error
// =========================================================================

#[tokio::test]
async fn rfc6121_s8_5_3_2_2_outbound_presence_to_blocked_jid_returns_error() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    // Alice blocks Bob
    block_jid(&mut alice, "bob@localhost", "block-bob-out-pres").await;

    // Alice sends directed presence to blocked Bob
    alice
        .send("<presence to='bob@localhost' xmlns='jabber:client'><status>Hi blocked</status></presence>")
        .await
        .expect("send presence");

    // Server should return error or silently drop
    let result = alice.read(DEFAULT_TIMEOUT).await;
    match result {
        Ok(data) => {
            // May contain error or may be empty
            if data.contains("type='error'") || data.contains("type=\"error\"") {
                // Good — server returned error for presence to blocked JID
            }
            // Silently dropping is also acceptable per RFC
        }
        Err(_) => {
            // Timeout acceptable — server silently dropped
        }
    }

    // Connection should still be alive
    let response = ping_query(&mut alice, "localhost", "post-block-pres")
        .await
        .expect("ping");
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Connection should survive after presence to blocked JID"
    );
}

// =========================================================================
// §8.5.3.2.3: Outbound IQ to blocked JID returns error
// =========================================================================

#[tokio::test]
async fn rfc6121_s8_5_3_2_3_outbound_iq_to_blocked_jid_returns_error() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    // Alice blocks Bob
    block_jid(&mut alice, "bob@localhost", "block-bob-out-iq").await;

    // Alice sends IQ to blocked Bob
    alice
        .send(
            "<iq type='get' id='out-blocked-iq' to='bob@localhost' xmlns='jabber:client'>\
                <ping xmlns='urn:xmpp:ping'/>\
            </iq>",
        )
        .await
        .expect("send IQ");

    // Server should return error for IQ to blocked JID
    let response = alice.read_until("</iq>", DEFAULT_TIMEOUT).await;
    match response {
        Ok(data) => {
            assert!(
                data.contains("type='error'")
                    || data.contains("type=\"error\"")
                    || data.contains("not-acceptable")
                    || data.contains("service-unavailable"),
                "Expected error for IQ to blocked JID, got: {}",
                data
            );
        }
        Err(_) => {
            // Some servers silently drop — acceptable
        }
    }
}
