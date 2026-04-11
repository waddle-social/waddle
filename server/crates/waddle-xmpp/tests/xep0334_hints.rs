#![recursion_limit = "256"]

//! XEP-0334: Message Processing Hints integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0334_no_store_hint_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "hints@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "hints@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice sends message with no-store hint
    alice
        .send(
            "<message type='groupchat' to='hints@muc.localhost' id='hint-1' xmlns='jabber:client'>\
                <body>Ephemeral message</body>\
                <no-store xmlns='urn:xmpp:hints'/>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("Ephemeral message", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        bob_response.contains("Ephemeral message"),
        "Bob should receive the message, got: {}",
        bob_response
    );
}

#[tokio::test]
async fn xep0334_no_copy_hint_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "hints2@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "hints2@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice sends message with store hint
    alice
        .send(
            "<message type='groupchat' to='hints2@muc.localhost' id='hint-2' xmlns='jabber:client'>\
                <body>Store this one</body>\
                <store xmlns='urn:xmpp:hints'/>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("Store this one", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        bob_response.contains("Store this one"),
        "Bob should receive the message with store hint"
    );
}
