#![recursion_limit = "256"]

//! XEP-0092: Software Version dedicated integration suite.
//!
//! Verifies that the server both advertises `jabber:iq:version` in its
//! disco#info features and answers version IQ queries with a result IQ
//! carrying `<name/>` and `<version/>` children (with an optional `<os/>`).

mod common;

use common::{disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer};

/// Send an XEP-0092 software version query and read a single IQ response.
async fn version_query(client: &mut RawXmppClient, to: &str, id: &str) -> std::io::Result<String> {
    client
        .send(&format!(
            "<iq type='get' id='{}' to='{}' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:version'/>\
            </iq>",
            id, to
        ))
        .await?;
    client
        .read_until(id, std::time::Duration::from_secs(5))
        .await
}

#[tokio::test]
async fn xep0092_server_disco_advertises_software_version() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "version-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("jabber:iq:version"),
        "Expected software version feature, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0092_version_query_returns_result_with_name_and_version() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = version_query(&mut client, "localhost", "version-1")
        .await
        .expect("version response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for version, got: {}",
        response
    );
    assert!(
        response.contains("<name"),
        "Expected <name/> child in version response, got: {}",
        response
    );
    assert!(
        response.contains("<version"),
        "Expected <version/> child in version response, got: {}",
        response
    );
    assert!(
        response.contains("Waddle"),
        "Expected server name 'Waddle', got: {}",
        response
    );
}

#[tokio::test]
async fn xep0092_version_query_preserves_stanza_id() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = version_query(&mut client, "localhost", "unique-version-42")
        .await
        .expect("version response");

    assert!(
        response.contains("unique-version-42"),
        "Expected stanza id preserved in response, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0092_version_response_includes_os_element() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = version_query(&mut client, "localhost", "version-os-1")
        .await
        .expect("version response");

    assert!(
        response.contains("<os"),
        "Expected <os/> child in version response, got: {}",
        response
    );
}
