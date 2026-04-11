#![recursion_limit = "256"]

//! XEP-0352: Client State Indication dedicated integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0352_stream_features_advertise_csi() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    // Open stream
    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await
        .expect("send");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();

    // STARTTLS + auth to get post-auth features
    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .expect("send");
    client
        .read_until("<proceed", DEFAULT_TIMEOUT)
        .await
        .expect("proceed");
    client.clear();
    client
        .upgrade_tls(server.tls_connector(), &server.domain)
        .await
        .expect("tls");

    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await
        .expect("send");
    let features = client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");

    // CSI should be advertised as a stream feature
    assert!(
        features.contains("urn:xmpp:csi:0") || features.contains("csi"),
        "Expected CSI in stream features, got: {}",
        features
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
