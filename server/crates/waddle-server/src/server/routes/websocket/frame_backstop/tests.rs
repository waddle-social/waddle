use super::*;
use std::future::pending;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::presence::Presence;

fn iq_get_stanza(id: &str, from: &str, to: &str) -> Stanza {
    let payload = Element::builder("query", "http://jabber.org/protocol/disco#info").build();
    let iq = Iq::Get {
        id: id.to_string(),
        from: Some(from.parse().expect("from jid")),
        to: Some(to.parse().expect("to jid")),
        payload,
    };
    Stanza::Iq(Box::new(iq))
}

fn iq_result_stanza(id: &str) -> Stanza {
    // A `result` IQ is a response, not a request — it owes no reply even if it
    // somehow reaches dispatch and times out.
    let iq = Iq::Result {
        id: id.to_string(),
        from: Some("upload.example.com".parse().expect("from jid")),
        to: Some("alice@example.com/web".parse().expect("to jid")),
        payload: None,
    };
    Stanza::Iq(Box::new(iq))
}

fn message_stanza() -> Stanza {
    let to: xmpp_parsers::jid::Jid = "bob@example.com".parse().expect("to jid");
    Stanza::Message(Message::new(Some(to)))
}

#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn groupchat_dispatch_span_carries_correlation_and_bound_identity_without_body() {
    // Every test in this module drives dispatch, which emits OTel metrics;
    // hold the metric-reader guard so runs under plain `cargo test` (shared
    // process, parallel threads) serialize against the global provider
    // instead of racing the export assertions. No-op under nextest.
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let room: xmpp_parsers::jid::Jid = "team@muc.example.com".parse().expect("room jid");
    let mut message = Message::new(Some(room));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.id = Some(xmpp_parsers::message::Id("dispatch-correlation-1".into()));
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "sensitive body must not enter telemetry".into(),
    );
    let stanza = Stanza::Message(message);
    let bound: jid::FullJid = "alice@example.com/browser".parse().expect("bound jid");
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer(CaptureWriter(Arc::clone(&bytes)))
        .finish();

    let _guard = tracing::subscriber::set_default(subscriber);
    let backstop = StanzaBackstop::capture(&stanza, Some(&bound));
    run_with_backstop(backstop, async {
        tracing::info!("dispatch field probe");
        Vec::<String>::new()
    })
    .await
    .expect("dispatch completes");

    let output = String::from_utf8(bytes.lock().expect("capture lock").clone())
        .expect("captured tracing is UTF-8");
    for expected in [
        "\"message_id\":\"dispatch-correlation-1\"",
        "\"room\":\"team@muc.example.com\"",
        "\"xmpp.resource\":\"browser\"",
        "\"user\":\"alice@example.com\"",
    ] {
        assert!(output.contains(expected), "missing {expected} in {output}");
    }
    assert!(
        !output.contains("sensitive body must not enter telemetry"),
        "message bodies must never be tracing fields: {output}"
    );
}

fn presence_stanza() -> Stanza {
    Stanza::Presence(Presence::new(xmpp_parsers::presence::Type::None))
}

fn responses_and_disposition(
    result: Result<Vec<String>, StanzaTimeout>,
) -> (Vec<String>, InboundDisposition) {
    match result {
        Ok(responses) => (responses, InboundDisposition::Handled),
        Err(StanzaTimeout::HandledIq(reply)) => (
            vec![crate::server::routes::websocket::element_to_xml(reply)],
            InboundDisposition::Handled,
        ),
        Err(StanzaTimeout::Unhandled) => (Vec::new(), InboundDisposition::Unhandled),
        Err(StanzaTimeout::AdmissionRevoked) => (Vec::new(), InboundDisposition::Unhandled),
    }
}

#[tokio::test]
async fn already_revoked_admission_never_starts_dispatch_and_preserves_sm_hole() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let stanza = message_stanza();
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let side_effect = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dispatch_side_effect = Arc::clone(&side_effect);
    lifecycle.begin_fenced_recovery();
    let dispatch = async move {
        dispatch_side_effect.store(true, std::sync::atomic::Ordering::SeqCst);
        Vec::new()
    };
    let guarded = run_with_backstop_and_admission(
        StanzaBackstop::capture(&stanza, None),
        dispatch,
        &permit,
        &shutdown,
    )
    .await;
    let (responses, disposition) = responses_and_disposition(guarded.result);

    assert!(
        responses.is_empty(),
        "revocation must not synthesize a reply"
    );
    assert_eq!(disposition, InboundDisposition::Unhandled);
    assert!(!side_effect.load(std::sync::atomic::Ordering::SeqCst));

    let mut sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
    sm_state.enable("revoked-dispatch".to_string(), true, Some(300));
    let mut completion = crate::server::routes::interpret::SmInboundCompletionTracker::default();
    let sequence = completion.reserve(&sm_state);
    crate::server::routes::websocket::frame::settle_inbound_dispatch(
        &crate::ingress_shadow::IngressShadowHandle::disabled(),
        disposition,
        false,
        Some(sequence),
        &mut completion,
        &mut sm_state,
    );
    assert_eq!(sm_state.get_inbound_count(), 0);
    assert!(completion.has_unhandled_hole());
    assert!(!completion.has_pending());
}

