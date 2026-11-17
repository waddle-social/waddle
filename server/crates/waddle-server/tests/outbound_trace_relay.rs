//! A stanza forwarded to a remote socket crosses nodes in its producer's
//! trace: the forwarder runs under a span opened from the stanza's captured
//! context, and the relay stamps its envelope from the current span.
#![cfg(feature = "clustering")]

use opentelemetry::trace::TraceContextExt;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use waddle_server::clustering::trace_context::{remote_forward_span, RelayTraceContext};
use waddle_xmpp::registry::OutboundStanza;
use waddle_xmpp::Stanza;

fn message() -> Stanza {
    Stanza::Message(xmpp_parsers::message::Message::new(None))
}

#[test]
fn a_forwarded_stanza_stamps_the_relay_envelope_with_its_producers_trace() {
    let _spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let producer = tracing::info_span!("producer-under-test");
    let (outbound, trace_id) = producer.in_scope(|| {
        let context = tracing::Span::current().context();
        (
            OutboundStanza::new(message()),
            context.span().span_context().trace_id(),
        )
    });
    drop(producer);

    // The forwarder task has no span of its own.
    assert_eq!(RelayTraceContext::capture(), RelayTraceContext::default());

    let envelope = remote_forward_span(&outbound)
        .expect("a traced stanza has a forward span")
        .in_scope(RelayTraceContext::capture);
    let wire = serde_json::to_string(&envelope).expect("envelope encodes");
    assert!(
        wire.contains(&format!("{trace_id:032x}")),
        "the envelope must carry the producer's trace id: {wire}"
    );
}

#[test]
fn an_untraced_stanza_is_forwarded_without_a_span() {
    let _spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    assert!(remote_forward_span(&OutboundStanza::new(message())).is_none());
}
