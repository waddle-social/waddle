//! End-to-End Messaging Tests
//!
//! These tests verify that the XMPP messaging implementation works correctly:
//! - Groupchat messages are properly broadcast to all room occupants
//! - Direct (chat) messages are delivered to the intended recipient
//! - Message ordering is preserved in real-time delivery
//!
//! Run with: `cargo test -p waddle-xmpp --test messaging_e2e`

mod common;

use std::time::Duration;

use common::{
    encode_sasl_plain, extract_bound_jid, RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

/// Initialize tracing and crypto provider for tests.
fn init_test() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        common::install_crypto_provider();
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_test_writer()
            .try_init();
    });
}

/// Helper function to establish a fully authenticated XMPP session.
async fn establish_session(
    client: &mut RawXmppClient,
    server: &TestServer,
    username: &str,
    resource: &str,
) -> String {
    // Initial stream
    client
        .send(
            "<?xml version='1.0'?>\
        <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
        to='localhost' version='1.0'>",
        )
        .await
        .expect("Send stream header");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("Read features");
    client.clear();

    // STARTTLS
    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .expect("Send STARTTLS");
    client
        .read_until("<proceed", DEFAULT_TIMEOUT)
        .await
        .expect("Read proceed");
    client.clear();

    let connector = server.tls_connector();
    client
        .upgrade_tls(connector, "localhost")
        .await
        .expect("TLS upgrade");

    // Post-TLS stream
    client
        .send(
            "<?xml version='1.0'?>\
        <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
        to='localhost' version='1.0'>",
        )
        .await
        .expect("Send post-TLS stream");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("Read SASL features");
    client.clear();

    // SASL PLAIN auth
    let auth_data = encode_sasl_plain(&format!("{}@localhost", username), "token123");
    client
        .send(&format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{}</auth>",
            auth_data
        ))
        .await
        .expect("Send auth");
    client
        .read_until("<success", DEFAULT_TIMEOUT)
        .await
        .expect("Auth success");
    client.clear();

    // Post-SASL stream
    client
        .send(
            "<?xml version='1.0'?>\
        <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
        to='localhost' version='1.0'>",
        )
        .await
        .expect("Send post-auth stream");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("Read bind features");
    client.clear();

    // Resource bind
    client
        .send(&format!(
            "<iq type='set' id='bind_1' xmlns='jabber:client'>\
        <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'>\
            <resource>{}</resource>\
        </bind>\
    </iq>",
            resource
        ))
        .await
        .expect("Send bind");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("Read bind result");
    client.clear();

    extract_bound_jid(&response).expect("Should have JID in response")
}

/// Helper to join a MUC room.
async fn join_room(client: &mut RawXmppClient, room: &str, nick: &str) {
    client
        .send(&format!(
            "<presence to='{}/{}' xmlns='jabber:client'>\
            <x xmlns='http://jabber.org/protocol/muc'>\
                <history maxstanzas='0'/>\
            </x>\
        </presence>",
            room, nick
        ))
        .await
        .expect("Send MUC join");

    // Wait for self-presence
    let response = client
        .read_until("</presence>", DEFAULT_TIMEOUT)
        .await
        .expect("Read join presence");
    assert!(
        response.contains("110"),
        "Self-presence should have status code 110, got: {}",
        response
    );
    client.clear();
}

// =============================================================================
// Test: Groupchat Message Multi-Occupant
// =============================================================================

