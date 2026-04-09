#![recursion_limit = "256"]
//! XEP-0503: Server-side Spaces integration tests.
//!
//! Tests the spaces service at `spaces.<domain>` including discovery,
//! feature advertisement, metadata forms, and pubsub item retrieval.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, MockAppState, RawXmppClient,
    TestServer, DEFAULT_TIMEOUT,
};
use std::sync::Arc;

fn test_waddle() -> waddle_xmpp::WaddleDetails {
    waddle_xmpp::WaddleDetails {
        id: "waddle-1".to_string(),
        name: "Test Waddle".to_string(),
        description: Some("A test community".to_string()),
        owner_id: "alice".to_string(),
        icon_url: None,
        is_public: true,
        created_at: "2026-01-15T10:00:00Z".to_string(),
    }
}

fn test_channels() -> Vec<waddle_xmpp::ChannelInfo> {
    vec![
        waddle_xmpp::ChannelInfo {
            id: "general".to_string(),
            name: "General".to_string(),
            channel_type: "text".to_string(),
        },
        waddle_xmpp::ChannelInfo {
            id: "random".to_string(),
            name: "Random".to_string(),
            channel_type: "text".to_string(),
        },
    ]
}

/// Helper: send a disco#items query and read the IQ response.
async fn disco_items_query(
    client: &mut RawXmppClient,
    to: &str,
    id: &str,
    node: Option<&str>,
) -> std::io::Result<String> {
    let node_attr = match node {
        Some(n) => format!(" node='{}'", n),
        None => String::new(),
    };
    client
        .send(&format!(
            "<iq type='get' id='{}' to='{}' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/disco#items'{}/>\
            </iq>",
            id, to, node_attr
        ))
        .await?;
    client.read_until("</iq>", DEFAULT_TIMEOUT).await?;
    Ok(client.take_buffer())
}

/// Helper: send a disco#info query with a node attribute.
async fn disco_info_query_with_node(
    client: &mut RawXmppClient,
    to: &str,
    id: &str,
    node: &str,
) -> std::io::Result<String> {
    client
        .send(&format!(
            "<iq type='get' id='{}' to='{}' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/disco#info' node='{}'/>\
            </iq>",
            id, to, node
        ))
        .await?;
    client.read_until("</iq>", DEFAULT_TIMEOUT).await?;
    Ok(client.take_buffer())
}

/// Helper: send a pubsub items query and read the IQ response.
async fn pubsub_items_query(
    client: &mut RawXmppClient,
    to: &str,
    id: &str,
    node: &str,
) -> std::io::Result<String> {
    client
        .send(&format!(
            "<iq type='get' id='{}' to='{}' xmlns='jabber:client'>\
                <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                    <items node='{}'/>\
                </pubsub>\
            </iq>",
            id, to, node
        ))
        .await?;
    client.read_until("</iq>", DEFAULT_TIMEOUT).await?;
    Ok(client.take_buffer())
}

// =========================================================================
// Test: spaces.localhost advertised in server disco#items
// =========================================================================

#[tokio::test]
async fn xep0503_spaces_advertised_in_server_disco_items() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.unwrap();
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .unwrap();

    let response = disco_items_query(&mut client, "localhost", "disco-items-1", None)
        .await
        .unwrap();

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
    assert!(
        response.contains("spaces.localhost"),
        "Expected spaces.localhost in disco#items, got: {}",
        response
    );
}

// =========================================================================
// Test: spaces service disco#info returns correct identity and features
// =========================================================================

#[tokio::test]
async fn xep0503_spaces_service_disco_info() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.unwrap();
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .unwrap();

    let response = disco_info_query(&mut client, "spaces.localhost", "spaces-info-1")
        .await
        .unwrap();

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
    // Identity: pubsub/service
    assert!(
        response.contains("category='pubsub'") || response.contains("category=\"pubsub\""),
        "Expected pubsub category, got: {}",
        response
    );
    assert!(
        response.contains("type='service'") || response.contains("type=\"service\""),
        "Expected service type, got: {}",
        response
    );
    // Features
    assert!(
        response.contains("urn:xmpp:spaces:0"),
        "Expected spaces feature, got: {}",
        response
    );
    assert!(
        response.contains("http://jabber.org/protocol/pubsub"),
        "Expected pubsub feature, got: {}",
        response
    );
    assert!(
        response.contains("http://jabber.org/protocol/pubsub#retrieve-items"),
        "Expected retrieve-items feature, got: {}",
        response
    );
    assert!(
        response.contains("http://jabber.org/protocol/pubsub#subscribe"),
        "Expected subscribe feature (advertised per XEP-0503), got: {}",
        response
    );
    assert!(
        response.contains("http://jabber.org/protocol/pubsub#create-nodes"),
        "Expected create-nodes feature (advertised per XEP-0503), got: {}",
        response
    );
}

