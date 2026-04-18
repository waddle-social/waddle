#![recursion_limit = "256"]

//! XEP-0428: Fallback Indication dedicated integration suite.

mod common;

use std::time::{Duration, Instant};

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room, RawXmppClient,
    TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0428_fallback_broadcast_preserves_body_range() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "fallback@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "fallback@muc.localhost", "Bob")
        .await
        .expect("bob join");

    bob.send(
        "<message type='groupchat' to='fallback@muc.localhost' id='msg-fallback-1' xmlns='jabber:client'>\
            <body>&gt; original quote\n\nmy reply</body>\
            <reply xmlns='urn:xmpp:reply:0' to='fallback@muc.localhost/Alice' id='parent-1'/>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0' end='18'/>\
            </fallback>\
        </message>",
    )
    .await
    .expect("send reply with fallback");

    let alice_response = alice
        .read_until("urn:xmpp:fallback:0", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives fallback");

    assert!(
        alice_response.contains("urn:xmpp:fallback:0"),
        "Expected fallback namespace preserved, got: {}",
        alice_response
    );
    assert!(
        alice_response.contains("for='urn:xmpp:reply:0'")
            || alice_response.contains("for=\"urn:xmpp:reply:0\""),
        "Expected fallback for=urn:xmpp:reply:0, got: {}",
        alice_response
    );
    assert!(
        alice_response.contains("start='0'") || alice_response.contains("start=\"0\""),
        "Expected fallback start attribute, got: {}",
        alice_response
    );
    assert!(
        alice_response.contains("end='18'") || alice_response.contains("end=\"18\""),
        "Expected fallback end attribute, got: {}",
        alice_response
    );
}

#[tokio::test]
async fn xep0428_fallback_survives_mam_query() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "fallback-mam@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "fallback-mam@muc.localhost", "Bob")
        .await
        .expect("bob join");

    bob.send(
        "<message type='groupchat' to='fallback-mam@muc.localhost' id='msg-fallback-1' xmlns='jabber:client'>\
            <body>&gt; parent\n\nreply</body>\
            <reply xmlns='urn:xmpp:reply:0' to='fallback-mam@muc.localhost/Alice' id='parent-1'/>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0' end='10'/>\
            </fallback>\
        </message>",
    )
    .await
    .expect("send reply with fallback");
    alice
        .read_until("urn:xmpp:fallback:0", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives fallback");
    alice.clear();

    tokio::time::sleep(Duration::from_millis(100)).await;

    alice
        .send(
            "<iq type='set' id='mam-fallback-1' to='fallback-mam@muc.localhost' xmlns='jabber:client'>\
                <query xmlns='urn:xmpp:mam:2' queryid='fallback-q1'>\
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
        mam_output.contains("urn:xmpp:fallback:0"),
        "Expected archived fallback payload in MAM result, got: {}",
        mam_output
    );
}

#[tokio::test]
async fn xep0428_fallback_feature_advertised_in_server_disco() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let response = disco_info_query(&mut alice, "localhost", "disco-fallback-1")
        .await
        .expect("disco info");

    assert!(
        response.contains("urn:xmpp:fallback:0"),
        "Expected fallback feature in disco#info, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0428_fallback_feature_advertised_in_muc_disco() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "fallback-disco@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let response = disco_info_query(
        &mut alice,
        "fallback-disco@muc.localhost",
        "disco-fallback-muc-1",
    )
    .await
    .expect("disco info");

    assert!(
        response.contains("urn:xmpp:fallback:0"),
        "Expected fallback feature in MUC disco#info, got: {}",
        response
    );
}
