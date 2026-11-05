//! Dark XEP-0388 OAUTHBEARER framing over the production WebSocket router.
#![cfg(feature = "test-support")]

use axum::serve::Listener;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use futures::{SinkExt, StreamExt};
use minidom::Element;
use std::future;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use waddle_server::test_support::{
    create_test_session, router, websocket_state, websocket_state_with_sasl2, WebSocketState,
};
use waddle_xmpp_core::xep0388::{
    Sasl2Authenticate, Sasl2Challenge, Sasl2Data, Sasl2Failure, Sasl2FailureCondition,
    Sasl2Feature, Sasl2Mechanism, Sasl2Response, Sasl2Success, NS_SASL2,
};

const TIMEOUT: Duration = Duration::from_secs(5);
const NS_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
const NS_SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
const NS_STREAM: &str = "http://etherx.jabber.org/streams";
const NS_XMPP_STREAMS: &str = "urn:ietf:params:xml:ns:xmpp-streams";

type Peer = WebSocketStream<DuplexStream>;

struct OneConnection(Option<DuplexStream>);

impl Listener for OneConnection {
    type Io = DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.take() {
            Some(io) => (io, ()),
            None => future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(())
    }
}

async fn connect(state: Arc<WebSocketState>) -> (Peer, JoinHandle<()>) {
    let (server_io, peer_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        axum::serve(OneConnection(Some(server_io)), router(state))
            .await
            .expect("serve");
    });
    let (peer, response) = tokio_tungstenite::client_async("ws://localhost/ws", peer_io)
        .await
        .expect("websocket upgrade");
    assert_eq!(response.status(), 101);
    (peer, server)
}

fn open() -> Element {
    Element::builder("open", NS_FRAMING)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), "example.com")
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "alice@example.com",
        )
        .attr(minidom::rxml::xml_ncname!("version").to_owned(), "1.0")
        .build()
}

fn authenticate(data: Vec<u8>) -> Element {
    Sasl2Authenticate::new(Sasl2Mechanism::OAuthBearer, Some(Sasl2Data::new(data))).to_element()
}

fn bearer(token: &str, authzid: Option<&str>) -> Vec<u8> {
    let mut payload = match authzid {
        Some(value) => format!("n,a={value},").into_bytes(),
        None => b"n,,".to_vec(),
    };
    payload.push(1);
    payload.extend_from_slice(b"auth=Bearer ");
    payload.extend_from_slice(token.as_bytes());
    payload.extend_from_slice(b"\x01\x01");
    payload
}

async fn send(peer: &mut Peer, element: Element) {
    peer.send(Message::Text(String::from(&element).into()))
        .await
        .expect("send frame");
}

async fn send_raw(peer: &mut Peer, xml: &str) {
    peer.send(Message::Text(xml.to_owned().into()))
        .await
        .expect("send raw frame");
}

async fn receive(peer: &mut Peer) -> Element {
    match timeout(TIMEOUT, peer.next()).await.expect("frame timeout") {
        Some(Ok(Message::Text(text))) => text.parse().expect("XML frame"),
        other => panic!("expected text frame, got {other:?}"),
    }
}

async fn start_stream(peer: &mut Peer) -> Element {
    send(peer, open()).await;
    let response = receive(peer).await;
    assert_eq!(response.name(), "open");
    assert_eq!(response.ns(), NS_FRAMING);
    let features = receive(peer).await;
    assert_eq!(features.name(), "features");
    assert_eq!(features.ns(), NS_STREAM);
    features
}

async fn stop_server(server: JoinHandle<()>) {
    server.abort();
    let _ = server.await;
}

async fn expect_transport_closed(peer: &mut Peer) {
    match timeout(TIMEOUT, peer.next())
        .await
        .expect("websocket close timeout")
    {
        Some(Ok(Message::Close(_))) => {}
        other => panic!("expected WebSocket close frame, got {other:?}"),
    }
}

async fn expect_stream_closed(peer: &mut Peer) {
    let close = receive(peer).await;
    assert_eq!((close.name(), close.ns()), ("close", NS_FRAMING.to_owned()));
    expect_transport_closed(peer).await;
}

