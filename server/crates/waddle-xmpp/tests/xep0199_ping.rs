#![recursion_limit = "256"]

//! XEP-0199: XMPP Ping dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, ping_query, RawXmppClient, TestServer,
};

#[tokio::test]
async fn xep0199_server_disco_advertises_ping() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "ping-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("urn:xmpp:ping"),
        "Expected ping feature, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0199_ping_to_server_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = ping_query(&mut client, "localhost", "ping-1")
        .await
        .expect("ping response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for ping, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0199_ping_to_own_bare_jid_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = ping_query(&mut client, "alice@localhost", "ping-bare-1")
        .await
        .expect("ping response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result for self-ping, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0199_ping_preserves_stanza_id() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = ping_query(&mut client, "localhost", "unique-ping-42")
        .await
        .expect("ping response");

    assert!(
        response.contains("unique-ping-42"),
        "Expected stanza id preserved in response, got: {}",
        response
    );
}
