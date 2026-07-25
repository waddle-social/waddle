//! W3C trace context carried on clustering relay messages (#1485).
//!
//! kameo 0.20's remote messaging has no header channel, so cross-node
//! deliveries used to split into two disjoint traces: the sending node's
//! work, and the receiving node's `clustering.relay.dispatch` root (#1483).
//! This module adds an **optional, additive** trace-context field to the
//! relay message shapes so the receiving root can be parented on the
//! sender's span.
//!
//! Design constraints this encodes:
//!
//! * **Telemetry only.** Nothing here participates in relay semantics:
//!   the field is never read for routing, ordering, dedupe, fencing, or
//!   reply classification, and an absent/garbage value only ever costs a
//!   trace link — never a delivery.
//! * **Mixed-version safe.** kameo serializes remote messages with
//!   `rmp_serde::to_vec_named` (a MessagePack *map* keyed by field name)
//!   and decodes with `rmp_serde::decode::from_slice`. A field absent on
//!   the wire (old sender → new receiver) is filled by `#[serde(default)]`;
//!   an unknown field (new sender → old receiver) is skipped by serde's
//!   derived `Deserialize`. Both directions are pinned by tests.
//! * **W3C encoding is not hand-rolled.** [`TraceContextPropagator`] does
//!   the injecting and extracting; this module only owns the two-slot
//!   carrier and the typed wire wrappers.
//! * **Remote parent, deliberately.** Extraction yields a `SpanContext`
//!   with `is_remote = true`, which the #1438 span-noise sampler leaves to
//!   the delegate sampler — so parenting the dispatch root on it keeps the
//!   span alive, unlike a *local* parent (see
//!   [`super::relay::relay_dispatch_span`]'s `parent: None`, which stays
//!   the no-context fallback).

use opentelemetry::propagation::{Extractor, Injector, TextMapPropagator};
use opentelemetry::trace::TraceContextExt;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use serde::{Deserialize, Serialize};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// W3C carrier key for the parent span reference.
const TRACEPARENT_KEY: &str = "traceparent";
/// W3C carrier key for the vendor trace state.
const TRACESTATE_KEY: &str = "tracestate";

/// Upper bound on an accepted `traceparent`. Version `00` is exactly 55
/// characters; later versions keep those fields and may append opaque ones,
/// so the bound is generous but still refuses an unbounded peer-supplied
/// string reaching the propagator.
const MAX_TRACEPARENT_LEN: usize = 128;

/// Upper bound on an accepted `tracestate` — the limit the W3C
/// specification tells implementations to enforce.
const MAX_TRACESTATE_LEN: usize = 512;

/// A W3C `traceparent` value, only ever produced by
/// [`TraceContextPropagator`] injection or read back by its extraction.
///
/// The inner `String` is the wire encoding at the serialization boundary,
/// never a free-form field: it cannot be constructed from arbitrary text
/// without passing [`Self::new`]'s bound — including through serde, whose
/// [`Deserialize`] impl enforces the bound inside `visit_str`, *before*
/// the peer-controlled bytes are owned (a derived transparent impl would
/// allocate an arbitrarily large string first and check it only later).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TraceParentHeader(String);

impl TraceParentHeader {
    fn new(value: String) -> Option<Self> {
        (!value.is_empty() && value.len() <= MAX_TRACEPARENT_LEN).then_some(Self(value))
    }
}

impl<'de> Deserialize<'de> for TraceParentHeader {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer
            .deserialize_str(BoundedHeaderVisitor {
                what: "a traceparent header",
                max_len: MAX_TRACEPARENT_LEN,
            })
            .map(Self)
    }
}

/// A W3C `tracestate` value. Same contract as [`TraceParentHeader`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TraceStateHeader(String);

impl TraceStateHeader {
    fn new(value: String) -> Option<Self> {
        (!value.is_empty() && value.len() <= MAX_TRACESTATE_LEN).then_some(Self(value))
    }
}

impl<'de> Deserialize<'de> for TraceStateHeader {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer
            .deserialize_str(BoundedHeaderVisitor {
                what: "a tracestate header",
                max_len: MAX_TRACESTATE_LEN,
            })
            .map(Self)
    }
}

/// Checks the length bound on the borrowed input before the string is
/// owned, so an oversized peer-supplied header is refused without ever
/// being allocated. The refusal is a decode error: relay messages are
/// only exchanged between enrolled cluster peers, and the codec NACKs
/// (and counts) rejected payloads rather than dropping them silently.
struct BoundedHeaderVisitor {
    what: &'static str,
    max_len: usize,
}

impl serde::de::Visitor<'_> for BoundedHeaderVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "{} of 1..={} bytes", self.what, self.max_len)
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        if value.is_empty() || value.len() > self.max_len {
            return Err(E::invalid_length(value.len(), &self));
        }
        Ok(value.to_owned())
    }
}

