//! Span-noise suppression at the sampling layer (#1438).
//!
//! Production Tempo ingest was ~95% noise: root `actor.handle_message`
//! spans minted by kameo for timer/self-check messages (72% of all
//! spans), kube-probe request spans, and whole-actor-life
//! `actor.lifecycle` spans that only export on actor death and wreck
//! duration-based queries. The kameo span families are created inside
//! the `kameo` crate behind its default `tracing` feature, so they
//! cannot be suppressed at the instrumentation site — this sampler is
//! the seam.
//!
//! Sampling a span out (rather than filtering at export) kills the
//! whole noise trace: the filter drops every span under an unsampled
//! parent itself, so no orphaned child spans reach Tempo no matter
//! which `OTEL_TRACES_SAMPLER` the operator dials.
//!
//! # Dedicated root spans (#1483)
//!
//! Dispatch paths that do real work with no upstream span open a named
//! root of their own so their `actor.handle_message` children are
//! parented and survive this filter. These names are load-bearing and
//! MUST NEVER be added to the suppression lists below:
//!
//! - `clustering.relay.dispatch` — one per inbound clustering relay
//!   message (`clustering::relay::relay_dispatch_span`); covers
//!   remote-relayed stanza delivery, cross-node resume steals, and
//!   remote-resource bookkeeping.
//! - `janitor.sweep` — one per janitor sweep tick
//!   (`server::session_janitors::janitor_sweep_span`), carrying the
//!   canonical `janitor` attribute value.
//! - `janitor.orphan_work` — one per orphan-reaper work-item attempt
//!   (`server::session_janitors::orphan_work_span`), linked — not
//!   parented — to the enqueuing sweep so retry queues can never hold
//!   a sweep root open.
//!
//! Pre-existing named roots (`xmpp.stanza.dispatch`, `xmpp.muc.fanout`,
//! `http_request`, `clustering.shutdown_drain`, ...) pass through the
//! same way: anything not on the lists delegates to the inner sampler.

use opentelemetry::{
    trace::{Link, SpanKind, TraceContextExt, TraceId},
    Context, KeyValue,
};
use opentelemetry_sdk::trace::{Sampler, SamplingDecision, SamplingResult, ShouldSample};

/// Span names dropped only when they would start a new root trace.
///
/// - `actor.handle_message`: kameo instruments every actor message;
///   inside a real dispatch these spans are useful and keep their
///   parent, but a timer/self-check message with no active parent
///   context mints a single-span root trace of a few microseconds.
/// - `health_check`: the DB/pool probe spans. Under a traced request
///   they stay children; once probe request spans are suppressed at
///   creation (`server::trace::make_request_span`) they would re-root
///   here, so root ones are dropped too.
const ROOT_SUPPRESSED_SPAN_NAMES: &[&str] = &["actor.handle_message", "health_check"];

/// Span names dropped unconditionally.
///
/// `actor.lifecycle` spans the actor's entire life (observed up to
/// 9.6 h in production), is invisible while the actor is alive, is
/// lost on crash, and skews every duration-based trace query. Actor
/// restarts are observable through `waddle.process.start_time` and
/// logs instead.
const ALWAYS_SUPPRESSED_SPAN_NAMES: &[&str] = &["actor.lifecycle"];

/// Wraps the env-configured sampler and drops the known-noise span
/// families before delegating every other decision to it.
#[derive(Clone, Debug)]
pub(crate) struct SpanNoiseFilter {
    inner: Sampler,
}

impl SpanNoiseFilter {
    pub(crate) fn new(inner: Sampler) -> Self {
        Self { inner }
    }
}

/// A span is a root when there is no parent context or the parent's
/// span context is invalid (the propagator's empty extraction).
fn has_valid_parent(parent_context: Option<&Context>) -> bool {
    parent_context.is_some_and(|cx| cx.span().span_context().is_valid())
}

/// A valid **local** parent that was itself dropped. Enforced here —
/// not left to the delegate — so a suppressed root can never leak
/// orphaned children when the operator dials a non-parent-based
/// sampler (`OTEL_TRACES_SAMPLER=always_on` / `traceidratio`):
/// `AlwaysOn` would re-sample the child of a dropped
/// `actor.handle_message` root and export it pointing at a parent span
/// that never exports.
///
/// Remote parents are excluded: an inbound `traceparent` with flags
/// `00` says the *caller* chose not to sample, and what happens then
/// is exactly the delegate sampler's decision to make (`ParentBased`
/// honors it, `AlwaysOn` deliberately overrides it). Only local
/// unsampled parents can be this filter's own suppressions.
fn has_unsampled_local_parent(parent_context: Option<&Context>) -> bool {
    parent_context.is_some_and(|cx| {
        let span = cx.span();
        let span_context = span.span_context();
        span_context.is_valid() && !span_context.is_sampled() && !span_context.is_remote()
    })
}

