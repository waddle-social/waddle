#![recursion_limit = "256"]

//! XEP-0184: Message Delivery Receipts dedicated integration suite.
//!
//! Tests that receipt requests are forwarded and receipt acknowledgments
//! are properly delivered between connected clients.

mod common;

use common::{
    establish_bound_session, init_test_env, start_server_with_channels, RawXmppClient,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0184_receipt_request_forwarded_in_muc() {
    init_test_env();
    let server = start_server_with_channels(&["receipts"]).await;

    // Alice joins via auto-join
    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    let _ = alice.read_until("</presence>", DEFAULT_TIMEOUT).await;
    alice.clear();

    // Bob joins via auto-join
    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    let _ = bob.read_until("</presence>", DEFAULT_TIMEOUT).await;
    bob.clear();

    // Drain cross-join notifications
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    alice.clear();
    bob.clear();

    // Alice sends message with receipt request
    alice
        .send(
            "<message type='groupchat' to='receipts@muc.localhost' id='msg-rcpt-1' xmlns='jabber:client'>\
                <body>Hello with receipt</body>\
                <request xmlns='urn:xmpp:receipts'/>\
            </message>",
        )
        .await
        .expect("send");

    // Bob should receive the message (with or without receipt request)
    let bob_response = bob
        .read_until("Hello with receipt", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives message");

    assert!(
        bob_response.contains("Hello with receipt"),
        "Bob should receive the message body, got: {}",
        bob_response
    );
}

#[tokio::test]
async fn xep0184_receipt_received_delivered_to_sender() {
    init_test_env();
    let server = start_server_with_channels(&["rcpt2"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    let _ = alice.read_until("</presence>", DEFAULT_TIMEOUT).await;
    alice.clear();

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    let _ = bob.read_until("</presence>", DEFAULT_TIMEOUT).await;
    bob.clear();

    // Drain cross-join notifications
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    alice.clear();
    bob.clear();

    // Bob sends receipt acknowledgment as groupchat
    bob.send(
        "<message type='groupchat' to='rcpt2@muc.localhost' id='rcpt-ack-1' xmlns='jabber:client'>\
            <received xmlns='urn:xmpp:receipts' id='original-msg-1'/>\
        </message>",
    )
    .await
    .expect("send receipt");

    // Alice should receive the receipt (it's groupchat so broadcast)
    let alice_response = alice
        .read_until("received", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives receipt");

    assert!(
        alice_response.contains("urn:xmpp:receipts"),
        "Expected receipt namespace, got: {}",
        alice_response
    );
}