/// The optional trace context carried alongside a relay message.
///
/// `Default` (both fields absent) is the "no context" value every
/// construction site starts from; [`super::relay::RelayHandle`] stamps the
/// real one at the single send seam, so no caller has to remember to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayTraceContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    traceparent: Option<TraceParentHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tracestate: Option<TraceStateHeader>,
}

impl RelayTraceContext {
    /// Capture the sending task's currently active span as a propagatable
    /// context. Returns the empty context when no span is active or the
    /// active span has no valid OpenTelemetry context (no tracer
    /// installed, or an unsampled/invalid parent) — the receiver then
    /// falls back to its own root span.
    pub fn capture() -> Self {
        Self::from_context(&tracing::Span::current().context())
    }

    fn from_context(cx: &opentelemetry::Context) -> Self {
        // An unsampled sender must NOT be propagated: a span the
        // `SpanNoiseFilter` dropped still has a *valid* SpanContext
        // (flags 00), and a remote-unsampled parent makes the receiving
        // node's `ParentBased` delegate drop `clustering.relay.dispatch`
        // and everything under it — undoing #1483 for exactly the
        // suppressed-sender traffic it protects. Falling back to the
        // empty context keeps the receiver's `parent: None` root.
        let span_context = cx.span().span_context().clone();
        if !span_context.is_valid() || !span_context.is_sampled() {
            return Self::default();
        }
        let mut carrier = RelayTraceCarrier::default();
        propagator().inject_context(cx, &mut carrier);
        Self {
            traceparent: carrier.traceparent.and_then(TraceParentHeader::new),
            tracestate: carrier.tracestate.and_then(TraceStateHeader::new),
        }
    }

    /// Parent `span` on the sending node's span context, if this envelope
    /// carried a valid one. A no-op otherwise, which leaves the span the
    /// fresh `parent: None` root its constructor made it.
    ///
    /// Called immediately after span construction and before the span is
    /// entered: `tracing-opentelemetry` can only re-parent a span whose
    /// builder has not been consumed yet.
    pub fn parent_span(&self, span: &tracing::Span) {
        if let Some(parent_cx) = self.remote_parent() {
            let _ = span.set_parent(parent_cx);
        }
    }

    /// The extracted remote parent context, or `None` when absent, out of
    /// bounds, unparsable, or unsampled (a peer running a build without
    /// the sampled-only send gate can still stamp flags `00` during a
    /// rolling deploy; parenting on it would drop the dispatch span).
    fn remote_parent(&self) -> Option<opentelemetry::Context> {
        let traceparent = self.traceparent.as_ref()?;
        if traceparent.0.len() > MAX_TRACEPARENT_LEN {
            return None;
        }
        let carrier = RelayTraceCarrier {
            traceparent: Some(traceparent.0.clone()),
            tracestate: self
                .tracestate
                .as_ref()
                .filter(|state| state.0.len() <= MAX_TRACESTATE_LEN)
                .map(|state| state.0.clone()),
        };
        let cx = propagator().extract(&carrier);
        {
            let span_context = cx.span().span_context().clone();
            (span_context.is_valid() && span_context.is_sampled()).then_some(cx)
        }
    }
}

/// A dedicated [`TraceContextPropagator`] rather than the process-global
/// one: the global propagator is only installed by the telemetry bootstrap,
/// so relying on it would make relay propagation silently depend on
/// initialization order (and vanish in tests). The W3C propagator is a
/// zero-sized, stateless value, so constructing one per call is free.
fn propagator() -> TraceContextPropagator {
    TraceContextPropagator::new()
}

/// The two-slot carrier the propagator injects into and extracts from.
/// Deliberately not a `HashMap`: the relay accepts exactly the two W3C
/// keys, so an unexpected key from a future propagator cannot smuggle
/// extra bytes onto the wire.
#[derive(Debug, Default)]
struct RelayTraceCarrier {
    traceparent: Option<String>,
    tracestate: Option<String>,
}

impl Injector for RelayTraceCarrier {
    fn set(&mut self, key: &str, value: String) {
        match key {
            TRACEPARENT_KEY => self.traceparent = Some(value),
            TRACESTATE_KEY => self.tracestate = Some(value),
            _ => {}
        }
    }
}

impl Extractor for RelayTraceCarrier {
    fn get(&self, key: &str) -> Option<&str> {
        match key {
            TRACEPARENT_KEY => self.traceparent.as_deref(),
            TRACESTATE_KEY => self.tracestate.as_deref(),
            _ => None,
        }
    }

    fn keys(&self) -> Vec<&str> {
        let mut keys = Vec::with_capacity(2);
        if self.traceparent.is_some() {
            keys.push(TRACEPARENT_KEY);
        }
        if self.tracestate.is_some() {
            keys.push(TRACESTATE_KEY);
        }
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};

    fn sampled_remote_context(trace_id: u128, span_id: u64) -> opentelemetry::Context {
        opentelemetry::Context::new().with_remote_span_context(SpanContext::new(
            TraceId::from(trace_id),
            SpanId::from(span_id),
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        ))
    }

