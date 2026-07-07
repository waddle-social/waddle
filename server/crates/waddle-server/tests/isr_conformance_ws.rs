//! ISR (`urn:xmpp:isr:0`) removal conformance tests (issue #1169).
//!
//! Waddle previously advertised `<isr xmlns='urn:xmpp:isr:0'/>` while
//! implementing only a bespoke `<token-request/>` IQ whose tokens were
//! never consumable — no `<inst-resume/>` handshake existed. Per the
//! XEP-conformance rule (official namespaces must match the official
//! wire shape exactly), the advertisement and the bespoke scheme were
//! removed. These tests pin the removal:
//! - server disco#info must not report `urn:xmpp:isr:0`
//! - the legacy `<token-request/>` IQ must get a plain
//!   `feature-not-implemented` error, never a token
//!
//! The stream-features counterpart (no `<isr/>` child post-auth) lives
//! in `websocket/tests/stream_features.rs` next to the feature builder.

use waddle_ws_test_support as ws_common;

use ws_common::{disco_info_query, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const ISR_NS: &str = "urn:xmpp:isr:0";

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("isr-conf-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection");
    (server, client)
}

#[tokio::test]
async fn server_disco_info_does_not_advertise_isr() {
    let (_server, mut client) = setup().await;

    let response = disco_info_query(&mut client, DOMAIN, "isr-disco")
        .await
        .expect("disco#info response");
    assert!(
        response.contains("type='result'"),
        "expected disco#info result, got: {response}"
    );
    assert!(
        !response.contains(ISR_NS),
        "server disco#info must not advertise urn:xmpp:isr:0, got: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn legacy_isr_token_request_iq_is_not_implemented() {
    let (_server, mut client) = setup().await;

    client
        .send(
            r#"<iq xmlns="jabber:client" type="get" id="isr-legacy-token"><token-request xmlns="urn:xmpp:isr:0"/></iq>"#,
        )
        .await
        .expect("send legacy ISR token request");
    let response = client
        .recv_matching(|frame| frame.contains("isr-legacy-token"))
        .await
        .expect("IQ response");
    assert!(
        response.contains("type='error'"),
        "expected IQ error for legacy ISR token request, got: {response}"
    );
    assert!(
        response.contains("feature-not-implemented"),
        "expected feature-not-implemented, got: {response}"
    );
    assert!(
        !response.contains("<token "),
        "no ISR token may be minted, got: {response}"
    );

    let _ = client.close().await;
}
