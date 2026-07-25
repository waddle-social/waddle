//! Create-at-increment OTel metrics — the single convention for new
//! Waddle metrics (spec #1323, foundation #1325).
//!
//! # Convention
//!
//! - **Creation and use are the same act.** The only way to emit a
//!   metric is [`counter_add!`] / [`histogram_record!`]; the first
//!   invocation lazily creates the instrument behind a `OnceLock`.
//!   There is no standalone registration API, so a
//!   registered-but-never-wired metric cannot exist by construction.
//! - **Names are dot.case** (`waddle.sm.unacked.evicted`), validated
//!   at compile time by [`validate_metric_name`]. The unit is never
//!   part of the name.
//! - **Units are UCUM** and set on the instrument (`"{message}"`,
//!   `"ms"`, `"By"`, `"1"`).
//! - **Attributes come only from the enumerated sets** in
//!   [`attributes`]. JIDs, room JIDs, stream ids, and message ids are
//!   never metric attributes — they belong on spans and logs.
//!
//! # Cardinality budget
//!
//! Every attribute is a closed enum defined in [`attributes`]; adding
//! an attribute value means editing that allowlist in review. An
//! instrument takes **at most two attribute dimensions** (review
//! enforces this; the janitor heartbeat's `janitor` × `outcome` = 18
//! series is the intended ceiling). Budget: ~70 instruments × ≤22
//! series each × 2 pods stays well under 5k active series — never
//! stack the larger enums (`condition` × `janitor` would be 198
//! series per instrument). Never implement
//! [`attributes::MetricAttribute`] for a type whose value space is
//! unbounded (user input, JIDs, ids of any kind).
//!
//! # Initialization order
//!
//! Instruments bind to the **global meter provider live at their
//! first increment** and are then cached for the process lifetime.
//! `waddle-server` installs the OTLP provider in `telemetry::init()`
//! as the first act of `main`, before any increment can run; keep it
//! that way — an increment that races ahead of `init()` binds to the
//! noop provider and is silently lost. In tests, acquire the
//! [`test_support`] guard before the first increment (under
//! `cargo nextest`, one process per test, this is automatic).
//!
//! # Testing
//!
//! Tests assert exported metrics and spans, never internal state, through
//! the in-memory seams in [`test_support`] (gated behind
//! `test`/`test-utils`).

pub mod attributes;
pub mod call;
pub mod messages;
pub mod push_pipeline;
pub mod reliability;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;
#[cfg(test)]
mod tests;

/// Macro internals. Not public API — only referenced from macro
/// expansions, which may live in downstream crates.
#[doc(hidden)]
pub mod __export {
    pub use opentelemetry::metrics::{Counter, Histogram};
    pub use opentelemetry::KeyValue;
    pub use std::sync::OnceLock;
}

/// Explicit bucket boundaries for histograms whose unit is seconds
/// (`"s"`), spanning 5ms to 10s — the seconds-scaled form of the
/// OpenTelemetry default advice.
///
/// Pass these with the `buckets:` form of [`histogram_record!`]: the
/// SDK default boundaries are millisecond-scale, so a seconds-unit
/// instrument left on them reports every sub-second observation in the
/// lowest bucket and a constant fake p99 (#1453).
pub const SECOND_SCALE_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// The meter every macro-created instrument attaches to. Resolved
/// from the global meter provider at instrument-creation time, so
/// production instruments bind to the OTLP pipeline installed by
/// `waddle-server::telemetry::init` and tests bind to the in-memory
/// reader from [`test_support`].
#[doc(hidden)]
pub fn meter() -> opentelemetry::metrics::Meter {
    opentelemetry::global::meter("waddle")
}

/// Mark the current protocol span as failed without coupling this crate to the
/// production OpenTelemetry tracing layer.
///
/// Callers must declare `otel.status_code` as an empty tracing field on their
/// operation span. `tracing-opentelemetry` interprets the recorded `ERROR`
/// value at the server boundary.
pub(crate) fn mark_span_error() {
    tracing::Span::current().record("otel.status_code", "ERROR");
}

/// Force-flush a meter provider without blocking the async runtime or
/// waiting longer than `timeout` for the SDK's synchronous flush.
///
/// Returns `true` only when the provider reports a successful flush
/// within `timeout`. The SDK flush is synchronous and offers no
/// cancellation, so on timeout it is abandoned on its own thread
/// rather than stopped: it runs on a dedicated detached thread — not
/// the runtime's bounded blocking pool — so an abandoned flush strands
/// only that thread (reaped at process exit) instead of pinning a
/// `spawn_blocking` slot for the rest of the process.
pub async fn force_flush_bounded(
    provider: &opentelemetry_sdk::metrics::SdkMeterProvider,
    timeout: std::time::Duration,
) -> bool {
    let provider = provider.clone();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("otel-metrics-flush".to_owned())
        .spawn(move || {
            let _ = result_tx.send(provider.force_flush());
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "Failed to spawn OpenTelemetry metrics flush thread");
        return false;
    }

    match tokio::time::timeout(timeout, result_rx).await {
        Ok(Ok(Ok(()))) => true,
        Ok(Ok(Err(error))) => {
            tracing::warn!(%error, "Failed to force-flush OpenTelemetry metrics");
            false
        }
        Ok(Err(_dropped)) => {
            tracing::warn!("OpenTelemetry metrics flush thread exited without reporting");
            false
        }
        Err(error) => {
            tracing::warn!(%error, "Timed out force-flushing OpenTelemetry metrics");
            false
        }
    }
}

