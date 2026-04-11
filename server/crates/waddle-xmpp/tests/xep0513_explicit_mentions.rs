#![recursion_limit = "256"]

//! XEP-0513: Explicit Mentions integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0513_explicit_mention_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "mentions@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "mentions@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice sends message with explicit mention of Bob
    alice
        .send(
            "<message type='groupchat' to='mentions@muc.localhost' id='emn-1' xmlns='jabber:client'>\
                <body>Hey Bob!</body>\
                <mentions xmlns='urn:xmpp:emn:0'>\
                    <mention jid='bob@localhost'/>\
                </mentions>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("Hey Bob!", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        bob_response.contains("Hey Bob!"),
        "Bob should receive the message"
    );
}

#[tokio::test]
async fn xep0513_everyone_mention_broadcast() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "everyone@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "everyone@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice sends @everyone
    alice
        .send(
            "<message type='groupchat' to='everyone@muc.localhost' id='emn-2' xmlns='jabber:client'>\
                <body>Attention @everyone!</body>\
                <mentions xmlns='urn:xmpp:emn:0'>\
                    <mention type='everyone'/>\
                </mentions>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("Attention @everyone!", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        bob_response.contains("Attention @everyone!"),
        "Bob should receive the @everyone message"
    );
}
