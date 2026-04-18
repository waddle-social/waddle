#![recursion_limit = "256"]

//! XEP-0452: MUC Mention Notifications integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0452_room_mentions_do_not_synthesize_notification_stanzas() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "mmn@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "mmn@muc.localhost", "Bob")
        .await
        .expect("bob join");

    alice.clear();
    bob.clear();

    alice
        .send(
            "<message type='groupchat' to='mmn@muc.localhost' id='mmn-1' xmlns='jabber:client'>\
                <body>Hey @Bob urgent!</body>\
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
        bob_response.contains("Hey @Bob urgent!"),
        "Bob should receive the room message with the mention"
    );
    assert!(
        !bob_response.contains("urn:xmpp:mmn:0"),
        "Room broadcast should not masquerade as an XEP-0452 notification, got: {}",
        bob_response
    );

    bob.clear();
    assert!(
        bob.read(std::time::Duration::from_millis(250))
            .await
            .is_err(),
        "Server should not generate an extra XEP-0452 notification stanza without runtime support"
    );
}
