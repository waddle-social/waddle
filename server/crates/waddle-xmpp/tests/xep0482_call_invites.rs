#![recursion_limit = "256"]

//! XEP-0482: Call Invites integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0482_call_propose_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "calls@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "calls@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice proposes a call
    alice
        .send(
            "<message type='groupchat' to='calls@muc.localhost' id='call-1' xmlns='jabber:client'>\
                <propose xmlns='urn:xmpp:call-invites:0' id='session-1'>\
                    <audio/>\
                </propose>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("call-invites", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        bob_response.contains("urn:xmpp:call-invites:0"),
        "Bob should receive call invite namespace, got: {}",
        bob_response
    );
}
