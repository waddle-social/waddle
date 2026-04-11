#![recursion_limit = "256"]

//! XEP-0433: Extended Channel Search dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room, RawXmppClient,
    TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0433_muc_service_advertises_channel_search() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "muc.localhost", "search-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("urn:xmpp:channel-search:0"),
        "Expected channel-search feature, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0433_search_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Create a room first by joining
    join_muc_room(&mut client, "searchable@muc.localhost", "Alice")
        .await
        .expect("join room");

    // Search for rooms
    client
        .send(
            "<iq type='get' id='search-1' to='muc.localhost' xmlns='jabber:client'>\
                <search xmlns='urn:xmpp:channel-search:0'>\
                    <q>searchable</q>\
                </search>\
            </iq>",
        )
        .await
        .expect("send search");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0433_empty_search_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='search-empty' to='muc.localhost' xmlns='jabber:client'>\
                <search xmlns='urn:xmpp:channel-search:0'>\
                    <q>nonexistentxyz123</q>\
                </search>\
            </iq>",
        )
        .await
        .expect("send search");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ even for empty search, got: {}",
        response
    );
}
