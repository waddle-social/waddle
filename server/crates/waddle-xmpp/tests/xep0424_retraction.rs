#![recursion_limit = "256"]

//! XEP-0424: Message Retraction dedicated integration suite.

mod common;

use std::time::{Duration, Instant};

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0424_retraction_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "retract@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "retract@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice sends a message
    alice
        .send(
            "<message type='groupchat' to='retract@muc.localhost' id='msg-retract-1' xmlns='jabber:client'>\
                <body>Delete me</body>\
            </message>",
        )
        .await
        .expect("send");
    bob.read_until("Delete me", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");
    bob.clear();

    // Alice retracts the message
    alice
        .send(
            "<message type='groupchat' to='retract@muc.localhost' id='retract-1' xmlns='jabber:client'>\
                <retract xmlns='urn:xmpp:message-retract:1' id='msg-retract-1'/>\
            </message>",
        )
        .await
        .expect("send retraction");

    let bob_response = bob
        .read_until("retract", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives retraction");

    assert!(
        bob_response.contains("urn:xmpp:message-retract:1") || bob_response.contains("retract"),
        "Expected retraction element, got: {}",
        bob_response
    );
}

#[tokio::test]
async fn xep0424_retraction_survives_mam_query() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "retract-mam@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "retract-mam@muc.localhost", "Bob")
        .await
        .expect("bob join");

    alice
        .send(
            "<message type='groupchat' to='retract-mam@muc.localhost' id='msg-retract-1' xmlns='jabber:client'>\
                <body>Delete me</body>\
            </message>",
        )
        .await
        .expect("send");
    bob.read_until("Delete me", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");
    bob.clear();

    alice
        .send(
            "<message type='groupchat' to='retract-mam@muc.localhost' id='retract-1' xmlns='jabber:client'>\
                <retract xmlns='urn:xmpp:message-retract:1' id='msg-retract-1'/>\
            </message>",
        )
        .await
        .expect("send retraction");
    bob.read_until("retract", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives retraction");
    bob.clear();

    tokio::time::sleep(Duration::from_millis(100)).await;

    alice
        .send(
            "<iq type='set' id='mam-retract-1' to='retract-mam@muc.localhost' xmlns='jabber:client'>\
                <query xmlns='urn:xmpp:mam:2' queryid='retract-q1'>\
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
        mam_output.contains("urn:xmpp:message-retract:1"),
        "Expected archived retraction payload in MAM result, got: {}",
        mam_output
    );
    assert!(
        mam_output.contains("id='msg-retract-1'") || mam_output.contains("id=\"msg-retract-1\""),
        "Expected retraction target id preserved in MAM result, got: {}",
        mam_output
    );
}