/// Mirror the SDK samplers: never set extra attributes, and preserve
/// the parent's trace state on the way out.
fn drop_result(parent_context: Option<&Context>) -> SamplingResult {
    SamplingResult {
        decision: SamplingDecision::Drop,
        attributes: Vec::new(),
        trace_state: parent_context
            .map(|cx| cx.span().span_context().trace_state().clone())
            .unwrap_or_default(),
    }
}

impl ShouldSample for SpanNoiseFilter {
    fn should_sample(
        &self,
        parent_context: Option<&Context>,
        trace_id: TraceId,
        name: &str,
        span_kind: &SpanKind,
        attributes: &[KeyValue],
        links: &[Link],
    ) -> SamplingResult {
        if ALWAYS_SUPPRESSED_SPAN_NAMES.contains(&name)
            || (ROOT_SUPPRESSED_SPAN_NAMES.contains(&name) && !has_valid_parent(parent_context))
            || has_unsampled_local_parent(parent_context)
        {
            return drop_result(parent_context);
        }
        self.inner
            .should_sample(parent_context, trace_id, name, span_kind, attributes, links)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceState, TracerProvider as _};
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::trace::{SdkTracerProvider, SpanData, SpanExporter};
    use tracing_subscriber::prelude::*;

    use super::*;

    fn sample(filter: &SpanNoiseFilter, parent: Option<&Context>, name: &str) -> SamplingDecision {
        filter
            .should_sample(
                parent,
                TraceId::from(1u128),
                name,
                &SpanKind::Internal,
                &[],
                &[],
            )
            .decision
    }

    fn parent_context_with(sampled: bool, remote: bool) -> Context {
        let flags = if sampled {
            TraceFlags::SAMPLED
        } else {
            TraceFlags::default()
        };
        Context::new().with_remote_span_context(SpanContext::new(
            TraceId::from(0xabcdu128),
            SpanId::from(0x1234u64),
            flags,
            remote,
            TraceState::default(),
        ))
    }

    /// A propagated (remote) parent — the shape `make_request_span`
    /// attaches from an inbound `traceparent`.
    fn parent_context(sampled: bool) -> Context {
        parent_context_with(sampled, true)
    }

    /// An in-process parent — the shape a span suppressed by this
    /// filter leaves behind for its children.
    fn local_parent_context(sampled: bool) -> Context {
        parent_context_with(sampled, false)
    }

    #[test]
    fn root_actor_handle_message_is_dropped() {
        let filter = SpanNoiseFilter::new(Sampler::AlwaysOn);
        assert_eq!(
            sample(&filter, None, "actor.handle_message"),
            SamplingDecision::Drop
        );
    }

    #[test]
    fn invalid_parent_context_still_counts_as_root() {
        // The W3C propagator extracts an empty (invalid) span context
        // when no traceparent header is present; that must not count
        // as a parent.
        let filter = SpanNoiseFilter::new(Sampler::AlwaysOn);
        let empty = Context::new();
        assert_eq!(
            sample(&filter, Some(&empty), "actor.handle_message"),
            SamplingDecision::Drop
        );
    }

    #[test]
    fn parented_actor_handle_message_is_delegated() {
        let filter = SpanNoiseFilter::new(Sampler::AlwaysOn);
        let parent = parent_context(true);
        assert_eq!(
            sample(&filter, Some(&parent), "actor.handle_message"),
            SamplingDecision::RecordAndSample
        );
    }

    #[test]
    fn root_health_check_is_dropped_but_parented_is_delegated() {
        let filter = SpanNoiseFilter::new(Sampler::AlwaysOn);
        assert_eq!(
            sample(&filter, None, "health_check"),
            SamplingDecision::Drop
        );
        let parent = parent_context(true);
        assert_eq!(
            sample(&filter, Some(&parent), "health_check"),
            SamplingDecision::RecordAndSample
        );
    }