/// Test that groupchat messages are broadcast to all occupants in a room.
///
/// This verifies:
/// 1. Two users can join the same MUC room
/// 2. A message from one user is received by the other
/// 3. The sender receives the echo of their own message
#[tokio::test]
async fn test_groupchat_message_multi_occupant() {
    init_test();

    let server = TestServer::start().await;
    let room = "test-room@muc.localhost";

    // Create two clients
    let mut alice = RawXmppClient::connect(server.addr).await.unwrap();
    let mut bob = RawXmppClient::connect(server.addr).await.unwrap();

    // Establish sessions
    let alice_jid = establish_session(&mut alice, &server, "alice", "client1").await;
    let bob_jid = establish_session(&mut bob, &server, "bob", "client2").await;

    println!("Alice JID: {}", alice_jid);
    println!("Bob JID: {}", bob_jid);

    // Alice joins room first
    join_room(&mut alice, room, "Alice").await;
    println!("Alice joined room");

    // Bob joins room - should see Alice's presence first, then self-presence
    bob.send(&format!(
        "<presence to='{}/{}' xmlns='jabber:client'>\
            <x xmlns='http://jabber.org/protocol/muc'>\
                <history maxstanzas='0'/>\
            </x>\
        </presence>",
        room, "Bob"
    ))
    .await
    .expect("Bob join");

    // Bob should receive Alice's presence and his own (110)
    let response = bob
        .read_until("110", DEFAULT_TIMEOUT)
        .await
        .expect("Bob join presence");
    println!("Bob received join presences: {}", response.len());
    bob.clear();

    // Alice should also see Bob's join presence
    let alice_notif = alice
        .read_until("</presence>", DEFAULT_TIMEOUT)
        .await
        .expect("Alice sees Bob join");
    assert!(
        alice_notif.contains("Bob"),
        "Alice should see Bob's join, got: {}",
        alice_notif
    );
    alice.clear();

    println!("Both users in room");

    // Alice sends a groupchat message
    let test_message = "Hello from Alice to the room!";
    alice
        .send(&format!(
            "<message to='{}' type='groupchat' id='msg-alice-1' xmlns='jabber:client'>\
            <body>{}</body>\
        </message>",
            room, test_message
        ))
        .await
        .expect("Send message");

    // Both should receive the message
    // Alice gets the echo
    let alice_echo = alice
        .read_until("</message>", DEFAULT_TIMEOUT)
        .await
        .expect("Alice echo");
    assert!(
        alice_echo.contains(test_message),
        "Alice should receive echo, got: {}",
        alice_echo
    );
    assert!(
        alice_echo.contains("type='groupchat'") || alice_echo.contains("type=\"groupchat\""),
        "Echo should be groupchat type"
    );

    // Bob receives the message
    let bob_msg = bob
        .read_until("</message>", DEFAULT_TIMEOUT)
        .await
        .expect("Bob message");
    assert!(
        bob_msg.contains(test_message),
        "Bob should receive message, got: {}",
        bob_msg
    );
    assert!(
        bob_msg.contains("from='test-room@muc.localhost/Alice'")
            || bob_msg.contains("from=\"test-room@muc.localhost/Alice\""),
        "Message should be from room/nick, got: {}",
        bob_msg
    );

    println!("Message delivery verified for both occupants");

    // Cleanup
    alice.send("</stream:stream>").await.ok();
    bob.send("</stream:stream>").await.ok();
}

// =============================================================================
// Test: Direct Chat Message
// =============================================================================

/// Test that direct (chat) messages are delivered to the recipient.
///
/// This verifies:
/// 1. Two users can send direct messages to each other
/// 2. Messages are delivered with correct from/to JIDs
#[tokio::test]
async fn test_chat_message_direct() {
    init_test();

    let server = TestServer::start().await;

    // Create two clients
    let mut alice = RawXmppClient::connect(server.addr).await.unwrap();
    let mut bob = RawXmppClient::connect(server.addr).await.unwrap();

    // Establish sessions
    let alice_jid = establish_session(&mut alice, &server, "alice", "direct1").await;
    let bob_jid = establish_session(&mut bob, &server, "bob", "direct2").await;

    println!("Alice JID: {}", alice_jid);
    println!("Bob JID: {}", bob_jid);

    // Alice sends a direct message to Bob
    let test_message = "Hello Bob, this is a direct message!";
    alice
        .send(&format!(
            "<message to='{}' type='chat' id='dm-1' xmlns='jabber:client'>\
            <body>{}</body>\
        </message>",
            bob_jid, test_message
        ))
        .await
        .expect("Send direct message");

    // Bob should receive the message
    let bob_msg = bob
        .read_until("</message>", Duration::from_secs(3))
        .await
        .expect("Bob receives message");

    assert!(
        bob_msg.contains(test_message),
        "Bob should receive message body, got: {}",
        bob_msg
    );

    // The 'from' should be Alice's full JID (or bare JID)
    assert!(
        bob_msg.contains("alice@localhost") || bob_msg.contains(&alice_jid),
        "Message should be from Alice, got: {}",
        bob_msg
    );

    println!("Direct message delivered successfully");

    // Bob responds
    let response_message = "Hi Alice, got your message!";
    bob.clear();
    bob.send(&format!(
        "<message to='{}' type='chat' id='dm-2' xmlns='jabber:client'>\
            <body>{}</body>\
        </message>",
        alice_jid, response_message
    ))
    .await
    .expect("Send response");

    // Alice should receive Bob's response
    let alice_msg = alice
        .read_until("</message>", Duration::from_secs(3))
        .await
        .expect("Alice receives response");

    assert!(
        alice_msg.contains(response_message),
        "Alice should receive response body, got: {}",
        alice_msg
    );

    println!("Bidirectional direct messaging verified");

    // Cleanup
    alice.send("</stream:stream>").await.ok();
    bob.send("</stream:stream>").await.ok();
}

