//! XEP-0493 OAuth discovery over SASL OAUTHBEARER, over the real router.
#![cfg(feature = "test-support")]

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use minidom::Element;
use waddle_server::test_support::{websocket_state, WebSocketState};

mod oauth_ws_support;
use oauth_ws_support::{bearer, connect, oauth, receive, send, start_stream};

const NS_SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";

fn sasl_response(data: &[u8]) -> Element {
    Element::builder("response", NS_SASL)
        .append(BASE64_STANDARD.encode(data))
        .build()
}

/// The RFC 8414 metadata URL the server's configured base URL implies.
fn discovery_url_of(state: &WebSocketState) -> String {
    format!(
        "{}/.well-known/oauth-authorization-server",
        state.deps.auth_state.base_url.trim_end_matches('/')
    )
}

/// The JSON document of an RFC 7628 §3.2.2 error challenge.
fn challenge_json(challenge: &Element) -> serde_json::Value {
    assert_eq!(
        (challenge.name(), challenge.ns()),
        ("challenge", NS_SASL.to_owned())
    );
    let bytes = BASE64_STANDARD
        .decode(challenge.text().trim())
        .expect("base64 challenge");
    serde_json::from_slice(&bytes).expect("JSON error document")
}

#[tokio::test]
async fn empty_token_discovery_returns_the_discovery_document_url() {
    let state = websocket_state().await;
    let discovery_url = discovery_url_of(&state);
    let (mut peer, server) = connect(state).await;
    start_stream(&mut peer, None).await;

    send(&mut peer, oauth(&[])).await;
    let challenge = receive(&mut peer).await;
    let json = challenge_json(&challenge);
    assert_eq!(json["status"], "invalid_token");
    assert_eq!(json["openid-configuration"], discovery_url);
    assert_eq!(json["scope"], "xmpp:client:normal");

    // RFC 7628 §3.2.2: the client's dummy response ends the exchange.
    send(&mut peer, sasl_response(&[1])).await;
    let failure = receive(&mut peer).await;
    assert_eq!(
        (failure.name(), failure.ns()),
        ("failure", NS_SASL.to_owned())
    );
    assert!(failure.get_child("not-authorized", NS_SASL).is_some());

    // The same stream can try again after the failure.
    send(&mut peer, oauth(&bearer("", None))).await;
    assert_eq!(
        challenge_json(&receive(&mut peer).await)["openid-configuration"],
        discovery_url
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn invalid_token_failure_also_names_the_discovery_document() {
    let state = websocket_state().await;
    let discovery_url = discovery_url_of(&state);
    let (mut peer, server) = connect(state).await;
    start_stream(&mut peer, None).await;
    send(&mut peer, oauth(&bearer("not-a-session", None))).await;
    let json = challenge_json(&receive(&mut peer).await);
    assert_eq!(json["status"], "invalid_token");
    assert_eq!(json["openid-configuration"], discovery_url);
    send(&mut peer, sasl_response(&[1])).await;
    assert_eq!(receive(&mut peer).await.name(), "failure");
    server.abort();
    let _ = server.await;
}
