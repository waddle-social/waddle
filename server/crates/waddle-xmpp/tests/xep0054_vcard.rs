#![recursion_limit = "256"]

//! XEP-0054: vcard-temp dedicated integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0054_get_own_vcard_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='vcard-1' xmlns='jabber:client'>\
                <vCard xmlns='vcard-temp'/>\
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
}

#[tokio::test]
async fn xep0054_set_vcard_accepted() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='set' id='vcard-set-1' xmlns='jabber:client'>\
                <vCard xmlns='vcard-temp'>\
                    <FN>Alice Wonderland</FN>\
                    <NICKNAME>alice</NICKNAME>\
                </vCard>\
            </iq>",
        )
        .await
        .expect("send");

    // Verify connection still works after vCard set
    let ping = common::ping_query(&mut client, "localhost", "post-vcard-ping")
        .await
        .expect("ping after vCard set");
    assert!(
        ping.contains("type='result'") || ping.contains("type=\"result\""),
        "Expected ping result after vCard set, got: {}",
        ping
    );
}

#[tokio::test]
async fn xep0054_get_other_user_vcard() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='vcard-other-1' to='bob@localhost' xmlns='jabber:client'>\
                <vCard xmlns='vcard-temp'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    // Should get result (empty vCard) or error, not a crash
    assert!(
        response.contains("type='result'")
            || response.contains("type=\"result\"")
            || response.contains("type='error'")
            || response.contains("type=\"error\""),
        "Expected result or error IQ, got: {}",
        response
    );
}
