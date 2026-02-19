//! XEP-0157: Contact Addresses for XMPP Services dedicated suite.

mod common;

use common::{disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer};

#[tokio::test]
async fn xep0157_server_disco_info_includes_server_info_form() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0157", "client")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "localhost", "xep0157-disco")
        .await
        .expect("disco#info response");

    assert!(
        response.contains("jabber:x:data"),
        "Expected data form extension for XEP-0157, got: {}",
        response
    );
    assert!(
        response.contains("FORM_TYPE") && response.contains("urn:xmpp:serverinfo:0"),
        "Expected FORM_TYPE=urn:xmpp:serverinfo:0, got: {}",
        response
    );
    assert!(
        response.contains("abuse-addresses") && response.contains("mailto:abuse@localhost"),
        "Expected abuse contact address field, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0157_muc_service_disco_info_does_not_include_server_info_form() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0157neg", "client")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "muc.localhost", "xep0157-muc-disco")
        .await
        .expect("muc disco#info response");

    assert!(
        !response.contains("abuse-addresses"),
        "MUC disco#info must not expose server contact form, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0157_server_info_form_matches_feature_advertisement() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0157consistency", "client")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "localhost", "xep0157-consistency")
        .await
        .expect("disco#info response");

    let has_feature = response.contains("var='urn:xmpp:serverinfo:0'")
        || response.contains("var=\"urn:xmpp:serverinfo:0\"");
    let has_form_type =
        response.contains("FORM_TYPE") && response.contains("urn:xmpp:serverinfo:0");

    assert!(
        has_feature && has_form_type,
        "Expected serverinfo feature and matching form payload, got: {}",
        response
    );
}
