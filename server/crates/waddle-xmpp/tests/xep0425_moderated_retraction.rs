#![recursion_limit = "256"]

//! XEP-0425: Moderated Message Retraction dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room,
    start_server_with_channels, RawXmppClient, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0425_muc_room_does_not_advertise_message_moderation() {
    init_test_env();
    let server = start_server_with_channels(&["modtest"]).await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    join_muc_room(&mut client, "modtest@muc.localhost", "alice")
        .await
        .expect("join");

    let response = disco_info_query(&mut client, "modtest@muc.localhost", "mod-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for room disco, got: {}",
        response
    );
    assert!(
        !response.contains("urn:xmpp:message-moderate:1"),
        "Room disco should not advertise XEP-0425 before runtime support exists, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0425_iq_moderation_request_returns_service_unavailable() {
    init_test_env();
    let server = start_server_with_channels(&["modact"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "modact@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "modact@muc.localhost", "bob")
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

    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected error for unsupported IQ moderation, got: {}",
        response
    );
    assert!(
        response.contains("service-unavailable"),
        "Expected service-unavailable for unsupported IQ moderation, got: {}",
        response
    );
}