// =========================================================================
// Test: disco#items to spaces domain returns user's waddles
// =========================================================================

#[tokio::test]
async fn xep0503_spaces_disco_items_returns_user_waddles() {
    init_test_env();
    let state = Arc::new(
        MockAppState::new("localhost").with_waddle(test_waddle(), test_channels()),
    );
    let server = TestServer::start_with_state(state).await;
    let mut client = RawXmppClient::connect(server.addr).await.unwrap();
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .unwrap();

    let response = disco_items_query(&mut client, "spaces.localhost", "spaces-items-1", None)
        .await
        .unwrap();

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
    // Waddle should be listed as a disco item with node attribute
    assert!(
        response.contains("waddle-1"),
        "Expected waddle-1 node in response, got: {}",
        response
    );
    assert!(
        response.contains("Test Waddle"),
        "Expected waddle name in response, got: {}",
        response
    );
}

// =========================================================================
// Test: disco#info for a space node returns metadata form
// =========================================================================

#[tokio::test]
async fn xep0503_space_node_disco_info_returns_metadata() {
    init_test_env();
    let state = Arc::new(
        MockAppState::new("localhost").with_waddle(test_waddle(), test_channels()),
    );
    let server = TestServer::start_with_state(state).await;
    let mut client = RawXmppClient::connect(server.addr).await.unwrap();
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .unwrap();

    let response =
        disco_info_query_with_node(&mut client, "spaces.localhost", "space-node-1", "waddle-1")
            .await
            .unwrap();

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
    // Identity: pubsub/leaf
    assert!(
        response.contains("category='pubsub'") || response.contains("category=\"pubsub\""),
        "Expected pubsub category, got: {}",
        response
    );
    assert!(
        response.contains("type='leaf'") || response.contains("type=\"leaf\""),
        "Expected leaf type for space node, got: {}",
        response
    );
    // Metadata form with spaces type
    assert!(
        response.contains("urn:xmpp:spaces:0"),
        "Expected spaces namespace in metadata form, got: {}",
        response
    );
    assert!(
        response.contains("pubsub#type"),
        "Expected pubsub#type field in metadata form, got: {}",
        response
    );
    assert!(
        response.contains("pubsub#title"),
        "Expected pubsub#title field in metadata form, got: {}",
        response
    );
}

// =========================================================================
// Test: pubsub items returns bookmark items for channels
// =========================================================================

#[tokio::test]
async fn xep0503_pubsub_items_returns_bookmarks() {
    init_test_env();
    let state = Arc::new(
        MockAppState::new("localhost").with_waddle(test_waddle(), test_channels()),
    );
    let server = TestServer::start_with_state(state).await;
    let mut client = RawXmppClient::connect(server.addr).await.unwrap();
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .unwrap();

    let response =
        pubsub_items_query(&mut client, "spaces.localhost", "pubsub-items-1", "waddle-1")
            .await
            .unwrap();

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
    // Should contain bookmark items for the channels
    assert!(
        response.contains("general"),
        "Expected general channel item, got: {}",
        response
    );
    assert!(
        response.contains("random"),
        "Expected random channel item, got: {}",
        response
    );
    // Items should contain conference elements (XEP-0402 bookmarks)
    assert!(
        response.contains("conference"),
        "Expected conference bookmark element, got: {}",
        response
    );
}

// =========================================================================
// Test: disco#info for nonexistent space node returns item-not-found
// =========================================================================

#[tokio::test]
async fn xep0503_unknown_node_returns_not_found() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.unwrap();
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .unwrap();

    let response = disco_info_query_with_node(
        &mut client,
        "spaces.localhost",
        "space-node-404",
        "nonexistent",
    )
    .await
    .unwrap();

    assert!(
        response.contains("item-not-found"),
        "Expected item-not-found error, got: {}",
        response
    );
}

// =========================================================================
// Test: pubsub write operations return service-unavailable
// =========================================================================

#[tokio::test]
async fn xep0503_pubsub_write_returns_service_unavailable() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.unwrap();
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .unwrap();

    // Send a pubsub publish to spaces domain
    client
        .send(
            "<iq type='set' id='pub-1' to='spaces.localhost' xmlns='jabber:client'>\
                <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                    <publish node='test-space'>\
                        <item id='channel-1'/>\
                    </publish>\
                </pubsub>\
            </iq>",
        )
        .await
        .unwrap();

    client
        .read_until("service-unavailable", DEFAULT_TIMEOUT)
        .await
        .unwrap();
    let response = client.take_buffer();
    assert!(
        response.contains("service-unavailable"),
        "Expected service-unavailable error for write operation, got: {}",
        response
    );
}
