#![recursion_limit = "256"]

//! XEP-0030: Service Discovery dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

// =========================================================================
// disco#info on server domain
// =========================================================================

#[tokio::test]
async fn xep0030_server_disco_info_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "disco-info-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
    assert!(
        response.contains("http://jabber.org/protocol/disco#info"),
        "Expected disco#info feature, got: {}",
        response
    );
    assert!(
        response.contains("http://jabber.org/protocol/disco#items"),
        "Expected disco#items feature, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0030_server_disco_info_has_server_identity() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "disco-ident-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("category='server'") || response.contains("category=\"server\""),
        "Expected server category identity, got: {}",
        response
    );
}

// =========================================================================
// disco#items on server domain
// =========================================================================

#[tokio::test]
async fn xep0030_server_disco_items_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='items-1' to='localhost' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/disco#items'/>\
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
    // MUC component should be listed
    assert!(
        response.contains("muc.localhost"),
        "Expected muc.localhost in disco#items, got: {}",
        response
    );
}

// =========================================================================
// disco#info on MUC component
// =========================================================================

#[tokio::test]
async fn xep0030_muc_disco_info_returns_conference_identity() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "muc.localhost", "muc-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
    assert!(
        response.contains("category='conference'") || response.contains("category=\"conference\""),
        "Expected conference category in MUC disco, got: {}",
        response
    );
    assert!(
        response.contains("http://jabber.org/protocol/muc"),
        "Expected MUC feature, got: {}",
        response
    );
}

// =========================================================================
// disco#info to nonexistent component returns error
// =========================================================================

#[tokio::test]
async fn xep0030_disco_info_to_unknown_host_returns_service_unavailable() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "bogus.localhost", "bogus-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected error for unknown component, got: {}",
        response
    );
    assert!(
        response.contains("service-unavailable"),
        "Expected service-unavailable for unknown component, got: {}",
        response
    );
}

// =========================================================================
// Core XMPP features advertised
// =========================================================================

#[tokio::test]
async fn xep0030_server_advertises_core_features() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "core-features-1")
        .await
        .expect("disco response");

    // XEP-0199 Ping
    assert!(
        response.contains("urn:xmpp:ping"),
        "Expected ping feature, got: {}",
        response
    );
    // XEP-0012 Last Activity
    assert!(
        response.contains("jabber:iq:last"),
        "Expected last activity feature, got: {}",
        response
    );
    // XEP-0191 Blocking
    assert!(
        response.contains("urn:xmpp:blocking"),
        "Expected blocking feature, got: {}",
        response
    );
    // PubSub/PEP
    assert!(
        response.contains("http://jabber.org/protocol/pubsub"),
        "Expected pubsub feature, got: {}",
        response
    );
}
