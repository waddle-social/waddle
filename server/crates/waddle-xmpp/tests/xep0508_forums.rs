#![recursion_limit = "256"]

//! XEP-0508: Forums dedicated integration suite.

mod common;

use std::sync::Arc;

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room, MockAppState,
    RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

fn forum_space() -> waddle_xmpp::SpaceDetails {
    waddle_xmpp::SpaceDetails {
        id: "space-forums".to_string(),
        name: "Forum Space".to_string(),
        description: Some("Forum tests".to_string()),
        owner_id: "owner".to_string(),
        icon_url: None,
        is_public: true,
        created_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

fn forum_channel() -> waddle_xmpp::ChannelInfo {
    waddle_xmpp::ChannelInfo {
        id: "forum-channel".to_string(),
        name: "Announcements".to_string(),
        channel_type: "forum".to_string(),
    }
}

fn forum_channel_two() -> waddle_xmpp::ChannelInfo {
    waddle_xmpp::ChannelInfo {
        id: "forum-channel-2".to_string(),
        name: "Q&A".to_string(),
        channel_type: "forum".to_string(),
    }
}

#[tokio::test]
async fn xep0508_thread_create_broadcast_in_forum_room() {
    init_test_env();
    let state = Arc::new(
        MockAppState::new("localhost")
            .with_space(forum_space(), vec![forum_channel(), forum_channel_two()]),
    );
    let server = TestServer::start_with_state(state).await;
    let room_jid = waddle_xmpp::managed_room_jid("forum-channel", "muc.localhost")
        .expect("canonical room jid");

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, &room_jid.to_string(), "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, &room_jid.to_string(), "bob")
        .await
        .expect("bob join");

    // Alice creates a thread
    alice
        .send(&format!(
            "<message type='groupchat' to='{}' id='thread-create-1' xmlns='jabber:client'>\
                <body>New discussion topic</body>\
                <thread-create xmlns='urn:xmpp:forums:0' title='Important Topic'/>\
            </message>",
            room_jid
        ))
        .await
        .expect("send thread create");

    let bob_response = bob
        .read_until("New discussion topic", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives thread");

    assert!(
        bob_response.contains("New discussion topic"),
        "Bob should receive thread creation, got: {}",
        bob_response
    );
}

#[tokio::test]
async fn xep0508_thread_reply_broadcast_in_forum_room() {
    init_test_env();
    let state = Arc::new(
        MockAppState::new("localhost")
            .with_space(forum_space(), vec![forum_channel(), forum_channel_two()]),
    );
    let server = TestServer::start_with_state(state).await;
    let room_jid = waddle_xmpp::managed_room_jid("forum-channel-2", "muc.localhost")
        .expect("canonical room jid");

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, &room_jid.to_string(), "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, &room_jid.to_string(), "bob")
        .await
        .expect("bob join");

    // Bob replies to a thread
    bob.send(&format!(
        "<message type='groupchat' to='{}' id='thread-reply-1' xmlns='jabber:client'>\
            <body>My reply</body>\
            <thread-reply xmlns='urn:xmpp:forums:0' thread-id='thread-create-1'/>\
        </message>",
        room_jid
    ))
    .await
    .expect("send thread reply");

    let alice_response = alice
        .read_until("My reply", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives reply");

    assert!(
        alice_response.contains("My reply"),
        "Alice should receive thread reply, got: {}",
        alice_response
    );
}

#[tokio::test]
async fn xep0508_channel_backed_forum_room_advertises_forum_feature() {
    init_test_env();
    let state =
        Arc::new(MockAppState::new("localhost").with_space(forum_space(), vec![forum_channel()]));
    let server = TestServer::start_with_state(state).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let room_jid = waddle_xmpp::managed_room_jid("forum-channel", "muc.localhost")
        .expect("canonical room jid");
    let response = disco_info_query(&mut alice, &room_jid.to_string(), "forum-disco-1")
        .await
        .expect("disco#info");

    assert!(
        response.contains("urn:xmpp:forums:0"),
        "Forum rooms must advertise XEP-0508, got: {}",
        response
    );
    assert!(
        response.contains("Announcements"),
        "Forum room should use stored channel metadata, got: {}",
        response
    );
}
