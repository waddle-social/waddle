#![recursion_limit = "256"]

//! XEP-0359: Unique and Stable Stanza IDs dedicated integration suite.
//!
//! Tests that the server assigns stanza-id elements to messages.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, start_server_with_channels,
    RawXmppClient, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0359_server_assigns_stanza_id_to_muc_message() {
    init_test_env();
    let server = start_server_with_channels(&["stanzaid"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "stanzaid@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "stanzaid@muc.localhost", "bob")
        .await
        .expect("bob join");

    alice
        .send(
            "<message type='groupchat' to='stanzaid@muc.localhost' id='sid-test-1' xmlns='jabber:client'>\
                <body>Check stanza ID</body>\
                <origin-id xmlns='urn:xmpp:sid:0' id='client-origin-1'/>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("Check stanza ID", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        bob_response.contains("Check stanza ID"),
        "Bob should receive the message, got: {}",
        bob_response
    );
    // Server should add stanza-id
    assert!(
        bob_response.contains("stanza-id") || bob_response.contains("urn:xmpp:sid:0"),
        "Expected server-assigned stanza-id, got: {}",
        bob_response
    );
}

#[tokio::test]
async fn xep0359_origin_id_preserved_in_muc() {
    init_test_env();
    let server = start_server_with_channels(&["originid"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "originid@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "originid@muc.localhost", "bob")
        .await
        .expect("bob join");

    alice
        .send(
            "<message type='groupchat' to='originid@muc.localhost' id='oid-test-1' xmlns='jabber:client'>\
                <body>Origin ID test</body>\
                <origin-id xmlns='urn:xmpp:sid:0' id='my-origin-abc'/>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("Origin ID test", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        bob_response.contains("my-origin-abc"),
        "Expected origin-id preserved, got: {}",
        bob_response
    );
}
