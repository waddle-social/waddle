#![recursion_limit = "256"]
//! XEP-0493: OAuth Client Login compatibility suite.
//!
//! Tests the SASL OAUTHBEARER mechanism (RFC 7628) and discovery flow
//! as specified by XEP-0493 §2.3.1.
//!
//! Discovery wire format per RFC 7628 §3.2.2:
//! ```text
//! C: <auth mechanism='OAUTHBEARER'>base64(empty-token)</auth>
//! S: <challenge>base64({"status":"invalid_token","openid-configuration":"..."})</challenge>
//! C: <response>AQ==</response>
//! S: <failure><not-authorized/></failure>
//! ```

mod common;

use base64::prelude::*;
use common::{init_test_env, MockAppState, RawXmppClient, TestServer, DEFAULT_TIMEOUT};
use std::sync::Arc;

const SASL_NS: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
const OAUTH_DISCOVERY_URL: &str = "https://localhost/.well-known/oauth-authorization-server";

async fn connect_and_get_sasl_features(
    client: &mut RawXmppClient,
    server: &TestServer,
) -> std::io::Result<String> {
    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await?;
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await?;
    client.clear();

    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await?;
    client.read_until("<proceed", DEFAULT_TIMEOUT).await?;
    client.clear();

    client
        .upgrade_tls(server.tls_connector(), &server.domain)
        .await?;

    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await?;
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
}

async fn send_oauthbearer_auth(
    client: &mut RawXmppClient,
    raw_payload: &str,
) -> std::io::Result<()> {
    let encoded = BASE64_STANDARD.encode(raw_payload.as_bytes());
    client
        .send(&format!(
            "<auth xmlns='{}' mechanism='OAUTHBEARER'>{}</auth>",
            SASL_NS, encoded
        ))
        .await
}

/// Helper: send empty OAUTHBEARER, receive the RFC 7628 JSON challenge,
/// send the dummy \x01 response, then receive the SASL failure.
///
/// Returns the decoded JSON string from the challenge.
async fn perform_discovery_exchange(client: &mut RawXmppClient) -> std::io::Result<String> {
    // Server should send a <challenge> with base64-encoded JSON
    let challenge_xml = client.read_until("</challenge>", DEFAULT_TIMEOUT).await?;

    // Extract the base64 content from <challenge xmlns='...'>DATA</challenge>
    let b64_start = challenge_xml.find('>').map(|i| i + 1).unwrap_or(0);
    let b64_end = challenge_xml
        .find("</challenge>")
        .unwrap_or(challenge_xml.len());
    let b64_data = &challenge_xml[b64_start..b64_end];
    let json_bytes = BASE64_STANDARD
        .decode(b64_data.trim().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let json_str = String::from_utf8(json_bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    client.clear();

    // Send the RFC 7628 §3.2.3 dummy response (\x01)
    let dummy_response = BASE64_STANDARD.encode(b"\x01");
    client
        .send(&format!(
            "<response xmlns='{}'>{}</response>",
            SASL_NS, dummy_response
        ))
        .await?;

    // Server should then send <failure><not-authorized/></failure>
    let failure = client.read_until("</failure>", DEFAULT_TIMEOUT).await?;
    assert!(
        failure.contains("<not-authorized/>"),
        "Expected <not-authorized/> in failure, got: {}",
        failure
    );

    Ok(json_str)
}

#[tokio::test]
async fn xep0493_discovery_returns_rfc7628_json_challenge() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    let features = connect_and_get_sasl_features(&mut client, &server)
        .await
        .expect("sasl features");
    assert!(
        features.contains("<mechanism>OAUTHBEARER</mechanism>"),
        "Expected OAUTHBEARER mechanism advertisement, got: {}",
        features
    );

    client.clear();

    // Send empty OAUTHBEARER to trigger discovery
    send_oauthbearer_auth(&mut client, "n,,\x01\x01")
        .await
        .expect("send discovery auth");

    let json_str = perform_discovery_exchange(&mut client)
        .await
        .expect("discovery exchange");

    // Validate the JSON structure per RFC 7628 §3.2.2
    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).expect("challenge should be valid JSON");

    assert_eq!(
        parsed["status"], "invalid_token",
        "Expected status=invalid_token, got: {}",
        json_str
    );
    assert!(
        parsed["openid-configuration"]
            .as_str()
            .unwrap_or("")
            .contains("/.well-known/oauth-authorization-server"),
        "Expected openid-configuration URL in JSON, got: {}",
        json_str
    );
}

#[tokio::test]
async fn xep0493_oauthbearer_auth_succeeds_with_valid_token() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    connect_and_get_sasl_features(&mut client, &server)
        .await
        .expect("sasl features");
    client.clear();

    send_oauthbearer_auth(&mut client, "n,,\x01auth=Bearer valid-token-123\x01\x01")
        .await
        .expect("send oauth auth");
    let response = client
        .read_until("<success", DEFAULT_TIMEOUT)
        .await
        .expect("sasl success");

    assert!(
        response.contains("<success") && response.contains(SASL_NS),
        "Expected SASL success response, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0493_oauthbearer_auth_fails_with_invalid_token() {
    init_test_env();

    let server = TestServer::start_with_state(Arc::new(MockAppState::rejecting("localhost"))).await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    connect_and_get_sasl_features(&mut client, &server)
        .await
        .expect("sasl features");
    client.clear();

    send_oauthbearer_auth(&mut client, "n,,\x01auth=Bearer invalid-token-456\x01\x01")
        .await
        .expect("send oauth auth");
    let response = client
        .read_until("</failure>", DEFAULT_TIMEOUT)
        .await
        .expect("sasl failure");

    assert!(
        response.contains("<failure") && response.contains("<not-authorized/>"),
        "Expected not-authorized failure, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0493_feature_advertisement_matches_discovery_contract() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    let features = connect_and_get_sasl_features(&mut client, &server)
        .await
        .expect("sasl features");
    let advertises_oauthbearer = features.contains("<mechanism>OAUTHBEARER</mechanism>");

    client.clear();

    // Trigger discovery
    send_oauthbearer_auth(&mut client, "n,,\x01\x01")
        .await
        .expect("send discovery auth");
    let json_str = perform_discovery_exchange(&mut client)
        .await
        .expect("discovery exchange");
    let has_discovery_url = json_str.contains(OAUTH_DISCOVERY_URL);

    assert!(
        advertises_oauthbearer && has_discovery_url,
        "Expected OAUTHBEARER feature and matching discovery URL. features: {}, json: {}",
        features,
        json_str
    );
}

#[tokio::test]
async fn xep0493_discovery_json_contains_required_fields() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");

    connect_and_get_sasl_features(&mut client, &server)
        .await
        .expect("sasl features");
    client.clear();

    send_oauthbearer_auth(&mut client, "n,,\x01\x01")
        .await
        .expect("send discovery auth");
    let json_str = perform_discovery_exchange(&mut client)
        .await
        .expect("discovery exchange");

    let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("valid JSON");

    // RFC 7628 §3.2.2: "status" is REQUIRED
    assert!(
        parsed.get("status").is_some(),
        "Missing required 'status' field in discovery JSON"
    );

    // RFC 7628 §3.2.2: "openid-configuration" is OPTIONAL but XEP-0493 §4.2
    // says servers MUST provide it
    assert!(
        parsed.get("openid-configuration").is_some(),
        "Missing 'openid-configuration' field required by XEP-0493"
    );
}
