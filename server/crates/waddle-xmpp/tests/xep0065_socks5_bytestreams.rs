//! XEP-0065: SOCKS5 Bytestreams compatibility suite.

mod common;

use common::{disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer};

const BYTESTREAMS_NS: &str = "http://jabber.org/protocol/bytestreams";

#[tokio::test]
async fn xep0065_server_advertises_bytestreams_feature() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0065", "client")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "localhost", "xep0065-disco")
        .await
        .expect("disco#info response");

    assert!(
        response.contains("var='http://jabber.org/protocol/bytestreams'")
            || response.contains("var=\"http://jabber.org/protocol/bytestreams\""),
        "Expected bytestreams feature in server disco#info, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0065_unimplemented_bytestream_query_returns_error() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0065neg", "client")
        .await
        .expect("bind session");

    client
        .send(
            "<iq type='get' id='xep0065-unsupported' to='localhost' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/bytestreams'/>\
            </iq>",
        )
        .await
        .expect("send bytestream query");

    let response = client
        .read_until("</iq>", common::DEFAULT_TIMEOUT)
        .await
        .expect("read error response");

    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected IQ error for unimplemented bytestream query, got: {}",
        response
    );
    assert!(
        response.contains("service-unavailable"),
        "Expected service-unavailable condition, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0065_bytestream_feature_not_advertised_on_upload_component() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0065scope", "client")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "upload.localhost", "xep0065-upload-disco")
        .await
        .expect("upload disco#info response");

    assert!(
        !response.contains(BYTESTREAMS_NS),
        "Upload component should not advertise bytestreams, got: {}",
        response
    );
}