    fn unsampled_remote_context(trace_id: u128, span_id: u64) -> opentelemetry::Context {
        opentelemetry::Context::new().with_remote_span_context(SpanContext::new(
            TraceId::from(trace_id),
            SpanId::from(span_id),
            TraceFlags::default(),
            true,
            TraceState::default(),
        ))
    }

    /// A sender span the `SpanNoiseFilter` dropped still has a valid
    /// SpanContext with flags 00; propagating it would make the
    /// receiver's ParentBased delegate drop `clustering.relay.dispatch`
    /// (adversarial-review finding on #1485).
    #[test]
    fn an_unsampled_sender_context_is_not_captured() {
        let captured =
            RelayTraceContext::from_context(&unsampled_remote_context(0xdead_beef, 0x0bad_cafe));
        assert_eq!(captured, RelayTraceContext::default());
        assert!(captured.remote_parent().is_none());
    }

    /// Rolling-deploy hardening: a peer without the sampled-only send
    /// gate can still stamp a flags-00 traceparent; the receiver must
    /// keep its `parent: None` root rather than parent on it.
    #[test]
    fn an_unsampled_traceparent_from_the_wire_falls_back_to_no_parent() {
        let stale_peer = RelayTraceContext {
            traceparent: TraceParentHeader::new(
                "00-123456789abcdef011223344556677ff-00ff00ff00ff00ff-00".to_string(),
            ),
            tracestate: None,
        };
        assert!(
            stale_peer.traceparent.is_some(),
            "header itself is well-formed"
        );
        assert!(stale_peer.remote_parent().is_none());
    }

    #[test]
    fn capture_of_an_empty_context_carries_nothing() {
        let captured = RelayTraceContext::from_context(&opentelemetry::Context::new());
        assert_eq!(captured, RelayTraceContext::default());
        assert!(captured.remote_parent().is_none());
    }

    #[test]
    fn inject_extract_round_trips_the_sender_span_context() {
        let sender = sampled_remote_context(0x1234_5678_9abc_def0_1122_3344_5566_7788, 0x00ff_00ff);
        let captured = RelayTraceContext::from_context(&sender);

        let extracted = captured
            .remote_parent()
            .expect("a valid sender context round-trips");
        let span_context = extracted.span().span_context().clone();
        assert_eq!(
            span_context.trace_id(),
            TraceId::from(0x1234_5678_9abc_def0_1122_3344_5566_7788)
        );
        assert_eq!(span_context.span_id(), SpanId::from(0x00ff_00ffu64));
        assert!(
            span_context.is_remote(),
            "the extracted parent must be remote so the #1438 sampler \
             delegates the decision instead of dropping the span"
        );
        assert!(span_context.is_sampled());
    }

    #[test]
    fn wire_round_trip_through_the_relay_codec_preserves_the_context() {
        let captured = RelayTraceContext::from_context(&sampled_remote_context(0xabc, 0xdef));
        let encoded = rmp_serde::to_vec_named(&captured).expect("context encodes");
        let decoded: RelayTraceContext = rmp_serde::from_slice(&encoded).expect("context decodes");
        assert_eq!(decoded, captured);
    }

    #[test]
    fn an_unparsable_traceparent_falls_back_to_no_parent() {
        let garbage = RelayTraceContext {
            traceparent: TraceParentHeader::new("not-a-traceparent".to_string()),
            tracestate: None,
        };
        assert!(garbage.remote_parent().is_none());
    }

    #[test]
    fn an_oversized_traceparent_is_refused_before_the_propagator() {
        assert!(TraceParentHeader::new("x".repeat(MAX_TRACEPARENT_LEN + 1)).is_none());
        assert!(TraceStateHeader::new("x".repeat(MAX_TRACESTATE_LEN + 1)).is_none());
        assert!(TraceParentHeader::new(String::new()).is_none());
    }

    /// The length bound must hold through serde too: a derived
    /// transparent impl would own an arbitrarily large peer-controlled
    /// string before any check ran (Greptile/codex review, PR #1487).
    #[test]
    fn an_oversized_header_is_refused_at_decode_time() {
        #[derive(Serialize)]
        #[serde(transparent)]
        struct RawHeader(String);

        let oversized = rmp_serde::to_vec_named(&RawHeader("x".repeat(MAX_TRACEPARENT_LEN + 1)))
            .expect("encodes");
        assert!(rmp_serde::from_slice::<TraceParentHeader>(&oversized).is_err());
        let empty = rmp_serde::to_vec_named(&RawHeader(String::new())).expect("encodes");
        assert!(rmp_serde::from_slice::<TraceParentHeader>(&empty).is_err());

        let oversized_state =
            rmp_serde::to_vec_named(&RawHeader("x".repeat(MAX_TRACESTATE_LEN + 1)))
                .expect("encodes");
        assert!(rmp_serde::from_slice::<TraceStateHeader>(&oversized_state).is_err());

        let ok = rmp_serde::to_vec_named(&RawHeader("00-abc-def-01".to_string())).expect("encodes");
        assert!(rmp_serde::from_slice::<TraceParentHeader>(&ok).is_ok());
    }
}