async fn seed_scram_account(state: &Arc<WebSocketState>) {
    use waddle_server::auth::{NativeUserStore, RegisterRequest};
    use waddle_server::test_support::seed_local_account;

    seed_local_account(state, "alice").await;
    NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone())
        .register(RegisterRequest {
            username: "alice".to_owned(),
            domain: "example.com".to_owned(),
            password: "test-password".to_owned(),
            email: None,
        })
        .await
        .expect("register SCRAM account");
}

async fn begin_sasl2_scram(peer: &mut Peer) {
    send(
        peer,
        Sasl2Authenticate::new(
            Sasl2Mechanism::ScramSha256,
            Some(Sasl2Data::new(b"n,,n=alice,r=clientnonce".to_vec())),
        )
        .to_element(),
    )
    .await;
    let challenge =
        Sasl2Challenge::from_element(&receive(peer).await).expect("SASL2 SCRAM challenge");
    assert!(String::from_utf8_lossy(challenge.data.as_bytes()).starts_with("r=clientnonce"));
}

fn oversized_iq_frame() -> String {
    format!(
        "<iq xmlns='jabber:client' type='get' id='too-big'>{}</iq>",
        "x".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE)
    )
}

#[tokio::test]
async fn explicit_fixture_capability_authenticates_and_sends_post_auth_features_in_place() {
    let state = websocket_state_with_sasl2().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;

    let features = start_stream(&mut peer).await;
    let authentication = features
        .get_child("authentication", NS_SASL2)
        .expect("explicit fixture advertises SASL2");
    let offered = Sasl2Feature::from_element(authentication).expect("typed SASL2 feature");
    assert_eq!(
        offered.mechanisms,
        vec![Sasl2Mechanism::ScramSha256, Sasl2Mechanism::OAuthBearer]
    );
    assert!(
        offered.inline.is_none(),
        "inline features remain out of scope"
    );

    send(&mut peer, authenticate(bearer(&session.id, None))).await;
    let success = receive(&mut peer).await;
    let success = Sasl2Success::from_element(&success).expect("SASL2 success");
    assert_eq!(
        success.authorization_identifier.to_string(),
        "alice@example.com"
    );
    assert!(success.inline_results.is_empty());

    // XEP-0388 keeps the authenticated stream open and immediately sends the
    // post-authentication features without an RFC 7395 stream restart.
    let post_auth_features = receive(&mut peer).await;
    assert_eq!(
        (post_auth_features.name(), post_auth_features.ns()),
        ("features", NS_STREAM.to_owned())
    );
    assert!(post_auth_features
        .get_child("bind", waddle_xmpp::ns::BIND)
        .is_some());
    assert!(post_auth_features
        .get_child("authentication", NS_SASL2)
        .is_none());

    // An abort outside an active SASL exchange is rejected without clearing
    // the established session or moving the connection phase.
    send_raw(&mut peer, "<abort xmlns='urn:xmpp:sasl:2'/>").await;
    let failure =
        Sasl2Failure::from_element(&receive(&mut peer).await).expect("post-auth abort is rejected");
    assert_eq!(failure.condition, Sasl2FailureCondition::Aborted);
    send_raw(
        &mut peer,
        "<iq xmlns='jabber:client' type='set' id='bind-after-abort'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource>web</resource></bind></iq>",
    )
    .await;
    let bind = receive(&mut peer).await;
    assert_eq!(bind.name(), "iq");
    assert_eq!(bind.attr("type"), Some("result"));

    stop_server(server).await;
}

#[tokio::test]
async fn default_fixture_does_not_advertise_or_accept_sasl2() {
    let state = websocket_state().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;

    let features = start_stream(&mut peer).await;
    assert!(features.get_child("authentication", NS_SASL2).is_none());
    assert!(features.get_child("mechanisms", NS_SASL).is_some());

    send(&mut peer, authenticate(bearer(&session.id, None))).await;
    let failure = receive(&mut peer).await;
    let failure = Sasl2Failure::from_element(&failure).expect("SASL2 refusal");
    assert_eq!(failure.condition, Sasl2FailureCondition::InvalidMechanism);

    stop_server(server).await;
}

