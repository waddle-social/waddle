#![recursion_limit = "256"]

//! XEP-0202: Entity Time dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0202_time_query_to_server_returns_response() {
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
        response.contains("urn:xmpp:time"),
        "Expected entity time namespace, got: {}",
        response
    );
    assert!(
        response.contains("<tzo") && response.contains("<utc"),
        "Expected entity time payload, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0202_server_disco_advertises_entity_time() {
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
        "Expected entity time disco feature, got: {}",
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
    assert!(
        response.contains("service-unavailable"),
        "Expected service-unavailable for unsupported time set, got: {}",
        response
    );
}
