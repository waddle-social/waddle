#![recursion_limit = "256"]

//! Integration tests for server-side link preview enrichment.
//!
//! Each test stands up a wiremock instance on loopback as the fake
//! origin, a `TestServer` as the XMPP server, and two clients (Alice
//! sender, Bob receiver). Alice sends a message with a URL pointing at
//! wiremock; Bob's inbound broadcast is asserted to include (or not
//! include) the server-injected `<reference><preview>` child.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Configure env so [`waddle_xmpp_xep_link_preview::LinkPreviewEnricher::from_env`]
/// (called inside `TestServer::start`) builds a permissive client that
/// can reach wiremock on loopback.
///
/// MUST be invoked *before* `TestServer::start`.
fn init_link_preview_env() {
    init_test_env();
    // SAFETY: tests in the same integration-test binary share env state;
    // every test in this file needs these flags identically set, so we
    // just re-assert them on each call.
    unsafe {
        std::env::set_var("WADDLE_LINK_PREVIEW_ALLOW_PRIVATE", "1");
        std::env::remove_var("WADDLE_LINK_PREVIEW_DISABLE");
    }
}

const OG_HTML_BODY: &str = "<html><head>\
    <meta property='og:title' content='Example Title'>\
    <meta property='og:description' content='Short summary'>\
    <meta property='og:image' content='https://cdn.example.com/og.png'>\
    <meta property='og:site_name' content='Example'>\
    <meta property='og:type' content='article'>\
</head><body>content</body></html>";

async fn mount_ok_html(wiremock: &MockServer) {
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(OG_HTML_BODY.as_bytes(), "text/html"),
        )
        .mount(wiremock)
        .await;
}

#[tokio::test]
async fn server_enriches_message_with_og_preview() {
    init_link_preview_env();

    let wiremock = MockServer::start().await;
    mount_ok_html(&wiremock).await;

    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect alice");
    establish_bound_session(&mut alice, &server, "alice", "web")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "preview@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect bob");
    establish_bound_session(&mut bob, &server, "bob", "web")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "preview@muc.localhost", "Bob")
        .await
        .expect("bob join");

    let target = format!("{}/article", wiremock.uri());
    let body = format!("look at {target} cool");
    alice
        .send(&format!(
            "<message type='groupchat' to='preview@muc.localhost' id='m1' xmlns='jabber:client'>\
                <body>{body}</body>\
            </message>"
        ))
        .await
        .expect("send");

    let received = bob
        .read_until("look at", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(received.contains(&body), "body preserved");
    assert!(
        received.contains("urn:waddle:link-preview:0"),
        "preview namespace present in broadcast: {received}"
    );
    assert!(
        received.contains("Example Title"),
        "og:title surfaced: {received}"
    );
    assert!(
        received.contains("urn:xmpp:reference:0"),
        "wrapping reference present"
    );
}

#[tokio::test]
async fn server_strips_client_authored_preview_before_fanout() {
    init_link_preview_env();

    let wiremock = MockServer::start().await;
    mount_ok_html(&wiremock).await;

    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect alice");
    establish_bound_session(&mut alice, &server, "alice", "web")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "strip@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect bob");
    establish_bound_session(&mut bob, &server, "bob", "web")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "strip@muc.localhost", "Bob")
        .await
        .expect("bob join");

    let target = format!("{}/article", wiremock.uri());
    let body = format!("forge {target}");
    alice
        .send(&format!(
            "<message type='groupchat' to='strip@muc.localhost' id='m2' xmlns='jabber:client'>\
                <body>{body}</body>\
                <reference xmlns='urn:xmpp:reference:0' type='data' begin='6' end='{}' uri='{target}'>\
                    <preview xmlns='urn:waddle:link-preview:0' url='{target}'><title>FORGED</title></preview>\
                </reference>\
            </message>",
            6 + target.chars().count(),
        ))
        .await
        .expect("send");

    let received = bob
        .read_until("forge", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        !received.contains("FORGED"),
        "forged preview must be stripped: {received}"
    );
    // And the server-generated preview from wiremock should take its place.
    assert!(
        received.contains("Example Title"),
        "server-generated preview present: {received}"
    );
}

#[tokio::test]
async fn no_preview_hint_skips_enrichment() {
    init_link_preview_env();

    let wiremock = MockServer::start().await;
    mount_ok_html(&wiremock).await;

    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect alice");
    establish_bound_session(&mut alice, &server, "alice", "web")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "nohint@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect bob");
    establish_bound_session(&mut bob, &server, "bob", "web")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "nohint@muc.localhost", "Bob")
        .await
        .expect("bob join");

    let target = format!("{}/article", wiremock.uri());
    let body = format!("quiet {target}");
    alice
        .send(&format!(
            "<message type='groupchat' to='nohint@muc.localhost' id='m3' xmlns='jabber:client'>\
                <body>{body}</body>\
                <no-preview xmlns='urn:waddle:link-preview:0'/>\
            </message>"
        ))
        .await
        .expect("send");

    let received = bob
        .read_until("quiet", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        !received.contains("Example Title"),
        "no-preview hint must suppress enrichment: {received}"
    );
}

#[tokio::test]
async fn messages_without_urls_are_untouched() {
    init_link_preview_env();

    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect alice");
    establish_bound_session(&mut alice, &server, "alice", "web")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "plain@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect bob");
    establish_bound_session(&mut bob, &server, "bob", "web")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "plain@muc.localhost", "Bob")
        .await
        .expect("bob join");

    alice
        .send(
            "<message type='groupchat' to='plain@muc.localhost' id='m4' xmlns='jabber:client'>\
                <body>just plain text</body>\
            </message>",
        )
        .await
        .expect("send");

    let received = bob
        .read_until("just plain text", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(!received.contains("urn:waddle:link-preview:0"));
}
