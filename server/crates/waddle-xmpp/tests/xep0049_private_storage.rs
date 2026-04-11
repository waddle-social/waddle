#![recursion_limit = "256"]

//! XEP-0049: Private XML Storage dedicated integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0049_store_and_retrieve_private_xml() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Store private XML
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
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for private set, got: {}",
        response
    );
    client.clear();

    // Retrieve private XML
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
async fn xep0049_retrieve_nonexistent_returns_empty() {
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
