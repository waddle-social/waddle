#![recursion_limit = "256"]

//! XEP-0433: Extended Channel Search dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room, RawXmppClient,
    TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0433_muc_service_does_not_advertise_channel_search() {
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
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
    assert!(
        !response.contains("urn:xmpp:channel-search:0"),
        "MUC service should not advertise XEP-0433 before runtime support exists, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0433_search_request_to_muc_returns_service_unavailable() {
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

    client
        .send(
            "<iq type='get' id='search-1' to='muc.localhost' xmlns='jabber:client'>\
                <search xmlns='urn:xmpp:channel-search:0'>\
                    <query>searchable</query>\
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
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected error IQ, got: {}",
        response
    );
    assert!(
        response.contains("service-unavailable"),
        "Expected service-unavailable for unsupported search, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0433_muc_disco_items_lists_rooms() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Create a room first
    join_muc_room(&mut client, "listed@muc.localhost", "Alice")
        .await
        .expect("join room");

    // disco#items on MUC service lists rooms
    client
        .send(
            "<iq type='get' id='muc-items-1' to='muc.localhost' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/disco#items'/>\
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
        response.contains("listed@muc.localhost"),
        "Expected created room in disco#items, got: {}",
        response
    );
}
