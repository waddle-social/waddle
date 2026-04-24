#![recursion_limit = "256"]

//! XEP-0372: References (@mentions) integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, start_server_with_channels,
    RawXmppClient, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0372_reference_mention_broadcast_in_muc() {
    init_test_env();
    let server = start_server_with_channels(&["refs"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "refs@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "refs@muc.localhost", "bob")
        .await
        .expect("bob join");

    // Alice mentions Bob
    alice
        .send(
            "<message type='groupchat' to='refs@muc.localhost' id='ref-1' xmlns='jabber:client'>\
                <body>Hey @Bob check this out</body>\
                <reference xmlns='urn:xmpp:reference:0' type='mention' begin='4' end='8' uri='xmpp:bob@localhost'/>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("Hey @Bob", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        bob_response.contains("Hey @Bob check this out"),
        "Bob should receive the message with mention"
    );
    // Reference element should be preserved
    assert!(
        bob_response.contains("urn:xmpp:reference:0") || bob_response.contains("reference"),
        "Reference should be preserved in broadcast, got: {}",
        bob_response
    );
}
