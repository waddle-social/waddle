#![recursion_limit = "256"]

//! XEP-0444: Message Reactions dedicated integration suite.

mod common;

use std::time::{Duration, Instant};

use common::{
    establish_bound_session, init_test_env, join_muc_room, start_server_with_channels,
    RawXmppClient, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0444_reaction_broadcast_in_muc() {
    init_test_env();
    let server = start_server_with_channels(&["react"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "react@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "react@muc.localhost", "bob")
        .await
        .expect("bob join");

    // Alice sends a message
    alice
        .send(
            "<message type='groupchat' to='react@muc.localhost' id='msg-react-1' xmlns='jabber:client'>\
                <body>React to this!</body>\
            </message>",
        )
        .await
        .expect("send");
    bob.read_until("React to this!", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");
    bob.clear();

    // Bob sends a reaction
    bob.send(
        "<message type='groupchat' to='react@muc.localhost' id='reaction-1' xmlns='jabber:client'>\
            <reactions xmlns='urn:xmpp:reactions:0' id='msg-react-1'>\
                <reaction>👍</reaction>\
            </reactions>\
        </message>",
    )
    .await
    .expect("send reaction");

    // Alice should receive the reaction
    let alice_response = alice
        .read_until("reactions", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives reaction");

    assert!(
        alice_response.contains("urn:xmpp:reactions:0"),
        "Expected reactions namespace, got: {}",
        alice_response
    );
}

#[tokio::test]
async fn xep0444_reaction_survives_mam_query() {
    init_test_env();
    let server = start_server_with_channels(&["react-mam"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "react-mam@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "react-mam@muc.localhost", "bob")
        .await
        .expect("bob join");

    alice
        .send(
            "<message type='groupchat' to='react-mam@muc.localhost' id='msg-react-1' xmlns='jabber:client'>\
                <body>React to this!</body>\
            </message>",
        )
        .await
        .expect("send");
    bob.read_until("React to this!", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");
    bob.clear();

    bob.send(
        "<message type='groupchat' to='react-mam@muc.localhost' id='reaction-1' xmlns='jabber:client'>\
            <reactions xmlns='urn:xmpp:reactions:0' id='msg-react-1'>\
                <reaction>👍</reaction>\
            </reactions>\
        </message>",
    )
    .await
    .expect("send reaction");
    alice
        .read_until("reactions", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives reaction");
    alice.clear();

    tokio::time::sleep(Duration::from_millis(100)).await;

    alice
        .send(
            "<iq type='set' id='mam-react-1' to='react-mam@muc.localhost' xmlns='jabber:client'>\
                <query xmlns='urn:xmpp:mam:2' queryid='react-q1'>\
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
        mam_output.contains("urn:xmpp:reactions:0"),
        "Expected archived reaction payload in MAM result, got: {}",
        mam_output
    );
    assert!(
        mam_output.contains("id='msg-react-1'") || mam_output.contains("id=\"msg-react-1\""),
        "Expected reaction target id preserved in MAM result, got: {}",
        mam_output
    );
}
