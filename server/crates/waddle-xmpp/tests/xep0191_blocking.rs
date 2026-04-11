#![recursion_limit = "256"]

//! XEP-0191: Blocking Command dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0191_server_disco_advertises_blocking() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "block-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("urn:xmpp:blocking"),
        "Expected blocking feature, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0191_get_blocklist_returns_empty() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='blocklist-1' xmlns='jabber:client'>\
                <blocklist xmlns='urn:xmpp:blocking'/>\
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
        response.contains("urn:xmpp:blocking"),
        "Expected blocking namespace, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0191_block_jid_returns_success() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='set' id='block-1' xmlns='jabber:client'>\
                <block xmlns='urn:xmpp:blocking'>\
                    <item jid='spammer@example.com'/>\
                </block>\
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
        "Expected result IQ for block, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0191_unblock_jid_returns_success() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='set' id='unblock-1' xmlns='jabber:client'>\
                <unblock xmlns='urn:xmpp:blocking'>\
                    <item jid='friend@example.com'/>\
                </unblock>\
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
        "Expected result IQ for unblock, got: {}",
        response
    );
}
