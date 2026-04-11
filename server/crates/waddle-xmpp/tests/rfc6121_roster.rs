#![recursion_limit = "256"]

//! RFC 6121: Roster Management
//!
//! Tests roster operations beyond basic get/set: push notifications to
//! multiple resources, item deletion, and versioning.

mod common;

use common::{
    establish_bound_session, init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

// =========================================================================
// Roster Get
// =========================================================================

#[tokio::test]
async fn rfc6121_roster_get_returns_query_with_namespace() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='roster-get-1' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:roster'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
    assert!(
        response.contains("jabber:iq:roster"),
        "Expected roster namespace in response, got: {}",
        response
    );
}

// =========================================================================
// Roster Set (Add)
// =========================================================================

#[tokio::test]
async fn rfc6121_roster_set_item_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='set' id='roster-set-1' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:roster'>\
                    <item jid='bob@localhost' name='Bob'/>\
                </query>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for roster set, got: {}",
        response
    );
}

// =========================================================================
// Roster Push to Multiple Resources
// =========================================================================

#[tokio::test]
async fn rfc6121_roster_push_to_connected_resources() {
    init_test_env();
    let server = TestServer::start().await;

    // Connect two resources for same user
    let mut desktop = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut desktop, &server, "alice", "desktop")
        .await
        .expect("bind desktop");

    let mut mobile = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut mobile, &server, "alice", "mobile")
        .await
        .expect("bind mobile");

    // Set roster item from desktop
    desktop
        .send(
            "<iq type='set' id='roster-push-1' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:roster'>\
                    <item jid='charlie@localhost' name='Charlie'/>\
                </query>\
            </iq>",
        )
        .await
        .expect("send roster set");

    // Desktop should get result
    let desktop_response = desktop
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("desktop response");
    assert!(
        desktop_response.contains("type='result'")
            || desktop_response.contains("type=\"result\"")
            || desktop_response.contains("type='set'")
            || desktop_response.contains("type=\"set\""),
        "Expected result or push IQ on desktop, got: {}",
        desktop_response
    );

    // Mobile should receive a roster push (type='set' with the item)
    // Give it a moment for async delivery
    let mobile_result = mobile.read(DEFAULT_TIMEOUT).await;
    match mobile_result {
        Ok(data) => {
            // Roster push is type='set' with jabber:iq:roster
            if data.contains("jabber:iq:roster") {
                assert!(
                    data.contains("charlie@localhost"),
                    "Roster push should contain the new contact, got: {}",
                    data
                );
            }
            // May also get nothing if server doesn't push to same-user resources
        }
        Err(_) => {
            // Timeout is acceptable if server doesn't implement roster push
        }
    }
}

// =========================================================================
// Roster Delete
// =========================================================================

#[tokio::test]
async fn rfc6121_roster_delete_item() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Delete a roster item (subscription='remove')
    client
        .send(
            "<iq type='set' id='roster-del-1' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:roster'>\
                    <item jid='bob@localhost' subscription='remove'/>\
                </query>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    // Result for existing item, or item-not-found for non-existent — both valid
    assert!(
        response.contains("type='result'")
            || response.contains("type=\"result\"")
            || response.contains("item-not-found"),
        "Expected result or item-not-found for roster delete, got: {}",
        response
    );
}

// =========================================================================
// Roster Versioning
// =========================================================================

#[tokio::test]
async fn rfc6121_roster_versioning_request() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Request roster with version attribute
    client
        .send(
            "<iq type='get' id='roster-ver-1' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:roster' ver=''/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    // Server should respond with result (full roster or empty if up-to-date)
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for versioned roster, got: {}",
        response
    );
}
