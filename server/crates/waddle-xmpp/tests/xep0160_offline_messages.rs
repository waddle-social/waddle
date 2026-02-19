//! XEP-0160: Best Practices for Handling Offline Messages dedicated suite.

mod common;

use common::{disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer};

const OFFLINE_FEATURE: &str = "msgoffline";

#[tokio::test]
async fn xep0160_server_advertises_offline_message_feature() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0160", "client")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "localhost", "xep0160-disco")
        .await
        .expect("disco#info response");

    assert!(
        response.contains("var='msgoffline'") || response.contains("var=\"msgoffline\""),
        "Expected offline feature in server disco#info, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0160_offline_feature_not_advertised_on_muc_component() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0160neg", "client")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "muc.localhost", "xep0160-muc")
        .await
        .expect("muc disco#info response");

    assert!(
        !response.contains(OFFLINE_FEATURE),
        "MUC component should not advertise msgoffline, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0160_offline_feature_not_advertised_on_upload_component() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0160scope", "client")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "upload.localhost", "xep0160-upload")
        .await
        .expect("upload disco#info response");

    assert!(
        !response.contains(OFFLINE_FEATURE),
        "Upload component should not advertise msgoffline, got: {}",
        response
    );
}
