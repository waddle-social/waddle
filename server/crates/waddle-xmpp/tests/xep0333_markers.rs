#![recursion_limit = "256"]

//! XEP-0333: Displayed Markers dedicated integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0333_markable_message_forwarded_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "markers@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "markers@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice sends markable message
    alice
        .send(
            "<message type='groupchat' to='markers@muc.localhost' id='mark-1' xmlns='jabber:client'>\
                <body>Read me</body>\
                <markable xmlns='urn:xmpp:chat-markers:0'/>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("Read me", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");
    assert!(
        bob_response.contains("Read me"),
        "Bob should receive markable message, got: {}",
        bob_response
    );
}

#[tokio::test]
async fn xep0333_displayed_marker_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "markers2@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "markers2@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Bob sends displayed marker
    bob.send(
        "<message type='groupchat' to='markers2@muc.localhost' id='displayed-1' xmlns='jabber:client'>\
            <displayed xmlns='urn:xmpp:chat-markers:0' id='some-msg-id'/>\
        </message>",
    )
    .await
    .expect("send");

    let alice_response = alice
        .read_until("displayed", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives marker");
    assert!(
        alice_response.contains("urn:xmpp:chat-markers:0"),
        "Expected chat-markers namespace, got: {}",
        alice_response
    );
}
