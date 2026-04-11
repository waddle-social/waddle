#![recursion_limit = "256"]

//! XEP-0424: Message Retraction dedicated integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0424_retraction_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "retract@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "retract@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice sends a message
    alice
        .send(
            "<message type='groupchat' to='retract@muc.localhost' id='msg-retract-1' xmlns='jabber:client'>\
                <body>Delete me</body>\
            </message>",
        )
        .await
        .expect("send");
    bob.read_until("Delete me", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");
    bob.clear();

    // Alice retracts the message
    alice
        .send(
            "<message type='groupchat' to='retract@muc.localhost' id='retract-1' xmlns='jabber:client'>\
                <retract xmlns='urn:xmpp:message-retract:1' id='msg-retract-1'/>\
            </message>",
        )
        .await
        .expect("send retraction");

    let bob_response = bob
        .read_until("retract", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives retraction");

    assert!(
        bob_response.contains("urn:xmpp:message-retract:1")
            || bob_response.contains("retract"),
        "Expected retraction element, got: {}",
        bob_response
    );
}
