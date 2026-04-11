#![recursion_limit = "256"]

//! XEP-0077: In-Band Registration dedicated integration suite.

mod common;

use common::{init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT};

#[tokio::test]
async fn xep0077_registration_fields_available_before_auth() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    // Open stream
    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await
        .expect("send");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();

    // STARTTLS
    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .expect("send starttls");
    client
        .read_until("<proceed", DEFAULT_TIMEOUT)
        .await
        .expect("proceed");
    client.clear();
    client
        .upgrade_tls(server.tls_connector(), &server.domain)
        .await
        .expect("tls");

    // Post-TLS stream
    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await
        .expect("send");
    let features = client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");

    // Stream features should advertise registration
    assert!(
        features.contains("http://jabber.org/features/iq-register")
            || features.contains("register"),
        "Expected registration feature in stream features, got: {}",
        features
    );
    client.clear();

    // Query registration fields
    client
        .send(
            "<iq type='get' id='reg-1' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:register'/>\
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
        response.contains("jabber:iq:register"),
        "Expected register namespace, got: {}",
        response
    );
    assert!(
        response.contains("<username") || response.contains("username"),
        "Expected username field, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0077_register_new_account() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    // Open stream + STARTTLS
    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await
        .expect("send");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();

    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .expect("send starttls");
    client
        .read_until("<proceed", DEFAULT_TIMEOUT)
        .await
        .expect("proceed");
    client.clear();
    client
        .upgrade_tls(server.tls_connector(), &server.domain)
        .await
        .expect("tls");

    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await
        .expect("send");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();

    // Submit registration
    client
        .send(
            "<iq type='set' id='reg-2' xmlns='jabber:client'>\
                <query xmlns='jabber:iq:register'>\
                    <username>newuser</username>\
                    <password>secret123</password>\
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
        "Expected result IQ for registration, got: {}",
        response
    );
}
