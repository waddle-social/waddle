#![recursion_limit = "256"]

//! XEP-0508: Forums dedicated integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0508_thread_create_broadcast_in_forum_room() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "forum@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "forum@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice creates a thread
    alice
        .send(
            "<message type='groupchat' to='forum@muc.localhost' id='thread-create-1' xmlns='jabber:client'>\
                <body>New discussion topic</body>\
                <thread-create xmlns='urn:xmpp:forums:0' title='Important Topic'/>\
            </message>",
        )
        .await
        .expect("send thread create");

    let bob_response = bob
        .read_until("New discussion topic", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives thread");

    assert!(
        bob_response.contains("New discussion topic"),
        "Bob should receive thread creation, got: {}",
        bob_response
    );
}

#[tokio::test]
async fn xep0508_thread_reply_broadcast_in_forum_room() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "forum2@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "forum2@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Bob replies to a thread
    bob.send(
        "<message type='groupchat' to='forum2@muc.localhost' id='thread-reply-1' xmlns='jabber:client'>\
            <body>My reply</body>\
            <thread-reply xmlns='urn:xmpp:forums:0' thread-id='thread-create-1'/>\
        </message>",
    )
    .await
    .expect("send thread reply");

    let alice_response = alice
        .read_until("My reply", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives reply");

    assert!(
        alice_response.contains("My reply"),
        "Alice should receive thread reply, got: {}",
        alice_response
    );
}