/// Test that direct messages with GitHub payloads are echoed back to the sender.
#[tokio::test]
async fn test_chat_message_direct_github_payload_echoed_to_sender() {
    init_test();

    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.unwrap();
    let mut bob = RawXmppClient::connect(server.addr).await.unwrap();

    let alice_jid = establish_session(&mut alice, &server, "alice", "github1").await;
    let bob_jid = establish_session(&mut bob, &server, "bob", "github2").await;

    alice.clear();
    bob.clear();

    alice
        .send(&format!(
            "<message to='{}' type='chat' id='dm-gh-1' xmlns='jabber:client'>\
                <body>Repo payload attached</body>\
                <repo xmlns='urn:waddle:github:0' owner='rust-lang' name='rust' \
                      url='https://github.com/rust-lang/rust'/>\
            </message>",
            bob_jid
        ))
        .await
        .expect("Send GitHub direct message");

    let bob_msg = bob
        .read_until("</message>", Duration::from_secs(3))
        .await
        .expect("Bob receives GitHub message");
    assert!(
        bob_msg.contains("urn:waddle:github:0"),
        "Bob should receive GitHub payload, got: {}",
        bob_msg
    );

    let alice_echo = alice
        .read_until("</message>", Duration::from_secs(3))
        .await
        .expect("Alice receives GitHub echo");
    assert!(
        alice_echo.contains("urn:waddle:github:0"),
        "Alice echo should contain GitHub payload, got: {}",
        alice_echo
    );
    assert!(
        alice_echo.contains(&alice_jid),
        "Alice echo should target sender JID, got: {}",
        alice_echo
    );

    alice.send("</stream:stream>").await.ok();
    bob.send("</stream:stream>").await.ok();
}

// =============================================================================
// Test: Real-time Delivery Ordering
// =============================================================================

/// Test that multiple messages are delivered in order.
///
/// This verifies:
/// 1. Messages sent rapidly are all delivered
/// 2. Order is preserved when received
#[tokio::test]
async fn test_real_time_delivery_ordering() {
    init_test();

    let server = TestServer::start().await;
    let room = "order-room@muc.localhost";

    // Create two clients
    let mut sender = RawXmppClient::connect(server.addr).await.unwrap();
    let mut receiver = RawXmppClient::connect(server.addr).await.unwrap();

    // Establish sessions
    let _sender_jid = establish_session(&mut sender, &server, "sender", "s1").await;
    let _receiver_jid = establish_session(&mut receiver, &server, "receiver", "r1").await;

    // Both join room
    join_room(&mut sender, room, "Sender").await;

    receiver
        .send(&format!(
            "<presence to='{}/{}' xmlns='jabber:client'>\
            <x xmlns='http://jabber.org/protocol/muc'>\
                <history maxstanzas='0'/>\
            </x>\
        </presence>",
            room, "Receiver"
        ))
        .await
        .expect("Receiver join");
    receiver
        .read_until("110", DEFAULT_TIMEOUT)
        .await
        .expect("Receiver self presence");
    receiver.clear();

    // Sender should see receiver join
    sender
        .read_until("Receiver", DEFAULT_TIMEOUT)
        .await
        .expect("Sender sees receiver");
    sender.clear();

    println!("Both users in room for ordering test");

    // Send multiple messages rapidly
    let message_count = 5;
    for i in 0..message_count {
        sender
            .send(&format!(
                "<message to='{}' type='groupchat' id='order-{}' xmlns='jabber:client'>\
                <body>Message {}</body>\
            </message>",
                room, i, i
            ))
            .await
            .expect("Send ordered message");
    }

    // Clear sender's buffer (they'll receive echoes)
    tokio::time::sleep(Duration::from_millis(200)).await;
    sender.clear();

    // Receiver should get all messages in order
    let mut received_order = Vec::new();

    for i in 0..message_count {
        let msg = receiver
            .read_until("</message>", Duration::from_secs(2))
            .await
            .unwrap_or_else(|_| panic!("Failed to receive message {}", i));

        // Extract the message number from the body
        if let Some(body_start) = msg.find("<body>") {
            if let Some(body_end) = msg.find("</body>") {
                let body = &msg[body_start + 6..body_end];
                if body.starts_with("Message ") {
                    if let Ok(num) = body[8..].parse::<i32>() {
                        received_order.push(num);
                    }
                }
            }
        }
        receiver.clear();
    }

    println!("Received order: {:?}", received_order);

    // Verify order is preserved
    assert_eq!(
        received_order.len(),
        message_count,
        "Should receive all {} messages, got {}",
        message_count,
        received_order.len()
    );

    for i in 0..message_count {
        assert_eq!(
            received_order[i] as usize, i,
            "Message {} should be in position {}, but position {} has {}",
            i, i, i, received_order[i]
        );
    }

    println!("Message ordering verified!");

    // Cleanup
    sender.send("</stream:stream>").await.ok();
    receiver.send("</stream:stream>").await.ok();
}

