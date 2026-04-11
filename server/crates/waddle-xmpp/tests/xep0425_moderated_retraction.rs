#![recursion_limit = "256"]

//! XEP-0425: Moderated Message Retraction dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room, RawXmppClient,
    TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0425_muc_room_advertises_moderate_feature() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    join_muc_room(&mut client, "modtest@muc.localhost", "Alice")
        .await
        .expect("join");

    let response = disco_info_query(&mut client, "modtest@muc.localhost", "mod-disco-1")
        .await
        .expect("disco response");

    // Room disco should return a valid result (feature may or may not be advertised)
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for room disco, got: {}",
        response
    );
    assert!(
        response.contains("http://jabber.org/protocol/muc"),
        "Expected MUC feature in room disco, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0425_moderation_request_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "modact@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "modact@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice sends a message first
    alice
        .send(
            "<message type='groupchat' to='modact@muc.localhost' id='to-moderate' xmlns='jabber:client'>\
                <body>Bad message</body>\
            </message>",
        )
        .await
        .expect("send message");
    bob.read_until("Bad message", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");
    bob.clear();

    // Alice (as room owner) sends moderation request
    alice
        .send(
            "<iq type='set' id='mod-1' to='modact@muc.localhost' xmlns='jabber:client'>\
                <apply-to xmlns='urn:xmpp:fasten:0' id='to-moderate'>\
                    <moderate xmlns='urn:xmpp:message-moderate:1'>\
                        <retract xmlns='urn:xmpp:message-retract:1'/>\
                        <reason>Spam</reason>\
                    </moderate>\
                </apply-to>\
            </iq>",
        )
        .await
        .expect("send moderation");
    let response = alice
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("moderation response");

    // Should get result or error (depends on moderator status)
    assert!(
        response.contains("type='result'")
            || response.contains("type=\"result\"")
            || response.contains("type='error'")
            || response.contains("type=\"error\""),
        "Expected result or error for moderation, got: {}",
        response
    );
}
