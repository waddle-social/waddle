#![recursion_limit = "256"]

//! XEP-0513: Explicit Mentions integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, start_server_with_channels,
    RawXmppClient, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0513_explicit_mention_broadcast_in_muc() {
    init_test_env();
    let server = start_server_with_channels(&["mentions"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "mentions@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "mentions@muc.localhost", "bob")
        .await
        .expect("bob join");

    // Alice sends message with explicit mention of Bob
    alice
        .send(
            "<message type='groupchat' to='mentions@muc.localhost' id='emn-1' xmlns='jabber:client'>\
                <body>Hey Bob!</body>\
                <mentions xmlns='urn:xmpp:emn:0'>\
                    <mention type='jid' value='bob@localhost'/>\
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
    assert!(
        bob_response.contains("urn:xmpp:emn:0"),
        "Explicit mentions namespace should be preserved, got: {}",
        bob_response
    );
    assert!(
        (bob_response.contains("type='jid'") || bob_response.contains("type=\"jid\""))
            && (bob_response.contains("value='bob@localhost'")
                || bob_response.contains("value=\"bob@localhost\"")),
        "Explicit JID mention should be preserved, got: {}",
        bob_response
    );
}

#[tokio::test]
async fn xep0513_everyone_mention_broadcast() {
    init_test_env();
    let server = start_server_with_channels(&["everyone"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "everyone@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "everyone@muc.localhost", "bob")
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
    assert!(
        bob_response.contains("urn:xmpp:emn:0")
            && (bob_response.contains("type='everyone'")
                || bob_response.contains("type=\"everyone\"")),
        "@everyone mention should be preserved, got: {}",
        bob_response
    );
}
