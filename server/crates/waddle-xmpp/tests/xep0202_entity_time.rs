#![recursion_limit = "256"]

//! XEP-0202: Entity Time dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0202_server_disco_advertises_time() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "time-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("urn:xmpp:time"),
        "Expected time feature, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0202_time_query_returns_utc_and_tzo() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='time-1' to='localhost' xmlns='jabber:client'>\
                <time xmlns='urn:xmpp:time'/>\
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
        response.contains("<utc>"),
        "Expected <utc> element, got: {}",
        response
    );
    assert!(
        response.contains("<tzo>"),
        "Expected <tzo> element, got: {}",
        response
    );
    assert!(
        response.contains("urn:xmpp:time"),
        "Expected time namespace, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0202_time_set_returns_error() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='set' id='time-bad-1' to='localhost' xmlns='jabber:client'>\
                <time xmlns='urn:xmpp:time'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected error for set on time, got: {}",
        response
    );
}
