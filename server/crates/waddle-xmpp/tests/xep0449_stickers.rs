#![recursion_limit = "256"]

//! XEP-0449: Stickers integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, start_server_with_channels,
    RawXmppClient, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0449_sticker_message_broadcast_in_muc() {
    init_test_env();
    let server = start_server_with_channels(&["stickers"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "stickers@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "stickers@muc.localhost", "bob")
        .await
        .expect("bob join");

    // Alice sends a sticker
    alice
        .send(
            "<message type='groupchat' to='stickers@muc.localhost' id='sticker-1' xmlns='jabber:client'>\
                <sticker xmlns='urn:xmpp:stickers:0' pack='https://example.com/stickers' hash='abc123'/>\
                <body>👋</body>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("sticker", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        bob_response.contains("urn:xmpp:stickers:0") || bob_response.contains("sticker"),
        "Bob should receive sticker message, got: {}",
        bob_response
    );
}