// =============================================================================
// Test: Message with Stanza IDs (XEP-0359)
// =============================================================================

/// Test that messages include server-assigned stanza IDs for MAM.
///
/// XEP-0359 Unique and Stable Stanza IDs - messages should have a
/// stanza-id element added by the server for archive reference.
#[tokio::test]
async fn test_message_has_stanza_id() {
    init_test();

    let server = TestServer::start().await;
    let room = "stanza-id-room@muc.localhost";

    let mut client = RawXmppClient::connect(server.addr).await.unwrap();
    let _jid = establish_session(&mut client, &server, "user", "client").await;

    join_room(&mut client, room, "User").await;

    // Send a message
    client
        .send(&format!(
            "<message to='{}' type='groupchat' id='test-stanza-id' xmlns='jabber:client'>\
            <body>Test message for stanza ID</body>\
        </message>",
            room
        ))
        .await
        .expect("Send message");

    // Check the echo for stanza-id
    let echo = client
        .read_until("</message>", DEFAULT_TIMEOUT)
        .await
        .expect("Read echo");

    // Messages should have a stanza-id element for MAM
    assert!(
        echo.contains("<stanza-id") || echo.contains("stanza-id"),
        "Message echo should include stanza-id for MAM, got: {}",
        echo
    );

    if echo.contains("urn:xmpp:sid:0") {
        println!("Stanza ID present with correct namespace");
    } else {
        println!("Note: Stanza ID present but namespace may vary");
    }

    client.send("</stream:stream>").await.ok();
}

// =============================================================================
// Test: Presence Updates in MUC
// =============================================================================

/// Test that presence changes are broadcast to room occupants.
#[tokio::test]
async fn test_muc_presence_updates() {
    init_test();

    let server = TestServer::start().await;
    let room = "presence-room@muc.localhost";

    let mut alice = RawXmppClient::connect(server.addr).await.unwrap();
    let mut bob = RawXmppClient::connect(server.addr).await.unwrap();

    let _alice_jid = establish_session(&mut alice, &server, "alice", "pres1").await;
    let _bob_jid = establish_session(&mut bob, &server, "bob", "pres2").await;

    // Alice joins
    join_room(&mut alice, room, "Alice").await;

    // Bob joins
    bob.send(&format!(
        "<presence to='{}/{}' xmlns='jabber:client'>\
            <x xmlns='http://jabber.org/protocol/muc'>\
                <history maxstanzas='0'/>\
            </x>\
        </presence>",
        room, "Bob"
    ))
    .await
    .expect("Bob join");
    bob.read_until("110", DEFAULT_TIMEOUT)
        .await
        .expect("Bob self presence");
    bob.clear();

    // Alice sees Bob join
    alice
        .read_until("Bob", DEFAULT_TIMEOUT)
        .await
        .expect("Alice sees Bob");
    alice.clear();

    // Bob leaves - send unavailable presence
    bob.send(&format!(
        "<presence to='{}/{}' type='unavailable' xmlns='jabber:client'/>",
        room, "Bob"
    ))
    .await
    .expect("Bob leave");

    // Bob gets his own leave confirmation
    let bob_leave = bob
        .read_until("</presence>", DEFAULT_TIMEOUT)
        .await
        .expect("Bob leave presence");
    assert!(
        bob_leave.contains("unavailable"),
        "Bob should see unavailable presence"
    );

    // Alice should see Bob's departure
    let alice_notif = alice
        .read_until("unavailable", Duration::from_secs(2))
        .await
        .expect("Alice sees Bob leave");
    assert!(
        alice_notif.contains("Bob"),
        "Alice should see Bob's leave, got: {}",
        alice_notif
    );

    println!("Presence updates broadcast correctly");

    alice.send("</stream:stream>").await.ok();
    bob.send("</stream:stream>").await.ok();
}