    #[test]
    fn actor_lifecycle_is_dropped_even_with_sampled_parent() {
        let filter = SpanNoiseFilter::new(Sampler::AlwaysOn);
        let parent = parent_context(true);
        assert_eq!(
            sample(&filter, Some(&parent), "actor.lifecycle"),
            SamplingDecision::Drop
        );
        assert_eq!(
            sample(&filter, None, "actor.lifecycle"),
            SamplingDecision::Drop
        );
    }

    #[test]
    fn other_spans_delegate_to_the_inner_sampler() {
        let on = SpanNoiseFilter::new(Sampler::AlwaysOn);
        assert_eq!(
            sample(&on, None, "xmpp.stanza.dispatch"),
            SamplingDecision::RecordAndSample
        );
        let off = SpanNoiseFilter::new(Sampler::AlwaysOff);
        assert_eq!(
            sample(&off, None, "xmpp.stanza.dispatch"),
            SamplingDecision::Drop
        );
    }

    /// #1483: the dedicated dispatch/sweep roots exist precisely so work
    /// that used to root at a suppressed `actor.handle_message` stays
    /// traceable — the filter must pass them straight to the delegate
    /// and they must never appear on a suppression list.
    #[test]
    fn dedicated_root_span_names_are_never_suppressed() {
        const DEDICATED_ROOT_SPAN_NAMES: &[&str] = &[
            "clustering.relay.dispatch",
            "janitor.sweep",
            "janitor.orphan_work",
        ];
        let filter = SpanNoiseFilter::new(Sampler::AlwaysOn);
        for name in DEDICATED_ROOT_SPAN_NAMES {
            assert!(
                !ROOT_SUPPRESSED_SPAN_NAMES.contains(name)
                    && !ALWAYS_SUPPRESSED_SPAN_NAMES.contains(name),
                "{name} is a dedicated root span and must never be suppressed"
            );
            assert_eq!(
                sample(&filter, None, name),
                SamplingDecision::RecordAndSample,
                "root {name} must delegate to the inner sampler"
            );
        }
    }

    #[test]
    fn children_of_unsampled_parents_stay_dropped_via_parent_based_delegate() {
        // Production wires ParentBased(...) as the inner sampler, so a
        // child created under a suppressed (unsampled) parent — e.g. a
        // handler span inside a suppressed probe request — inherits
        // the drop.
        let filter = SpanNoiseFilter::new(Sampler::ParentBased(Box::new(Sampler::AlwaysOn)));
        let parent = parent_context(false);
        assert_eq!(
            sample(&filter, Some(&parent), "db.query"),
            SamplingDecision::Drop
        );
    }

    #[test]
    fn orphans_are_prevented_even_under_non_parent_based_samplers() {
        // With OTEL_TRACES_SAMPLER=always_on the delegate would happily
        // re-sample the child of a suppressed root and export it as an
        // orphan; the filter must enforce consistency for LOCAL
        // unsampled parents itself.
        let filter = SpanNoiseFilter::new(Sampler::AlwaysOn);
        let parent = local_parent_context(false);
        assert_eq!(
            sample(&filter, Some(&parent), "db.query"),
            SamplingDecision::Drop
        );
    }

    #[test]
    fn remote_unsampled_parents_stay_the_delegates_decision() {
        // An inbound traceparent with flags 00 is the CALLER's sampling
        // choice, not one of this filter's suppressions: always_on is
        // documented to override it, and the filter must not veto that.
        let filter = SpanNoiseFilter::new(Sampler::AlwaysOn);
        let parent = parent_context(false);
        assert_eq!(
            sample(&filter, Some(&parent), "http_request"),
            SamplingDecision::RecordAndSample
        );
        // The default parent-based delegate keeps honoring it.
        let parent_based = SpanNoiseFilter::new(Sampler::ParentBased(Box::new(Sampler::AlwaysOn)));
        assert_eq!(
            sample(&parent_based, Some(&parent), "http_request"),
            SamplingDecision::Drop
        );
    }

    #[derive(Clone, Debug)]
    struct CaptureSpanExporter(Arc<Mutex<Vec<SpanData>>>);

