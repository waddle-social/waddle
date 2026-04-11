#![recursion_limit = "256"]

//! XEP-0049: Private XML Storage dedicated integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0049_store_private_xml_accepted() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Send private XML storage set
    client
        .send(
            "<iq type='set' id='priv-set-1' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:private'>\
                    <mydata xmlns='waddle:test:private'>hello</mydata>\
                </query>\
            </iq>",
        )
        .await
        .expect("send");

    // Verify connection still works after set (ping succeeds)
    let ping = common::ping_query(&mut client, "localhost", "post-priv-ping")
        .await
        .expect("ping after private set");
    assert!(
        ping.contains("type='result'") || ping.contains("type=\"result\""),
        "Expected ping result after private storage set, got: {}",
        ping
    );
}

#[tokio::test]
async fn xep0049_retrieve_private_xml_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='priv-get-1' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:private'>\
                    <mydata xmlns='waddle:test:private'/>\
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
        "Expected result IQ for private get, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0049_retrieve_nonexistent_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='priv-empty-1' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:private'>\
                    <nothing xmlns='waddle:test:nonexistent'/>\
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
        "Expected result IQ even for empty data, got: {}",
        response
    );
}
