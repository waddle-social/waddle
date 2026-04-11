#![recursion_limit = "256"]

//! XEP-0444: Message Reactions dedicated integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0444_reaction_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "react@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "react@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice sends a message
    alice
        .send(
            "<message type='groupchat' to='react@muc.localhost' id='msg-react-1' xmlns='jabber:client'>\
                <body>React to this!</body>\
            </message>",
        )
        .await
        .expect("send");
    bob.read_until("React to this!", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");
    bob.clear();

    // Bob sends a reaction
    bob.send(
        "<message type='groupchat' to='react@muc.localhost' id='reaction-1' xmlns='jabber:client'>\
            <reactions xmlns='urn:xmpp:reactions:0' id='msg-react-1'>\
                <reaction>👍</reaction>\
            </reactions>\
        </message>",
    )
    .await
    .expect("send reaction");

    // Alice should receive the reaction
    let alice_response = alice
        .read_until("reactions", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives reaction");

    assert!(
        alice_response.contains("urn:xmpp:reactions:0"),
        "Expected reactions namespace, got: {}",
        alice_response
    );
}