    impl SpanExporter for CaptureSpanExporter {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            self.0.lock().expect("capture lock").extend(batch);
            Ok(())
        }
    }

    /// End-to-end through the real `tracing` → tracing-opentelemetry →
    /// SDK pipeline: the production wiring (`SpanNoiseFilter` around a
    /// parent-based sampler) suppresses a root `actor.handle_message`
    /// span and its children, while the same span under a real parent
    /// exports together with that parent.
    #[test]
    fn pipeline_drops_root_actor_spans_but_keeps_parented_ones() {
        let exported = Arc::new(Mutex::new(Vec::new()));
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(CaptureSpanExporter(Arc::clone(&exported)))
            .with_sampler(SpanNoiseFilter::new(Sampler::ParentBased(Box::new(
                Sampler::AlwaysOn,
            ))))
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("span-noise-test")));

        tracing::subscriber::with_default(subscriber, || {
            // Timer/self-check shape: a root actor span with a child.
            let root_actor = tracing::info_span!(parent: None, "actor.handle_message");
            root_actor.in_scope(|| {
                tracing::info_span!("db.query").in_scope(|| {});
            });
            drop(root_actor);

            // Real-dispatch shape: the same span name under a root
            // dispatch span must survive.
            let dispatch = tracing::info_span!(parent: None, "xmpp.stanza.dispatch");
            dispatch.in_scope(|| {
                tracing::info_span!("actor.handle_message").in_scope(|| {});
            });
            drop(dispatch);
        });

        let names: Vec<String> = exported
            .lock()
            .expect("capture lock")
            .iter()
            .map(|span| span.name.to_string())
            .collect();
        assert!(
            names.contains(&"xmpp.stanza.dispatch".to_string()),
            "real dispatch trace must export: {names:?}"
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| *name == "actor.handle_message")
                .count(),
            1,
            "only the parented actor span may export: {names:?}"
        );
        assert!(
            !names.contains(&"db.query".to_string()),
            "children of a suppressed root must not export as orphans: {names:?}"
        );
    }

    /// #1483 acceptance: the dedicated relay/janitor roots export through
    /// the production sampler wiring, and the `actor.handle_message` work
    /// under them is parented — it survives instead of being dropped as a
    /// root the way it was before those paths opened named roots.
    #[test]
    fn pipeline_exports_dedicated_roots_with_parented_actor_children() {
        let exported = Arc::new(Mutex::new(Vec::new()));
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(CaptureSpanExporter(Arc::clone(&exported)))
            .with_sampler(SpanNoiseFilter::new(Sampler::ParentBased(Box::new(
                Sampler::AlwaysOn,
            ))))
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("span-noise-test")));

        tracing::subscriber::with_default(subscriber, || {
            // `parent: None` mirrors the production spans: the relay spans
            // are minted inside kameo's own suppressed root.
            let relay = tracing::info_span!(parent: None, "clustering.relay.dispatch");
            relay.in_scope(|| {
                tracing::info_span!("actor.handle_message").in_scope(|| {});
            });
            drop(relay);

            let sweep = tracing::info_span!(parent: None, "janitor.sweep");
            sweep.in_scope(|| {
                tracing::info_span!("actor.handle_message").in_scope(|| {});
            });
            drop(sweep);
        });

        let spans = exported.lock().expect("capture lock").clone();
        let names: Vec<&str> = spans.iter().map(|span| span.name.as_ref()).collect();
        for root_name in ["clustering.relay.dispatch", "janitor.sweep"] {
            let root = spans
                .iter()
                .find(|span| span.name == root_name)
                .unwrap_or_else(|| panic!("{root_name} must export: {names:?}"));
            assert!(
                spans.iter().any(|span| span.name == "actor.handle_message"
                    && span.parent_span_id == root.span_context.span_id()),
                "actor work under {root_name} must export parented to it: {names:?}"
            );
        }
        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "actor.handle_message")
                .count(),
            2,
            "exactly the two parented actor spans may export: {names:?}"
        );
    }

    #[test]
    fn drop_result_preserves_parent_trace_state() {
        let trace_state = TraceState::from_key_value([("vendor", "value")]).expect("trace state");
        let parent = Context::new().with_remote_span_context(SpanContext::new(
            TraceId::from(0xabcdu128),
            SpanId::from(0x1234u64),
            TraceFlags::SAMPLED,
            true,
            trace_state.clone(),
        ));
        let filter = SpanNoiseFilter::new(Sampler::AlwaysOn);
        let result = filter.should_sample(
            Some(&parent),
            TraceId::from(1u128),
            "actor.lifecycle",
            &SpanKind::Internal,
            &[],
            &[],
        );
        assert_eq!(result.decision, SamplingDecision::Drop);
        assert_eq!(result.trace_state.get("vendor"), Some("value"));
    }
}
