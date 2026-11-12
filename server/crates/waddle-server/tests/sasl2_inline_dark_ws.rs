//! Dark XEP-0388 handshake with XEP-0198 inline resume and XEP-0386 Bind 2
//! over the production WebSocket router.
//!
//! The fixture advertises and dispatches SASL2 only because it opts into the
//! test capability, and processes inline requests only because it selects the
//! durable stream-management authority. Neither is true of a production
//! build's defaults. The loopback transport proves the application handshake,
//! not TLS or early-data behaviour.
#![cfg(feature = "test-support")]

use axum::serve::Listener;
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
    create_test_session, router, websocket_state_with_sasl2,
    websocket_state_with_sasl2_and_durable_sm, WebSocketState,
};
use waddle_xmpp::ownership::SmSessionKey;
use waddle_xmpp::stream_management::durable::DurableSmState;
use waddle_xmpp_core::xep0388::{
    Sasl2Authenticate, Sasl2Data, Sasl2Failure, Sasl2FailureCondition, Sasl2Mechanism, Sasl2Success,
};
use waddle_xmpp_core::xep0388_inline::{
    Bind2Request, Bind2StreamManagementResult, Sasl2InlineRequests, Sasl2InlineResults,
    SmInlineEnable, SmInlineResume, SmInlineResumeResult, SmStreamId, NS_SM,
};

const TIMEOUT: Duration = Duration::from_secs(5);
const NS_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
const NS_STREAM: &str = "http://etherx.jabber.org/streams";

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

async fn send(peer: &mut Peer, element: Element) {
    peer.send(Message::Text(String::from(&element).into()))
        .await
        .expect("send frame");
}

async fn receive(peer: &mut Peer) -> Element {
    match timeout(TIMEOUT, peer.next()).await.expect("frame timeout") {
        Some(Ok(Message::Text(text))) => text.parse().expect("XML frame"),
        other => panic!("expected text frame, got {other:?}"),
    }
}

async fn start_stream(peer: &mut Peer) {
    send(
        peer,
        Element::builder("open", NS_FRAMING)
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), "example.com")
            .attr(minidom::rxml::xml_ncname!("version").to_owned(), "1.0")
            .build(),
    )
    .await;
    assert_eq!(receive(peer).await.name(), "open");
    let features = receive(peer).await;
    assert_eq!(
        (features.name(), features.ns()),
        ("features", NS_STREAM.to_owned())
    );
}

fn bearer(token: &str) -> Vec<u8> {
    let mut payload = b"n,,".to_vec();
    payload.push(1);
    payload.extend_from_slice(b"auth=Bearer ");
    payload.extend_from_slice(token.as_bytes());
    payload.extend_from_slice(b"\x01\x01");
    payload
}

/// Authenticate with OAUTHBEARER, carrying `inline` in the request, and
/// return the next frame.
async fn authenticate(peer: &mut Peer, token: &str, inline: &Sasl2InlineRequests) -> Element {
    let mut request = Sasl2Authenticate::new(
        Sasl2Mechanism::OAuthBearer,
        Some(Sasl2Data::new(bearer(token))),
    );
    inline
        .append_to(&mut request)
        .expect("inline requests fit the authenticate request");
    send(peer, request.to_element()).await;
    receive(peer).await
}

fn inline_results(frame: &Element) -> Sasl2InlineResults {
    let success = Sasl2Success::from_element(frame).expect("SASL2 success");
    assert_eq!(
        success.authorization_identifier.to_string(),
        "alice@example.com"
    );
    Sasl2InlineResults::from_success(&success).expect("typed inline results")
}

fn bind_with_resumable_session() -> Sasl2InlineRequests {
    Sasl2InlineRequests {
        resume: None,
        bind: Some(Bind2Request {
            stream_management: Some(SmInlineEnable {
                resume: true,
                max: None,
            }),
            ..Bind2Request::default()
        }),
    }
}

async fn expect_post_auth_features(peer: &mut Peer) {
    let features = receive(peer).await;
    assert_eq!(
        (features.name(), features.ns()),
        ("features", NS_STREAM.to_owned())
    );
}

/// The state of the one session the authority holds.
async fn only_session_state(state: &WebSocketState) -> DurableSmState {
    let store = state
        .deps
        .app_state
        .durable_sm()
        .store()
        .expect("durable authority selected");
    let ids = store.stream_ids().await.expect("list durable sessions");
    assert_eq!(ids.len(), 1, "exactly one durable session");
    let key = SmSessionKey::from_uuid(ids[0].as_str().parse().expect("session key"));
    store
        .session(key)
        .await
        .expect("read durable session")
        .expect("durable session row")
        .state
}