/// Compile-time metric-name validation: dot.case, lowercase ascii
/// segments that start with a letter, digits and `_` allowed within a
/// segment, and no Prometheus-style `_total` suffix (the exporter adds
/// that at the wire boundary). Returns the name unchanged so the macro
/// can bind it in a `const`.
#[must_use]
pub const fn validate_metric_name(name: &str) -> &str {
    let bytes = name.as_bytes();
    assert!(!bytes.is_empty(), "metric name must not be empty");
    let mut i = 0;
    let mut segment_len = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'.' {
            assert!(
                segment_len > 0,
                "metric name must not have empty segments (leading, trailing, or doubled dots)"
            );
            segment_len = 0;
        } else if byte.is_ascii_lowercase() {
            segment_len += 1;
        } else if byte.is_ascii_digit() || byte == b'_' {
            assert!(
                segment_len > 0,
                "metric name segments must start with a lowercase letter"
            );
            segment_len += 1;
        } else {
            panic!(
                "metric names are dot.case: lowercase ascii letters, digits and '_' within \
                 segments, '.' between segments"
            );
        }
        i += 1;
    }
    assert!(segment_len > 0, "metric name must not end with '.'");

    const TOTAL_SUFFIX: &[u8] = b"_total";
    if bytes.len() >= TOTAL_SUFFIX.len() {
        let offset = bytes.len() - TOTAL_SUFFIX.len();
        let mut j = 0;
        let mut is_total = true;
        while j < TOTAL_SUFFIX.len() {
            if bytes[offset + j] != TOTAL_SUFFIX[j] {
                is_total = false;
                break;
            }
            j += 1;
        }
        assert!(
            !is_total,
            "metric names must not carry a Prometheus-style `_total` suffix; \
             the exporter appends it at the wire boundary"
        );
    }

    name
}

/// Add to a `u64` counter, creating the instrument on first use.
///
/// ```
/// use waddle_xmpp::counter_add;
/// use waddle_xmpp::telemetry::attributes::MessageKind;
///
/// counter_add!(
///     "waddle.messages.delivered",
///     "{message}",
///     "Message stanzas delivered to a local session.",
///     1,
///     MessageKind::Dm,
/// );
/// ```
///
/// The name is validated at compile time (dot.case, no `_total`
/// suffix); the unit is UCUM; attributes must implement
/// [`crate::telemetry::attributes::MetricAttribute`], which only the
/// enumerated allowlist types do.
#[macro_export]
macro_rules! counter_add {
    ($name:literal, $unit:literal, $desc:literal, $value:expr $(, $attr:expr)* $(,)?) => {{
        const _VALIDATED: &str = $crate::telemetry::validate_metric_name($name);
        static INSTRUMENT: $crate::telemetry::__export::OnceLock<
            $crate::telemetry::__export::Counter<u64>,
        > = $crate::telemetry::__export::OnceLock::new();
        let attributes: &[$crate::telemetry::__export::KeyValue] = &[
            $($crate::telemetry::attributes::MetricAttribute::key_value(&$attr)),*
        ];
        INSTRUMENT
            .get_or_init(|| {
                $crate::telemetry::meter()
                    .u64_counter($name)
                    .with_unit($unit)
                    .with_description($desc)
                    .build()
            })
            .add($value, attributes);
    }};
}

/// Record into an `f64` histogram, creating the instrument on first
/// use. Same naming/unit/attribute rules as [`counter_add!`].
///
/// The SDK's default buckets are millisecond-scale
/// (`[0, 5, 10, …, 10000]`); an instrument recording another scale
/// must pass its own boundaries with the `buckets:` form, e.g.
/// `histogram_record!(name, "s", desc, buckets: SECOND_SCALE_BUCKETS, value, attr)`
/// (see [`SECOND_SCALE_BUCKETS`]). Otherwise every real sample lands
/// in the lowest bucket and quantiles are constant (#1453).
#[macro_export]
macro_rules! histogram_record {
    (
        $name:literal, $unit:literal, $desc:literal,
        buckets: $buckets:expr, $value:expr $(, $attr:expr)* $(,)?
    ) => {{
        const _VALIDATED: &str = $crate::telemetry::validate_metric_name($name);
        static INSTRUMENT: $crate::telemetry::__export::OnceLock<
            $crate::telemetry::__export::Histogram<f64>,
        > = $crate::telemetry::__export::OnceLock::new();
        let attributes: &[$crate::telemetry::__export::KeyValue] = &[
            $($crate::telemetry::attributes::MetricAttribute::key_value(&$attr)),*
        ];
        INSTRUMENT
            .get_or_init(|| {
                $crate::telemetry::meter()
                    .f64_histogram($name)
                    .with_unit($unit)
                    .with_description($desc)
                    .with_boundaries($buckets.to_vec())
                    .build()
            })
            .record($value, attributes);
    }};
    ($name:literal, $unit:literal, $desc:literal, $value:expr $(, $attr:expr)* $(,)?) => {{
        const _VALIDATED: &str = $crate::telemetry::validate_metric_name($name);
        static INSTRUMENT: $crate::telemetry::__export::OnceLock<
            $crate::telemetry::__export::Histogram<f64>,
        > = $crate::telemetry::__export::OnceLock::new();
        let attributes: &[$crate::telemetry::__export::KeyValue] = &[
            $($crate::telemetry::attributes::MetricAttribute::key_value(&$attr)),*
        ];
        INSTRUMENT
            .get_or_init(|| {
                $crate::telemetry::meter()
                    .f64_histogram($name)
                    .with_unit($unit)
                    .with_description($desc)
                    .build()
            })
            .record($value, attributes);
    }};
}
