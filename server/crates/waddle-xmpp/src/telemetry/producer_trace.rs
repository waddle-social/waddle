//! Trace context carried across an in-process queue.
//!
//! A queued item outlives the task that produced it, so the consumer's
//! work cannot inherit the producer's span implicitly. [`ProducerTrace`]
//! captures the producer's OpenTelemetry span context as a value and lets
//! the consumer open a span in the same trace.
//!
//! It deliberately holds a [`SpanContext`], not a [`tracing::Span`]: a span
//! handle kept in a queue keeps the producer's span open until the item is
//! dequeued and inflates its duration by the queue wait.

use opentelemetry::trace::{SpanContext, TraceContextExt};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// The span context of `cx` when it is worth propagating: valid and sampled.
///
/// An unsampled context is not propagated. A span the sampler dropped still
/// has a valid context, and parenting on it makes the consumer's span and
/// everything under it disappear too.
pub fn sampled_span_context(cx: &opentelemetry::Context) -> Option<SpanContext> {
    let span_context = cx.span().span_context().clone();
    (span_context.is_valid() && span_context.is_sampled()).then_some(span_context)
}

/// The producer's trace context, or nothing when the producer was untraced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProducerTrace(Option<SpanContext>);

impl ProducerTrace {
    /// No producer context: the consumer opens no span.
    pub const NONE: Self = Self(None);

    /// Capture the calling task's current span.
    pub fn capture() -> Self {
        Self(sampled_span_context(&tracing::Span::current().context()))
    }

    /// Whether a producer context was captured.
    pub fn is_traced(&self) -> bool {
        self.0.is_some()
    }

    /// Open a consumer span in the producer's trace.
    ///
    /// `make` must build the span with `parent: None`; it is called only
    /// when a producer context exists, so untraced items add no spans. The
    /// span is parented before it is returned, which must precede its first
    /// entry.
    pub fn span(&self, make: impl FnOnce() -> tracing::Span) -> Option<tracing::Span> {
        let parent = self.0.clone()?;
        let span = make();
        let _ = span.set_parent(opentelemetry::Context::new().with_remote_span_context(parent));
        Some(span)
    }
}

#[cfg(test)]
mod tests {
    use super::ProducerTrace;
    use crate::pending_delivery::PendingRowId;
    use crate::registry::OutboundStanza;
    use crate::telemetry::test_support::acquire_spans;
    use crate::Stanza;
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    fn message() -> Stanza {
        Stanza::Message(xmpp_parsers::message::Message::new(None))
    }

    #[test]
    fn a_stanza_built_in_a_traced_span_is_written_in_that_trace() {
        let spans = acquire_spans();
        let producer = tracing::info_span!("producer-under-test");
        let (outbound, producer_context) = producer.in_scope(|| {
            let context = tracing::Span::current().context();
            (
                OutboundStanza::peer_stanza(message()),
                context.span().span_context().clone(),
            )
        });
        drop(producer);

        // Dequeued later, outside the producer's span.
        drop(
            outbound
                .write_span()
                .expect("a traced stanza has a write span"),
        );

        let exported = spans.exported();
        let write = exported
            .iter()
            .find(|span| span.name == "xmpp.connection.write")
            .expect("write span exports");
        assert_eq!(write.span_context.trace_id(), producer_context.trace_id());
        assert_eq!(write.parent_span_id, producer_context.span_id());
        assert_eq!(
            spans.attribute_of("xmpp.connection.write", "delivery"),
            Some("peer_stanza".to_string())
        );
    }

    #[test]
    fn a_stanza_built_outside_any_span_adds_no_write_span() {
        let spans = acquire_spans();
        let outbound = OutboundStanza::new(message());
        assert_eq!(outbound.trace, ProducerTrace::NONE);
        assert!(outbound.write_span().is_none());
        assert!(spans.exported().is_empty());
    }

    #[test]
    fn replayed_stanzas_do_not_join_the_span_that_replays_them() {
        let _spans = acquire_spans();
        let flush = tracing::info_span!("flush-under-test");
        let (pending, recovered) = flush.in_scope(|| {
            (
                OutboundStanza::for_pending_flush(
                    message(),
                    PendingRowId::new("row-1"),
                    chrono::Utc::now(),
                ),
                OutboundStanza::new(message()).without_trace(),
            )
        });
        assert!(!pending.trace.is_traced());
        assert!(!recovered.trace.is_traced());
    }
}
