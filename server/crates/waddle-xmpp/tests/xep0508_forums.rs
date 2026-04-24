#![recursion_limit = "256"]

//! XEP-0508: Forums dedicated integration suite.

mod common;

use std::sync::Arc;

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room, MockAppState,
    RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};
use minidom::Element;

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

fn text_channel(id: &str, name: &str) -> waddle_xmpp::ChannelInfo {
    waddle_xmpp::ChannelInfo {
        id: id.to_string(),
        name: name.to_string(),
        channel_type: "text".to_string(),
    }
}

fn owner_form_field_value(iq_xml: &str, var: &str) -> Option<String> {
    let iq: Element = iq_xml.parse().ok()?;
    let query = iq.get_child("query", "http://jabber.org/protocol/muc#owner")?;
    let form = query.get_child("x", "jabber:x:data")?;
    let field = form
        .children()
        .find(|child| child.name() == "field" && child.attr("var") == Some(var))?;
    field
        .get_child("value", "jabber:x:data")
        .map(|value| value.text())
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
    join_muc_room(&mut alice, &room_jid.to_string(), "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, &room_jid.to_string(), "Bob")
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
    join_muc_room(&mut alice, &room_jid.to_string(), "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, &room_jid.to_string(), "Bob")
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

#[tokio::test]
async fn xep0508_owner_config_round_trips_forum_mode() {
    init_test_env();
    let state = Arc::new(MockAppState::new("localhost").with_space(
        forum_space(),
        vec![text_channel("forum-config", "Forum Config")],
    ));
    let server = TestServer::start_with_state(state).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "forum-config@muc.localhost", "Alice")
        .await
        .expect("alice join");

    alice
        .send(
            "<iq type='get' id='owner-get-1' to='forum-config@muc.localhost' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/muc#owner'/>\
            </iq>",
        )
        .await
        .expect("send owner get");
    let initial = alice
        .read_until("owner-get-1", DEFAULT_TIMEOUT)
        .await
        .expect("read owner get");
    alice.clear();
    assert!(
        initial.contains("muc#roomconfig_forum"),
        "Config form must include forum field, got: {}",
        initial
    );
    assert_eq!(
        owner_form_field_value(&initial, "muc#roomconfig_forum").as_deref(),
        Some("0"),
        "Instant rooms should default forum mode off, got: {}",
        initial
    );

    alice
        .send(
            "<iq type='set' id='owner-set-1' to='forum-config@muc.localhost' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/muc#owner'>\
                    <x xmlns='jabber:x:data' type='submit'>\
                        <field var='FORM_TYPE' type='hidden'><value>http://jabber.org/protocol/muc#roomconfig</value></field>\
                        <field var='muc#roomconfig_forum' type='boolean'><value>1</value></field>\
                    </x>\
                </query>\
            </iq>",
        )
        .await
        .expect("send owner set");
    alice
        .read_until("owner-set-1", DEFAULT_TIMEOUT)
        .await
        .expect("read owner set");
    alice.clear();

    alice
        .send(
            "<iq type='get' id='owner-get-2' to='forum-config@muc.localhost' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/muc#owner'/>\
            </iq>",
        )
        .await
        .expect("send owner get 2");
    let updated = alice
        .read_until("owner-get-2", DEFAULT_TIMEOUT)
        .await
        .expect("read owner get 2");
    alice.clear();
    assert!(
        updated.contains("muc#roomconfig_forum"),
        "Updated config form must still include forum field, got: {}",
        updated
    );
    assert_eq!(
        owner_form_field_value(&updated, "muc#roomconfig_forum").as_deref(),
        Some("1"),
        "Forum mode should round-trip through owner config, got: {}",
        updated
    );
}
