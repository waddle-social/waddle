#![recursion_limit = "256"]

//! XEP-0461: Message Replies dedicated integration suite.

mod common;

use std::time::{Duration, Instant};

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room,
    start_server_with_channels, RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0461_reply_broadcast_in_muc() {
    init_test_env();
    let server = start_server_with_channels(&["reply"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "reply@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "reply@muc.localhost", "bob")
        .await
        .expect("bob join");

    alice
        .send(
            "<message type='groupchat' to='reply@muc.localhost' id='msg-reply-1' xmlns='jabber:client'>\
                <body>Original message</body>\
            </message>",
        )
        .await
        .expect("send original");
    bob.read_until("Original message", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives original");
    bob.clear();

    bob.send(
        "<message type='groupchat' to='reply@muc.localhost' id='reply-1' xmlns='jabber:client'>\
            <body>Responding to you</body>\
            <reply xmlns='urn:xmpp:reply:0' to='reply@muc.localhost/alice' id='msg-reply-1'/>\
        </message>",
    )
    .await
    .expect("send reply");

    let alice_response = alice
        .read_until("urn:xmpp:reply:0", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives reply");

    assert!(
        alice_response.contains("urn:xmpp:reply:0"),
        "Expected reply namespace in broadcast, got: {}",
        alice_response
    );
    assert!(
        alice_response.contains("id='msg-reply-1'")
            || alice_response.contains("id=\"msg-reply-1\""),
        "Expected reply target id preserved, got: {}",
        alice_response
    );
}

#[tokio::test]
async fn xep0461_reply_survives_mam_query() {
    init_test_env();
    let server = start_server_with_channels(&["reply-mam"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "reply-mam@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "reply-mam@muc.localhost", "bob")
        .await
        .expect("bob join");

    alice
        .send(
            "<message type='groupchat' to='reply-mam@muc.localhost' id='msg-reply-1' xmlns='jabber:client'>\
                <body>Original</body>\
            </message>",
        )
        .await
        .expect("send original");
    bob.read_until("Original", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives original");
    bob.clear();

    bob.send(
        "<message type='groupchat' to='reply-mam@muc.localhost' id='reply-1' xmlns='jabber:client'>\
            <body>Reply body</body>\
            <reply xmlns='urn:xmpp:reply:0' to='reply-mam@muc.localhost/alice' id='msg-reply-1'/>\
        </message>",
    )
    .await
    .expect("send reply");
    alice
        .read_until("urn:xmpp:reply:0", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives reply");
    alice.clear();

    tokio::time::sleep(Duration::from_millis(100)).await;

    alice
        .send(
            "<iq type='set' id='mam-reply-1' to='reply-mam@muc.localhost' xmlns='jabber:client'>\
                <query xmlns='urn:xmpp:mam:2' queryid='reply-q1'>\
                    <x xmlns='jabber:x:data' type='submit'>\
                        <field var='FORM_TYPE' type='hidden'><value>urn:xmpp:mam:2</value></field>\
                    </x>\
                </query>\
            </iq>",
        )
        .await
        .expect("send mam query");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut mam_output = String::new();
    let mut fin_received = false;
    while Instant::now() < deadline {
        if alice.read(Duration::from_millis(500)).await.is_ok() {
            let buffer = alice.take_buffer();
            mam_output.push_str(&buffer);
            if buffer.contains("<fin") && buffer.contains("urn:xmpp:mam:2") {
                fin_received = true;
                break;
            }
        }
    }

    assert!(fin_received, "Expected MAM fin, got: {}", mam_output);
    assert!(
        mam_output.contains("urn:xmpp:reply:0"),
        "Expected archived reply payload in MAM result, got: {}",
        mam_output
    );
    assert!(
        mam_output.contains("id='msg-reply-1'") || mam_output.contains("id=\"msg-reply-1\""),
        "Expected reply target id preserved in MAM result, got: {}",
        mam_output
    );
}

#[tokio::test]
async fn xep0461_reply_feature_advertised_in_server_disco() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let response = disco_info_query(&mut alice, "localhost", "disco-reply-1")
        .await
        .expect("disco info");

    assert!(
        response.contains("urn:xmpp:reply:0"),
        "Expected reply feature in disco#info, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0461_reply_feature_advertised_in_muc_disco() {
    init_test_env();
    let server = start_server_with_channels(&["reply-disco"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "reply-disco@muc.localhost", "alice")
        .await
        .expect("alice join");

    let response = disco_info_query(&mut alice, "reply-disco@muc.localhost", "disco-reply-muc-1")
        .await
        .expect("disco info");

    assert!(
        response.contains("urn:xmpp:reply:0"),
        "Expected reply feature in MUC disco#info, got: {}",
        response
    );
}