#[tokio::test]
async fn malformed_sasl2_authenticate_returns_sasl2_failure_without_authenticating() {
    let state = websocket_state_with_sasl2().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;

    send_raw(
        &mut peer,
        "<authenticate xmlns='urn:xmpp:sasl:2' mechanism='OAUTHBEARER'><initial-response>!</initial-response></authenticate>",
    )
    .await;
    let failure = Sasl2Failure::from_element(&receive(&mut peer).await)
        .expect("malformed SASL2 request returns SASL2 failure");
    assert_eq!(failure.condition, Sasl2FailureCondition::MalformedRequest);

    // Parsing the invalid request did not consume or establish session
    // authority; the same stream can still authenticate with the valid token.
    send(&mut peer, authenticate(bearer(&session.id, None))).await;
    let success = Sasl2Success::from_element(&receive(&mut peer).await)
        .expect("valid request still authenticates");
    assert_eq!(
        success.authorization_identifier.to_string(),
        "alice@example.com"
    );
    let _post_auth_features = receive(&mut peer).await;
    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_rejects_mismatched_authorization_identity() {
    let state = websocket_state_with_sasl2().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state.clone()).await;
    let _ = start_stream(&mut peer).await;
    send(
        &mut peer,
        authenticate(bearer(&session.id, Some("mallory@example.com"))),
    )
    .await;
    let challenge = Sasl2Challenge::from_element(&receive(&mut peer).await)
        .expect("SASL2 authzid mismatch uses an error challenge");
    assert_eq!(
        challenge.data.as_bytes(),
        br#"{"status":"invalid_request"}"#
    );
    send(
        &mut peer,
        Sasl2Response {
            data: Sasl2Data::new(vec![0x01]),
        }
        .to_element(),
    )
    .await;
    let failure = Sasl2Failure::from_element(&receive(&mut peer).await)
        .expect("dummy response completes the failed authzid exchange");
    assert_eq!(failure.condition, Sasl2FailureCondition::NotAuthorized);

    // A mismatch is a generic OAuth mechanism error, not an invalid bearer
    // token challenge; the valid token is still usable after failure.
    send(&mut peer, authenticate(bearer(&session.id, None))).await;
    let success = Sasl2Success::from_element(&receive(&mut peer).await)
        .expect("the failed authzid did not establish a session");
    assert_eq!(
        success.authorization_identifier.to_string(),
        "alice@example.com"
    );
    let _post_auth_features = receive(&mut peer).await;
    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_oauth_invalid_token_uses_rfc7628_error_challenge_then_fails() {
    let state = websocket_state_with_sasl2().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;

    send(&mut peer, authenticate(bearer("invalid-token", None))).await;
    let challenge = Sasl2Challenge::from_element(&receive(&mut peer).await)
        .expect("invalid token starts a SASL2 error challenge");
    assert_eq!(challenge.data.as_bytes(), br#"{"status":"invalid_token"}"#);

    send(
        &mut peer,
        Sasl2Response {
            data: Sasl2Data::new(vec![0x01]),
        }
        .to_element(),
    )
    .await;
    let failure = Sasl2Failure::from_element(&receive(&mut peer).await)
        .expect("dummy response completes the failed authentication");
    assert_eq!(failure.condition, Sasl2FailureCondition::NotAuthorized);

    // The failed challenge never authenticated the connection and returns it
    // to the pre-auth state so a fresh exchange can proceed.
    send(&mut peer, authenticate(bearer(&session.id, None))).await;
    let success = Sasl2Success::from_element(&receive(&mut peer).await)
        .expect("fresh valid token authenticates after error completion");
    assert_eq!(
        success.authorization_identifier.to_string(),
        "alice@example.com"
    );
    let _post_auth_features = receive(&mut peer).await;
    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_oauth_malformed_mechanism_data_uses_error_challenge() {
    let state = websocket_state_with_sasl2().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;

    // The SASL2 initial-response Base64 is valid, but its decoded bytes are
    // not a syntactically valid OAUTHBEARER client response.
    send(&mut peer, authenticate(vec![0xff, 0x00])).await;
    let challenge = Sasl2Challenge::from_element(&receive(&mut peer).await)
        .expect("mechanism data failure uses a SASL2 challenge");
    assert_eq!(
        challenge.data.as_bytes(),
        br#"{"status":"invalid_request"}"#
    );

    send(
        &mut peer,
        Sasl2Response {
            data: Sasl2Data::new(vec![0x01]),
        }
        .to_element(),
    )
    .await;
    let failure = Sasl2Failure::from_element(&receive(&mut peer).await)
        .expect("dummy response completes the failed mechanism exchange");
    assert_eq!(failure.condition, Sasl2FailureCondition::NotAuthorized);

    send(&mut peer, authenticate(bearer(&session.id, None))).await;
    let success = Sasl2Success::from_element(&receive(&mut peer).await)
        .expect("malformed mechanism data did not authenticate");
    assert_eq!(
        success.authorization_identifier.to_string(),
        "alice@example.com"
    );
    let _post_auth_features = receive(&mut peer).await;
    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_oauth_error_challenge_can_be_aborted() {
    let state = websocket_state_with_sasl2().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;

    send(&mut peer, authenticate(bearer("invalid-token", None))).await;
    let _challenge = Sasl2Challenge::from_element(&receive(&mut peer).await)
        .expect("invalid token starts a SASL2 error challenge");
    send_raw(&mut peer, "<abort xmlns='urn:xmpp:sasl:2'/>").await;
    let failure = Sasl2Failure::from_element(&receive(&mut peer).await)
        .expect("abort fails the current authentication");
    assert_eq!(failure.condition, Sasl2FailureCondition::Aborted);

    send(&mut peer, authenticate(bearer(&session.id, None))).await;
    let success = Sasl2Success::from_element(&receive(&mut peer).await)
        .expect("aborted auth does not block a fresh valid exchange");
    assert_eq!(
        success.authorization_identifier.to_string(),
        "alice@example.com"
    );
    let _post_auth_features = receive(&mut peer).await;
    stop_server(server).await;
}

#[tokio::test]
async fn malformed_sasl2_oauth_error_response_fails_and_resets_pre_auth_state() {
    let state = websocket_state_with_sasl2().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;

    send(&mut peer, authenticate(bearer("invalid-token", None))).await;
    let _challenge = Sasl2Challenge::from_element(&receive(&mut peer).await)
        .expect("invalid token starts a SASL2 error challenge");
    send_raw(&mut peer, "<response xmlns='urn:xmpp:sasl:2'>!</response>").await;
    let failure = Sasl2Failure::from_element(&receive(&mut peer).await)
        .expect("malformed challenge response fails SASL2");
    assert_eq!(failure.condition, Sasl2FailureCondition::MalformedRequest);

    send(&mut peer, authenticate(bearer(&session.id, None))).await;
    let success = Sasl2Success::from_element(&receive(&mut peer).await)
        .expect("malformed response did not leave the auth phase stuck");
    assert_eq!(
        success.authorization_identifier.to_string(),
        "alice@example.com"
    );
    let _post_auth_features = receive(&mut peer).await;
    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_oauth_error_pending_closes_on_non_sasl_traffic() {
    let state = websocket_state_with_sasl2().await;
    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;

    send(&mut peer, authenticate(bearer("invalid-token", None))).await;
    let _challenge = Sasl2Challenge::from_element(&receive(&mut peer).await)
        .expect("invalid token starts a SASL2 error challenge");
    send_raw(
        &mut peer,
        "<iq xmlns='jabber:client' type='get' id='pending-auth'><query xmlns='jabber:iq:roster'/></iq>",
    )
    .await;
    expect_stream_closed(&mut peer).await;
    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_oauth_error_pending_closes_on_whitespace() {
    let state = websocket_state_with_sasl2().await;
    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;

    send(&mut peer, authenticate(bearer("invalid-token", None))).await;
    let _challenge = Sasl2Challenge::from_element(&receive(&mut peer).await)
        .expect("invalid token starts a SASL2 error challenge");
    send_raw(&mut peer, " \t\r\n").await;
    expect_stream_closed(&mut peer).await;

    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_oauth_error_pending_closes_on_oversized_frame() {
    let state = websocket_state_with_sasl2().await;
    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;

    send(&mut peer, authenticate(bearer("invalid-token", None))).await;
    let _challenge = Sasl2Challenge::from_element(&receive(&mut peer).await)
        .expect("invalid token starts a SASL2 error challenge");
    let oversized = oversized_iq_frame();
    send_raw(&mut peer, &oversized).await;
    expect_stream_closed(&mut peer).await;

    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_scram_pending_closes_on_whitespace() {
    let state = websocket_state_with_sasl2().await;
    seed_scram_account(&state).await;
    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;
    begin_sasl2_scram(&mut peer).await;

    send_raw(&mut peer, " \t\r\n").await;
    expect_stream_closed(&mut peer).await;

    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_scram_pending_closes_on_oversized_frame() {
    let state = websocket_state_with_sasl2().await;
    seed_scram_account(&state).await;
    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;
    begin_sasl2_scram(&mut peer).await;

    let oversized = oversized_iq_frame();
    send_raw(&mut peer, &oversized).await;
    expect_stream_closed(&mut peer).await;

    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_scram_pending_closes_on_non_sasl_traffic() {
    let state = websocket_state_with_sasl2().await;
    seed_scram_account(&state).await;
    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;
    begin_sasl2_scram(&mut peer).await;

    send_raw(
        &mut peer,
        "<iq xmlns='jabber:client' type='get' id='pending-scram'><query xmlns='jabber:iq:roster'/></iq>",
    )
    .await;
    expect_stream_closed(&mut peer).await;

    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_authenticate_after_success_sends_stream_error_and_closes_transport() {
    let state = websocket_state_with_sasl2().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;

    let _ = start_stream(&mut peer).await;
    send(&mut peer, authenticate(bearer(&session.id, None))).await;
    let _success = Sasl2Success::from_element(&receive(&mut peer).await)
        .expect("first SASL2 authentication succeeds");
    let _post_auth_features = receive(&mut peer).await;

    send_raw(
        &mut peer,
        "<iq xmlns='jabber:client' type='set' id='bind'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource>web</resource></bind></iq>",
    )
    .await;
    let bind = receive(&mut peer).await;
    assert_eq!(bind.name(), "iq");
    assert_eq!(bind.attr("type"), Some("result"));

    send(&mut peer, authenticate(bearer(&session.id, None))).await;
    let stream_error = receive(&mut peer).await;
    assert_eq!(
        (stream_error.name(), stream_error.ns()),
        ("error", NS_STREAM.to_owned())
    );
    assert!(stream_error
        .get_child("not-authorized", NS_XMPP_STREAMS)
        .is_some());
    expect_stream_closed(&mut peer).await;

    stop_server(server).await;
}

#[tokio::test]
async fn sasl2_scram_sha256_reuses_the_existing_verifier_and_returns_server_final_data() {
    use waddle_server::auth::{NativeUserStore, RegisterRequest};
    use waddle_server::test_support::seed_local_account;

    let state = websocket_state_with_sasl2().await;
    seed_local_account(&state, "alice").await;
    NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone())
        .register(RegisterRequest {
            username: "alice".to_owned(),
            domain: "example.com".to_owned(),
            password: "test-password".to_owned(),
            email: None,
        })
        .await
        .expect("register SCRAM account");

    let (mut peer, server) = connect(state).await;
    let features = start_stream(&mut peer).await;
    let offered = Sasl2Feature::from_element(
        features
            .get_child("authentication", NS_SASL2)
            .expect("SASL2 feature"),
    )
    .expect("typed SASL2 feature");
    assert!(offered.mechanisms.contains(&Sasl2Mechanism::ScramSha256));

    let client_first = "n,,n=alice,r=clientnonce";
    send(
        &mut peer,
        Sasl2Authenticate::new(
            Sasl2Mechanism::ScramSha256,
            Some(Sasl2Data::new(client_first.as_bytes().to_vec())),
        )
        .to_element(),
    )
    .await;
    let challenge =
        Sasl2Challenge::from_element(&receive(&mut peer).await).expect("SASL2 SCRAM challenge");
    let client_final = scram_client_final("alice", "test-password", &challenge.data);
    send(
        &mut peer,
        Sasl2Response {
            data: Sasl2Data::new(client_final.into_bytes()),
        }
        .to_element(),
    )
    .await;

    let success =
        Sasl2Success::from_element(&receive(&mut peer).await).expect("SASL2 SCRAM success");
    assert_eq!(
        success.authorization_identifier.to_string(),
        "alice@example.com"
    );
    let server_final = success
        .additional_data
        .expect("SCRAM server-final additional-data");
    assert!(String::from_utf8_lossy(server_final.as_bytes()).starts_with("v="));
    let post_auth_features = receive(&mut peer).await;
    assert_eq!(
        (post_auth_features.name(), post_auth_features.ns()),
        ("features", NS_STREAM.to_owned())
    );
    stop_server(server).await;
}

#[tokio::test]
async fn malformed_sasl2_scram_continuation_returns_sasl2_failure_and_clears_pending_state() {
    use waddle_server::auth::{NativeUserStore, RegisterRequest};
    use waddle_server::test_support::seed_local_account;

    let state = websocket_state_with_sasl2().await;
    seed_local_account(&state, "alice").await;
    NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone())
        .register(RegisterRequest {
            username: "alice".to_owned(),
            domain: "example.com".to_owned(),
            password: "test-password".to_owned(),
            email: None,
        })
        .await
        .expect("register SCRAM account");

    let (mut peer, server) = connect(state).await;
    let _ = start_stream(&mut peer).await;
    send(
        &mut peer,
        Sasl2Authenticate::new(
            Sasl2Mechanism::ScramSha256,
            Some(Sasl2Data::new(b"n,,n=alice,r=clientnonce".to_vec())),
        )
        .to_element(),
    )
    .await;
    let challenge = receive(&mut peer).await;
    assert_eq!(
        (challenge.name(), challenge.ns()),
        ("challenge", NS_SASL2.to_owned())
    );

    send_raw(&mut peer, "<response xmlns='urn:xmpp:sasl:2'>!</response>").await;
    let failure = Sasl2Failure::from_element(&receive(&mut peer).await)
        .expect("malformed SASL2 continuation returns SASL2 failure");
    assert_eq!(failure.condition, Sasl2FailureCondition::MalformedRequest);
    stop_server(server).await;
}

fn scram_client_final(username: &str, password: &str, challenge: &Sasl2Data) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::{Digest, Sha256};

    type HmacSha256 = Hmac<Sha256>;

    fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts key");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    fn sha256(data: &[u8]) -> Vec<u8> {
        Sha256::digest(data).to_vec()
    }

    let server_first = String::from_utf8(challenge.as_bytes().to_vec()).expect("server-first");
    let mut nonce = None;
    let mut salt = None;
    let mut iterations = None;
    for field in server_first.split(',') {
        if let Some(value) = field.strip_prefix("r=") {
            nonce = Some(value.to_owned());
        } else if let Some(value) = field.strip_prefix("s=") {
            salt = Some(value.to_owned());
        } else if let Some(value) = field.strip_prefix("i=") {
            iterations = value.parse::<u32>().ok();
        }
    }
    let nonce = nonce.expect("server nonce");
    let salt = BASE64_STANDARD
        .decode(salt.expect("salt"))
        .expect("salt base64");
    let iterations = iterations.expect("iterations");
    let salted_password =
        pbkdf2::pbkdf2_hmac_array::<Sha256, 32>(password.as_bytes(), &salt, iterations);
    let client_key = hmac_sha256(&salted_password, b"Client Key");
    let stored_key = sha256(&client_key);
    let final_without_proof = format!("c=biws,r={nonce}");
    let client_first_bare = format!("n={username},r=clientnonce");
    let auth_message = format!("{client_first_bare},{server_first},{final_without_proof}");
    let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
    let proof = client_key
        .iter()
        .zip(client_signature)
        .map(|(key, signature)| key ^ signature)
        .collect::<Vec<_>>();
    format!("{final_without_proof},p={}", BASE64_STANDARD.encode(proof))
}
