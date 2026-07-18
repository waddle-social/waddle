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
//! Tests assert **exported samples**, not internal state, through the
//! in-memory reader seam in [`test_support`] (gated behind
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

/// The meter every macro-created instrument attaches to. Resolved
/// from the global meter provider at instrument-creation time, so
/// production instruments bind to the OTLP pipeline installed by
/// `waddle-server::telemetry::init` and tests bind to the in-memory
/// reader from [`test_support`].
#[doc(hidden)]
pub fn meter() -> opentelemetry::metrics::Meter {
    opentelemetry::global::meter("waddle")
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
#[macro_export]
macro_rules! histogram_record {
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
