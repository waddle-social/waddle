#![recursion_limit = "256"]

//! XEP-0308: Last Message Correction dedicated integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0308_correction_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "correct@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "correct@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice sends original message
    alice
        .send(
            "<message type='groupchat' to='correct@muc.localhost' id='orig-1' xmlns='jabber:client'>\
                <body>Helo world</body>\
            </message>",
        )
        .await
        .expect("send");
    bob.read_until("Helo world", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives original");
    bob.clear();

    // Alice sends correction
    alice
        .send(
            "<message type='groupchat' to='correct@muc.localhost' id='correct-1' xmlns='jabber:client'>\
                <body>Hello world</body>\
                <replace xmlns='urn:xmpp:message-correct:0' id='orig-1'/>\
            </message>",
        )
        .await
        .expect("send correction");

    let bob_response = bob
        .read_until("Hello world", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives correction");

    assert!(
        bob_response.contains("urn:xmpp:message-correct:0") || bob_response.contains("replace"),
        "Expected correction element, got: {}",
        bob_response
    );
}