#[tokio::test]
async fn revoked_after_committed_dispatch_suppresses_frames_but_settles_sm() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let stanza = message_stanza();
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let commits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let dispatch_commits = Arc::clone(&commits);
    let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let dispatch = async move {
        dispatch_commits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = committed_tx.send(());
        let _ = release_rx.await;
        vec!["<message type='result'/>".to_string()]
    };
    let mut guarded = Box::pin(run_with_backstop_and_admission(
        StanzaBackstop::capture(&stanza, None),
        dispatch,
        &permit,
        &shutdown,
    ));

    assert!(futures::poll!(guarded.as_mut()).is_pending());
    committed_rx.await.expect("dispatch committed its effect");
    lifecycle.begin_fenced_recovery();
    release_tx
        .send(())
        .expect("dispatch remains owned by the backstop");
    let guarded = guarded.await;
    assert!(guarded.authority_revoked_after_start);

    let (responses, disposition) = responses_and_disposition(guarded.result);
    assert!(
        responses.is_empty(),
        "stale admission must not write responses"
    );
    assert_eq!(disposition, InboundDisposition::Handled);
    assert_eq!(commits.load(std::sync::atomic::Ordering::SeqCst), 1);

    let mut sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
    sm_state.enable("revoked-after-commit".to_string(), true, Some(300));
    let mut completion = crate::server::routes::interpret::SmInboundCompletionTracker::default();
    let sequence = completion.reserve(&sm_state);
    crate::server::routes::websocket::frame::settle_inbound_dispatch(
        &crate::ingress_shadow::IngressShadowHandle::disabled(),
        disposition,
        false,
        Some(sequence),
        &mut completion,
        &mut sm_state,
    );
    assert_eq!(sm_state.get_inbound_count(), 1);
    assert!(!completion.has_unhandled_hole());
    assert!(!completion.has_pending());
}

#[tokio::test(start_paused = true)]
async fn iq_get_timeout_yields_conformant_resource_constraint() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let stanza = iq_get_stanza("disco-1", "alice@example.com/web", "upload.example.com");
    let backstop = StanzaBackstop::capture(&stanza, None);

    let mut fut = Box::pin(run_with_backstop(backstop, pending::<Vec<String>>()));
    assert!(
        futures::poll!(fut.as_mut()).is_pending(),
        "must not resolve before the wedge budget elapses"
    );
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let (responses, disposition) = responses_and_disposition(fut.await);

    assert_eq!(
        responses.len(),
        1,
        "a timed-out IQ get owes exactly one reply"
    );
    assert_eq!(disposition, InboundDisposition::Handled);
    let mut sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
    sm_state.enable("iq-timeout".to_string(), true, Some(300));
    let mut completion = crate::server::routes::interpret::SmInboundCompletionTracker::default();
    let sequence = completion.reserve(&sm_state);
    crate::server::routes::websocket::frame::settle_inbound_dispatch(
        &crate::ingress_shadow::IngressShadowHandle::disabled(),
        disposition,
        false,
        Some(sequence),
        &mut completion,
        &mut sm_state,
    );
    assert_eq!(
        sm_state.get_inbound_count(),
        1,
        "the retryable IQ error accepts responsibility for the request"
    );
    let reply = &responses[0];
    // minidom serializes attributes with single quotes (house style; see the
    // existing iq.rs assertions).
    assert!(
        reply.contains("type='error'"),
        "must be an IQ error: {reply}"
    );
    assert!(
        reply.contains("resource-constraint"),
        "must carry resource-constraint: {reply}"
    );
    assert!(
        reply.contains("type='wait'"),
        "resource-constraint must be a retryable wait error: {reply}"
    );
    assert!(
        reply.contains("id='disco-1'"),
        "must echo the request id: {reply}"
    );
    // RFC 6120 §8.2.3: from/to are swapped on the response.
    assert!(
        reply.contains("from='upload.example.com'"),
        "response from = request to: {reply}"
    );
    assert!(
        reply.contains("to='alice@example.com/web'"),
        "response to = request from: {reply}"
    );
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn iq_timeout_exports_span_status_error() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let stanza = iq_get_stanza("disco-1", "alice@example.com/web", "upload.example.com");
    let backstop = StanzaBackstop::capture(&stanza, None);

    let mut fut = Box::pin(run_with_backstop(backstop, pending::<Vec<String>>()));
    assert!(
        futures::poll!(fut.as_mut()).is_pending(),
        "must not resolve before the wedge budget elapses"
    );
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let (_, disposition) = responses_and_disposition(fut.await);
    assert_eq!(disposition, InboundDisposition::Handled);

    assert!(matches!(
        spans.status_of("xmpp.stanza.dispatch"),
        Some(opentelemetry::trace::Status::Error { .. })
    ));
    assert_eq!(
        spans.attribute_of("xmpp.stanza.dispatch", "condition"),
        Some("resource-constraint".to_string()),
        "the timeout fallback should expose its retryable stanza condition"
    );
}

