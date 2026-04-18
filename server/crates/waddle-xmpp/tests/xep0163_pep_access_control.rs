#![recursion_limit = "256"]

//! XEP-0163: Personal Eventing Protocol access control dedicated suite.

mod common;

use std::sync::Arc;

use common::{establish_bound_session, init_test_env, MockAppState, RawXmppClient, TestServer, DEFAULT_TIMEOUT};
use waddle_xmpp::roster::{RosterItem, Subscription};

const MOOD_NODE: &str = "http://jabber.org/protocol/mood";
const OMEMO_NODE: &str = "eu.siacs.conversations.axolotl.devicelist";
const BOOKMARKS_NODE: &str = "urn:xmpp:bookmarks:1";

async fn read_iq_response(client: &mut RawXmppClient) -> std::io::Result<String> {
    let response = client.read_until("</iq>", DEFAULT_TIMEOUT).await?;
    client.clear();
    Ok(response)
}

async fn publish_item(
    client: &mut RawXmppClient,
    to: &str,
    id: &str,
    node: &str,
    payload_xml: &str,
) -> std::io::Result<String> {
    client
        .send(&format!(
            "<iq type='set' id='{id}' to='{to}' xmlns='jabber:client'>\
                <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                    <publish node='{node}'>\
                        <item id='{id}-item'>{payload_xml}</item>\
                    </publish>\
                </pubsub>\
            </iq>"
        ))
        .await?;
    read_iq_response(client).await
}

async fn query_items(
    client: &mut RawXmppClient,
    to: &str,
    id: &str,
    node: &str,
) -> std::io::Result<String> {
    client
        .send(&format!(
            "<iq type='get' id='{id}' to='{to}' xmlns='jabber:client'>\
                <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                    <items node='{node}'/>\
                </pubsub>\
            </iq>"
        ))
        .await?;
    read_iq_response(client).await
}

#[tokio::test]
async fn xep0163_cross_user_publish_is_forbidden() {
    init_test_env();
    let server = TestServer::start().await;
    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let response = publish_item(
        &mut alice,
        "bob@localhost",
        "pub-foreign-1",
        MOOD_NODE,
        "<mood xmlns='http://jabber.org/protocol/mood'><happy/></mood>",
    )
    .await
    .expect("publish response");

    assert!(
        response.contains("type=\"error\"") || response.contains("type='error'"),
        "Expected publish error, got: {response}"
    );
    assert!(
        response.contains("<forbidden") || response.contains(":forbidden"),
        "Expected forbidden publish error, got: {response}"
    );
}

#[tokio::test]
async fn xep0163_presence_node_requires_presence_subscription() {
    init_test_env();
    let server = TestServer::start().await;

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect bob");
    establish_bound_session(&mut bob, &server, "bob", "phone")
        .await
        .expect("bind bob");
    let publish = publish_item(
        &mut bob,
        "bob@localhost",
        "bob-mood-1",
        MOOD_NODE,
        "<mood xmlns='http://jabber.org/protocol/mood'><calm/></mood>",
    )
    .await
    .expect("publish response");
    assert!(
        publish.contains("type=\"result\"") || publish.contains("type='result'"),
        "Expected bob publish success, got: {publish}"
    );

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect alice");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let response = query_items(&mut alice, "bob@localhost", "items-foreign-1", MOOD_NODE)
        .await
        .expect("items response");

    assert!(
        response.contains("type=\"error\"") || response.contains("type='error'"),
        "Expected read error, got: {response}"
    );
    assert!(
        response.contains("<forbidden") || response.contains(":forbidden"),
        "Expected forbidden read error, got: {response}"
    );
}

