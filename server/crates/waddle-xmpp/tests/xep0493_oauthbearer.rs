//! XEP-0493: OAuth Client Login compatibility suite.

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

#[tokio::test]
async fn xep0493_discovery_request_returns_openid_configuration() {
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
    send_oauthbearer_auth(&mut client, "n,,\x01\x01")
        .await
        .expect("send discovery auth");
    let response = client
        .read_until("</failure>", DEFAULT_TIMEOUT)
        .await
        .expect("discovery response");

    assert!(
        response.contains("<openid-configuration>") && response.contains(OAUTH_DISCOVERY_URL),
        "Expected openid-configuration in discovery response, got: {}",
        response
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
    send_oauthbearer_auth(&mut client, "n,,\x01\x01")
        .await
        .expect("send discovery auth");
    let discovery = client
        .read_until("</failure>", DEFAULT_TIMEOUT)
        .await
        .expect("discovery response");
    let has_discovery_url = discovery.contains(OAUTH_DISCOVERY_URL);

    assert!(
        advertises_oauthbearer && has_discovery_url,
        "Expected OAUTHBEARER feature and matching discovery URL. features: {}, discovery: {}",
        features,
        discovery
    );
}
