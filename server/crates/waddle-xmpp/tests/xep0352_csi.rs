#![recursion_limit = "256"]

//! XEP-0352: Client State Indication dedicated integration suite.

mod common;

use common::{disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer};

#[tokio::test]
async fn xep0352_server_disco_advertises_csi() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "csi-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("urn:xmpp:csi:0"),
        "Expected CSI feature in disco#info, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0352_csi_inactive_active_accepted() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Send inactive indication
    client
        .send("<inactive xmlns='urn:xmpp:csi:0'/>")
        .await
        .expect("send inactive");

    // Give server time to process
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Send active indication
    client
        .send("<active xmlns='urn:xmpp:csi:0'/>")
        .await
        .expect("send active");

    // If we can still ping, the CSI nonzas were accepted
    let response = common::ping_query(&mut client, "localhost", "csi-ping-1")
        .await
        .expect("ping after CSI");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected ping result after CSI, got: {}",
        response
    );
}