#[tokio::test]
async fn xep0163_presence_node_allows_mutual_subscribers() {
    init_test_env();
    let state = Arc::new(
        MockAppState::new("localhost")
            .with_roster_item(
                "alice@localhost",
                RosterItem::new("bob@localhost".parse().expect("valid jid"))
                    .set_subscription(Subscription::Both),
            )
            .with_roster_item(
                "bob@localhost",
                RosterItem::new("alice@localhost".parse().expect("valid jid"))
                    .set_subscription(Subscription::Both),
            ),
    );
    let server = TestServer::start_with_state(state).await;

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect bob");
    establish_bound_session(&mut bob, &server, "bob", "phone")
        .await
        .expect("bind bob");
    let publish = publish_item(
        &mut bob,
        "bob@localhost",
        "bob-mood-2",
        MOOD_NODE,
        "<mood xmlns='http://jabber.org/protocol/mood'><happy/></mood>",
    )
    .await
    .expect("publish response");
    assert!(
        publish.contains("type=\"result\"") || publish.contains("type='result'"),
        "Expected bob publish success, got: {publish}"
    );

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect alice");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let response = query_items(&mut alice, "bob@localhost", "items-foreign-2", MOOD_NODE)
        .await
        .expect("items response");

    assert!(
        response.contains("type=\"result\"") || response.contains("type='result'"),
        "Expected read success, got: {response}"
    );
    assert!(
        response.contains("<mood xmlns=\"http://jabber.org/protocol/mood\"><happy/></mood>")
            || response.contains("<mood xmlns='http://jabber.org/protocol/mood'><happy/></mood>"),
        "Expected mood payload, got: {response}"
    );
}

#[tokio::test]
async fn xep0163_open_nodes_remain_readable_without_subscription() {
    init_test_env();
    let server = TestServer::start().await;

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect bob");
    establish_bound_session(&mut bob, &server, "bob", "phone")
        .await
        .expect("bind bob");
    let publish = publish_item(
        &mut bob,
        "bob@localhost",
        "bob-omemo-1",
        OMEMO_NODE,
        "<list xmlns='eu.siacs.conversations.axolotl'><device id='23'/></list>",
    )
    .await
    .expect("publish response");
    assert!(
        publish.contains("type=\"result\"") || publish.contains("type='result'"),
        "Expected bob publish success, got: {publish}"
    );

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect alice");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let response = query_items(&mut alice, "bob@localhost", "items-foreign-3", OMEMO_NODE)
        .await
        .expect("items response");

    assert!(
        response.contains("type=\"result\"") || response.contains("type='result'"),
        "Expected read success, got: {response}"
    );
    assert!(
        response.contains("device id=\"23\"") || response.contains("device id='23'"),
        "Expected OMEMO payload, got: {response}"
    );
}

#[tokio::test]
async fn xep0163_private_nodes_remain_owner_only() {
    init_test_env();
    let state = Arc::new(
        MockAppState::new("localhost")
            .with_roster_item(
                "alice@localhost",
                RosterItem::new("bob@localhost".parse().expect("valid jid"))
                    .set_subscription(Subscription::Both),
            )
            .with_roster_item(
                "bob@localhost",
                RosterItem::new("alice@localhost".parse().expect("valid jid"))
                    .set_subscription(Subscription::Both),
            ),
    );
    let server = TestServer::start_with_state(state).await;

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect bob");
    establish_bound_session(&mut bob, &server, "bob", "phone")
        .await
        .expect("bind bob");
    let publish = publish_item(
        &mut bob,
        "bob@localhost",
        "bob-bookmark-1",
        BOOKMARKS_NODE,
        "<conference xmlns='urn:xmpp:bookmarks:1' name='Secret'/>",
    )
    .await
    .expect("publish response");
    assert!(
        publish.contains("type=\"result\"") || publish.contains("type='result'"),
        "Expected bob publish success, got: {publish}"
    );

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect alice");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let response = query_items(
        &mut alice,
        "bob@localhost",
        "items-foreign-4",
        BOOKMARKS_NODE,
    )
    .await
    .expect("items response");

    assert!(
        response.contains("type=\"error\"") || response.contains("type='error'"),
        "Expected read error, got: {response}"
    );
    assert!(
        response.contains("<forbidden") || response.contains(":forbidden"),
        "Expected forbidden read error, got: {response}"
    );
}
