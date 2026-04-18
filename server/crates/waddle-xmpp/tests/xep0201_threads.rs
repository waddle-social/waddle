#![recursion_limit = "256"]

//! XEP-0201: Best Practices for Message Threads dedicated integration suite.

mod common;

use std::time::{Duration, Instant};

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room, RawXmppClient,
    TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0201_thread_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "thread@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "thread@muc.localhost", "Bob")
        .await
        .expect("bob join");

    bob.send(
        "<message type='groupchat' to='thread@muc.localhost' id='thread-msg-1' xmlns='jabber:client'>\
            <body>Starting a thread</body>\
            <thread>root-thread-id</thread>\
        </message>",
    )
    .await
    .expect("send threaded message");

    let alice_response = alice
        .read_until("Starting a thread", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives thread");

    assert!(
        alice_response.contains("<thread"),
        "Expected thread element in broadcast, got: {}",
        alice_response
    );
    assert!(
        alice_response.contains("root-thread-id"),
        "Expected thread id in broadcast, got: {}",
        alice_response
    );
}

#[tokio::test]
async fn xep0201_thread_parent_round_trip_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "thread-parent@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "thread-parent@muc.localhost", "Bob")
        .await
        .expect("bob join");

    bob.send(
        "<message type='groupchat' to='thread-parent@muc.localhost' id='thread-child-1' xmlns='jabber:client'>\
            <body>Nested thread reply</body>\
            <thread parent='root-thread-id'>child-thread-id</thread>\
        </message>",
    )
    .await
    .expect("send nested thread");

    let alice_response = alice
        .read_until("Nested thread reply", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives nested thread");

    assert!(
        alice_response.contains("child-thread-id"),
        "Expected nested thread id in broadcast, got: {}",
        alice_response
    );
    assert!(
        alice_response.contains("parent='root-thread-id'")
            || alice_response.contains("parent=\"root-thread-id\""),
        "Expected thread parent attribute preserved, got: {}",
        alice_response
    );
}

#[tokio::test]
async fn xep0201_thread_survives_mam_query() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "thread-mam@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "thread-mam@muc.localhost", "Bob")
        .await
        .expect("bob join");

    bob.send(
        "<message type='groupchat' to='thread-mam@muc.localhost' id='thread-msg-1' xmlns='jabber:client'>\
            <body>Thread starter</body>\
            <thread parent='archive-root'>archive-child</thread>\
        </message>",
    )
    .await
    .expect("send threaded message");
    alice
        .read_until("Thread starter", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives thread");
    alice.clear();

    tokio::time::sleep(Duration::from_millis(100)).await;

    alice
        .send(
            "<iq type='set' id='mam-thread-1' to='thread-mam@muc.localhost' xmlns='jabber:client'>\
                <query xmlns='urn:xmpp:mam:2' queryid='thread-q1'>\
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
        mam_output.contains("archive-child"),
        "Expected archived thread id in MAM result, got: {}",
        mam_output
    );
    assert!(
        mam_output.contains("parent='archive-root'")
            || mam_output.contains("parent=\"archive-root\""),
        "Expected archived thread parent in MAM result, got: {}",
        mam_output
    );
}

#[tokio::test]
async fn xep0201_thread_parent_survives_muc_history_replay() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "thread-history@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "thread-history@muc.localhost", "Bob")
        .await
        .expect("bob join");

    bob.send(
        "<message type='groupchat' to='thread-history@muc.localhost' id='thread-history-1' xmlns='jabber:client'>\
            <body>Nested history reply</body>\
            <thread parent='history-root'>history-child</thread>\
        </message>",
    )
    .await
    .expect("send nested history thread");
    alice
        .read_until("Nested history reply", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives nested history thread");
    alice.clear();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut carol = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut carol, &server, "carol", "tablet")
        .await
        .expect("bind carol");
    carol
        .send(
            "<presence to='thread-history@muc.localhost/Carol' xmlns='jabber:client'>\
                <x xmlns='http://jabber.org/protocol/muc'>\
                    <history maxstanzas='10'/>\
                </x>\
            </presence>",
        )
        .await
        .expect("carol join with history");

    let history_response = carol
        .read_until("110", DEFAULT_TIMEOUT)
        .await
        .expect("carol receives join response");

    assert!(
        history_response.contains("Nested history reply"),
        "Expected archived body in MUC history, got: {}",
        history_response
    );
    assert!(
        history_response.contains("history-child"),
        "Expected archived thread id in MUC history, got: {}",
        history_response
    );
    assert!(
        history_response.contains("parent='history-root'")
            || history_response.contains("parent=\"history-root\""),
        "Expected archived thread parent in MUC history, got: {}",
        history_response
    );
    assert!(
        history_response.contains("urn:xmpp:delay"),
        "Expected replayed history message with delay stamp, got: {}",
        history_response
    );
}

#[tokio::test]
async fn xep0201_thread_feature_advertised_in_server_disco() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let response = disco_info_query(&mut alice, "localhost", "disco-threads-1")
        .await
        .expect("disco info");

    assert!(
        response.contains("urn:xmpp:threads:0"),
        "Expected threads feature in disco#info, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0201_thread_feature_advertised_in_muc_disco() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "thread-disco@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let response = disco_info_query(
        &mut alice,
        "thread-disco@muc.localhost",
        "disco-threads-muc-1",
    )
    .await
    .expect("disco info");

    assert!(
        response.contains("urn:xmpp:threads:0"),
        "Expected threads feature in MUC disco#info, got: {}",
        response
    );
}