async fn wait_for_only_session(state: &WebSocketState, expected: DurableSmState) {
    timeout(Duration::from_secs(15), async {
        while only_session_state(state).await != expected {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("durable session reached the expected state");
}

/// Bind a resource with a resumable session through Bind 2 and return the
/// resume token `<bound/>` reported.
async fn bind_resumable(peer: &mut Peer, token: &str) -> SmStreamId {
    let results = inline_results(&authenticate(peer, token, &bind_with_resumable_session()).await);
    assert!(results.resume.is_none(), "no resume was requested");
    let bound = results.bound.expect("Bind 2 result");
    let Some(Bind2StreamManagementResult::Enabled(enabled)) = bound.stream_management else {
        panic!("stream management was not enabled: {bound:?}");
    };
    assert!(enabled.resume);
    let resume_token = enabled.id.expect("a resumable session reports its id");
    expect_post_auth_features(peer).await;
    resume_token
}

#[tokio::test]
async fn bind2_binds_and_enables_stream_management_inside_success() {
    let state = websocket_state_with_sasl2_and_durable_sm().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state.clone()).await;
    start_stream(&mut peer).await;

    let resume_token = bind_resumable(&mut peer, &session.id).await;

    assert_eq!(only_session_state(&state).await, DurableSmState::Live);
    assert!(
        resume_token.as_str().parse::<uuid::Uuid>().is_err(),
        "the client holds an opaque token, not the session key"
    );
    // The stream is bound and counting: an `<r/>` is answered.
    send(&mut peer, Element::builder("r", NS_SM).build()).await;
    let answer = receive(&mut peer).await;
    assert!(answer.is("a", NS_SM));
    assert_eq!(answer.attr("h"), Some("0"));
    server.abort();
}

#[tokio::test]
async fn inline_resume_reports_resumed_skips_the_bind_and_sends_no_stream_features() {
    let state = websocket_state_with_sasl2_and_durable_sm().await;
    let session = create_test_session(&state, "alice").await;
    let (mut first, first_server) = connect(state.clone()).await;
    start_stream(&mut first).await;
    let resume_token = bind_resumable(&mut first, &session.id).await;
    drop(first);
    wait_for_only_session(&state, DurableSmState::Detached).await;
    first_server.abort();

    let (mut peer, server) = connect(state.clone()).await;
    start_stream(&mut peer).await;
    let requests = Sasl2InlineRequests {
        resume: Some(SmInlineResume {
            previd: resume_token.clone(),
            h: 0,
        }),
        ..bind_with_resumable_session()
    };
    let results = inline_results(&authenticate(&mut peer, &session.id, &requests).await);
    let Some(SmInlineResumeResult::Resumed(resumed)) = results.resume else {
        panic!("the session was not resumed: {results:?}");
    };
    assert_eq!(resumed.previd, resume_token);
    assert!(
        results.bound.is_none(),
        "a resumed session keeps its resource; the bind is skipped"
    );
    wait_for_only_session(&state, DurableSmState::Live).await;

    // No stream features follow a resumed `<success/>`: the next frame the
    // server sends is the answer to the next request.
    send(&mut peer, Element::builder("r", NS_SM).build()).await;
    let answer = receive(&mut peer).await;
    assert!(
        answer.is("a", NS_SM),
        "unexpected frame after resume: {answer:?}"
    );
    server.abort();
}

#[tokio::test]
async fn a_failed_inline_resume_falls_back_to_bind2() {
    let state = websocket_state_with_sasl2_and_durable_sm().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state.clone()).await;
    start_stream(&mut peer).await;

    let requests = Sasl2InlineRequests {
        resume: Some(SmInlineResume {
            previd: SmStreamId::new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .expect("token-shaped id"),
            h: 0,
        }),
        bind: Some(Bind2Request::default()),
    };
    let results = inline_results(&authenticate(&mut peer, &session.id, &requests).await);
    let Some(SmInlineResumeResult::Failed(failed)) = results.resume else {
        panic!("an unknown token cannot resume: {results:?}");
    };
    assert_eq!(
        failed.condition,
        Some(xmpp_parsers::stanza_error::DefinedCondition::ItemNotFound)
    );
    let bound = results
        .bound
        .expect("the bind follows a known resume failure");
    assert!(
        bound.stream_management.is_none(),
        "no enable was requested with the bind"
    );
    expect_post_auth_features(&mut peer).await;
    server.abort();
}

#[tokio::test]
async fn inline_requests_are_malformed_without_the_durable_authority() {
    let state = websocket_state_with_sasl2().await;
    let session = create_test_session(&state, "alice").await;
    let (mut peer, server) = connect(state).await;
    start_stream(&mut peer).await;

    let frame = authenticate(&mut peer, &session.id, &bind_with_resumable_session()).await;
    let failure = Sasl2Failure::from_element(&frame).expect("SASL2 failure");
    assert_eq!(failure.condition, Sasl2FailureCondition::MalformedRequest);
    server.abort();
}
