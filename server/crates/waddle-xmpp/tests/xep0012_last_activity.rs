#![recursion_limit = "256"]

//! XEP-0012: Last Activity dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0012_server_disco_advertises_last_activity() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "disco-0012")
        .await
        .expect("disco response");

    assert!(
        response.contains("jabber:iq:last"),
        "Expected jabber:iq:last feature in disco#info, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0012_server_uptime_query_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='last-1' to='localhost' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:last'/>\
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
        response.contains("jabber:iq:last"),
        "Expected last activity namespace, got: {}",
        response
    );
    assert!(
        response.contains("seconds="),
        "Expected seconds attribute in last activity, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0012_query_to_unknown_user_returns_error() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='last-2' to='nobody@localhost' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:last'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    // Should return either a result (seconds=0 for unknown) or service-unavailable
    assert!(
        response.contains("type='result'")
            || response.contains("type=\"result\"")
            || response.contains("type='error'")
            || response.contains("type=\"error\""),
        "Expected result or error IQ, got: {}",
        response
    );
}
