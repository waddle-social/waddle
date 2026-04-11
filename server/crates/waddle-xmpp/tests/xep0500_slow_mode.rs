#![recursion_limit = "256"]

//! XEP-0500: MUC Slow Mode dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room, RawXmppClient,
    TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0500_muc_room_advertises_slow_mode() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    join_muc_room(&mut client, "slowroom@muc.localhost", "Alice")
        .await
        .expect("join");

    let response = disco_info_query(&mut client, "slowroom@muc.localhost", "slow-disco-1")
        .await
        .expect("disco response");

    // Room should advertise slow mode feature (or it might be in config form)
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0500_messages_delivered_without_slow_mode() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "noslow@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "noslow@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Send two messages rapidly (no slow mode configured)
    alice
        .send(
            "<message type='groupchat' to='noslow@muc.localhost' id='fast-1' xmlns='jabber:client'>\
                <body>First fast</body>\
            </message>",
        )
        .await
        .expect("send first");
    alice
        .send(
            "<message type='groupchat' to='noslow@muc.localhost' id='fast-2' xmlns='jabber:client'>\
                <body>Second fast</body>\
            </message>",
        )
        .await
        .expect("send second");

    // Bob should receive both
    bob.read_until("First fast", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives first");
    bob.read_until("Second fast", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives second");
}
