#![recursion_limit = "256"]

//! XEP-0363: HTTP File Upload dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0363_upload_service_advertised_in_disco_items() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='items-upload' to='localhost' xmlns='jabber:client'>\
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
        response.contains("upload.localhost"),
        "Expected upload.localhost in disco#items, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0363_upload_service_disco_info_has_feature() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "upload.localhost", "upload-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("urn:xmpp:http:upload:0"),
        "Expected http:upload feature, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0363_slot_request_returns_put_and_get_urls() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='upload-1' to='upload.localhost' xmlns='jabber:client'>\
                <request xmlns='urn:xmpp:http:upload:0' \
                    filename='photo.jpg' \
                    size='1024' \
                    content-type='image/jpeg'/>\
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
        response.contains("<slot"),
        "Expected <slot> element, got: {}",
        response
    );
    assert!(
        response.contains("<put"),
        "Expected <put> element with URL, got: {}",
        response
    );
    assert!(
        response.contains("<get"),
        "Expected <get> element with URL, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0363_slot_request_too_large_returns_error() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Mock limit is 10MB; request 100MB
    client
        .send(
            "<iq type='get' id='upload-big' to='upload.localhost' xmlns='jabber:client'>\
                <request xmlns='urn:xmpp:http:upload:0' \
                    filename='huge.zip' \
                    size='104857600' \
                    content-type='application/zip'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected error IQ for oversized file, got: {}",
        response
    );
}
