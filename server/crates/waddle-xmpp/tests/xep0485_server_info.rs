//! XEP-0485: PubSub Server Information compatibility suite.

mod common;

use common::{disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer};

const SERVER_INFO_FEATURE: &str = "urn:xmpp:serverinfo:0";

#[tokio::test]
async fn xep0485_server_root_disco_advertises_server_info_feature() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0485", "client")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "localhost", "xep0485-disco")
        .await
        .expect("disco#info response");

    assert!(
        response.contains("var='urn:xmpp:serverinfo:0'")
            || response.contains("var=\"urn:xmpp:serverinfo:0\""),
        "Expected serverinfo feature in server disco#info, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0485_server_info_feature_not_advertised_on_upload_component() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0485neg", "client")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "upload.localhost", "xep0485-upload-disco")
        .await
        .expect("upload disco#info response");

    assert!(
        !response.contains(SERVER_INFO_FEATURE),
        "Upload component should not advertise serverinfo feature, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0485_feature_advertisement_matches_server_info_form_payload() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0485consistency", "client")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "localhost", "xep0485-consistency")
        .await
        .expect("server disco#info response");

    let has_feature = response.contains("var='urn:xmpp:serverinfo:0'")
        || response.contains("var=\"urn:xmpp:serverinfo:0\"");
    let has_form = response.contains("FORM_TYPE") && response.contains("urn:xmpp:serverinfo:0");

    assert!(
        has_feature && has_form,
        "Expected serverinfo feature and matching data form payload, got: {}",
        response
    );
}
