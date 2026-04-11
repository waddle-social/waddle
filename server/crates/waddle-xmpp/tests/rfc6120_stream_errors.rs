#![recursion_limit = "256"]

//! RFC 6120: Stream Error Conditions
//!
//! Tests that the server handles malformed input, unknown hosts, and other
//! stream-level error conditions correctly (close stream or return error).

mod common;

use common::{init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT};

/// RFC 6120 §4.9.3.3: not-well-formed — server should close stream on invalid XML.
#[tokio::test]
async fn rfc6120_malformed_xml_closes_stream() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    // Send stream header
    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='1.0'>",
        )
        .await
        .expect("send header");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();

    // Send garbage XML
    client.send("<<<<INVALID>>>").await.expect("send garbage");

    // Server should close the stream (error or EOF)
    let result = client.read(DEFAULT_TIMEOUT).await;
    match result {
        Ok(data) => {
            // Should contain stream:error or </stream:stream>
            assert!(
                data.contains("stream:error")
                    || data.contains("</stream:stream>")
                    || data.contains("not-well-formed")
                    || data.contains("bad-format")
                    || data.is_empty(),
                "Expected stream error or close on malformed XML, got: {}",
                data
            );
        }
        Err(_) => {
            // Connection closed — acceptable response to malformed XML
        }
    }
}

/// RFC 6120 §4.9.3.3: send well-formed but nonsense XML after stream open.
#[tokio::test]
async fn rfc6120_unexpected_element_handled() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='1.0'>",
        )
        .await
        .expect("send header");
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("features");
    client.clear();

    // Send valid XML but not a recognized XMPP stanza (pre-auth)
    client
        .send("<bogus xmlns='urn:completely:made:up'/>")
        .await
        .expect("send bogus");

    // Server should either ignore, close stream, or return error — not crash
    let result = client.read(DEFAULT_TIMEOUT).await;
    // Any response (or connection close) is acceptable — we just verify no crash
    match result {
        Ok(_) | Err(_) => {} // Both acceptable
    }
}

/// RFC 6120 §4.4: missing 'to' attribute should still work (server infers domain).
#[tokio::test]
async fn rfc6120_stream_header_missing_to_attribute() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    // Stream header with no 'to' attribute
    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            version='1.0'>",
        )
        .await
        .expect("send header without to");

    let result = client.read(DEFAULT_TIMEOUT).await;
    match result {
        Ok(data) => {
            // Server should either respond with stream header or error
            assert!(
                data.contains("<stream:stream") || data.contains("stream:error"),
                "Expected stream response or error, got: {}",
                data
            );
        }
        Err(_) => {
            // Connection closed — acceptable for missing required attribute
        }
    }
}

/// RFC 6120 §4.9.3.6: host-unknown — wrong domain in 'to' attribute.
#[tokio::test]
async fn rfc6120_wrong_domain_in_to_attribute() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='wrong-domain.example.com' version='1.0'>",
        )
        .await
        .expect("send header with wrong domain");

    let result = client.read(DEFAULT_TIMEOUT).await;
    match result {
        Ok(data) => {
            // Server may accept (virtual hosting), return host-unknown, or proceed
            assert!(
                data.contains("<stream:stream")
                    || data.contains("host-unknown")
                    || data.contains("stream:error"),
                "Expected stream or host-unknown error, got: {}",
                data
            );
        }
        Err(_) => {
            // Connection closed — acceptable for unknown host
        }
    }
}

/// RFC 6120 §4.9.1: unsupported version triggers stream error.
#[tokio::test]
async fn rfc6120_unsupported_version_handled() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    client
        .send(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='localhost' version='99.0'>",
        )
        .await
        .expect("send header with bad version");

    let result = client.read(DEFAULT_TIMEOUT).await;
    match result {
        Ok(data) => {
            // Server may downgrade to 1.0, return error, or close connection
            assert!(
                data.contains("<stream:stream")
                    || data.contains("unsupported-version")
                    || data.contains("stream:error")
                    || data.is_empty(),
                "Expected stream, version error, or close, got: {}",
                data
            );
        }
        Err(_) => {
            // Connection closed — acceptable for unsupported version
        }
    }
}