#[tokio::test(start_paused = true)]
async fn message_timeout_yields_no_response() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let stanza = message_stanza();
    let backstop = StanzaBackstop::capture(&stanza, None);

    let fut = run_with_backstop(backstop, pending::<Vec<String>>());
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let (responses, disposition) = responses_and_disposition(fut.await);

    assert!(
        responses.is_empty(),
        "a timed-out message owes no reply: {:?}",
        responses
    );
    assert_eq!(disposition, InboundDisposition::Unhandled);
}

#[tokio::test(start_paused = true)]
async fn presence_timeout_yields_no_response() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let stanza = presence_stanza();
    let backstop = StanzaBackstop::capture(&stanza, None);

    let fut = run_with_backstop(backstop, pending::<Vec<String>>());
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let (responses, disposition) = responses_and_disposition(fut.await);

    assert!(responses.is_empty(), "a timed-out presence owes no reply");
    assert_eq!(disposition, InboundDisposition::Unhandled);
}

#[tokio::test(start_paused = true)]
async fn iq_result_timeout_yields_no_response() {
    // RFC 6120 §8.2.3: only `get`/`set` owe a response. A timed-out `result`
    // IQ must NOT synthesize an error reply (locks the `_ => None` arm in
    // `capture`).
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let stanza = iq_result_stanza("ack-1");
    let backstop = StanzaBackstop::capture(&stanza, None);

    let fut = run_with_backstop(backstop, pending::<Vec<String>>());
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let (responses, disposition) = responses_and_disposition(fut.await);

    assert!(
        responses.is_empty(),
        "a timed-out IQ result owes no reply: {:?}",
        responses
    );
    assert_eq!(disposition, InboundDisposition::Unhandled);
}

#[tokio::test(start_paused = true)]
async fn stanza_timeout_exports_the_canonical_counter_end_to_end() {
    // #1136: a synthetic wedge timeout driven through the production
    // backstop must land on the canonical exported series, proving the
    // increment site → OTel export chain (the reader seam stands in for
    // the OTLP exporter; both read the same meter provider).
    let guard = waddle_xmpp::telemetry::test_support::acquire().await;
    let stanza = iq_get_stanza(
        "disco-timeout-metric",
        "alice@example.com/web",
        "upload.example.com",
    );
    let backstop = StanzaBackstop::capture(&stanza, None);

    let fut = run_with_backstop(backstop, pending::<Vec<String>>());
    tokio::time::advance(STANZA_HANDLER_WEDGE_TIMEOUT + Duration::from_millis(1)).await;
    let (_, disposition) = responses_and_disposition(fut.await);
    assert_eq!(disposition, InboundDisposition::Handled);

    assert_eq!(
        guard.counter_sum(
            "xmpp.stanza.handler.timeout",
            &[
                ("kind", "iq"),
                ("payload_ns", "http://jabber.org/protocol/disco#info"),
            ],
        ),
        Some(1),
        "a synthetic wedge timeout must export exactly one canonical sample",
    );
}

#[tokio::test(start_paused = true)]
async fn fast_dispatch_passes_through_untouched() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let stanza = iq_get_stanza("disco-2", "alice@example.com/web", "example.com");
    let backstop = StanzaBackstop::capture(&stanza, None);

    // A handler that completes immediately must pass its response through
    // unchanged, with no timeout reply.
    let result = run_with_backstop(backstop, async {
        vec!["<iq id=\"disco-2\" type=\"result\"/>".to_string()]
    })
    .await;
    let (responses, disposition) = responses_and_disposition(result);

    assert_eq!(
        responses,
        vec!["<iq id=\"disco-2\" type=\"result\"/>".to_string()]
    );
    assert_eq!(disposition, InboundDisposition::Handled);
}
