//! XMPP metrics for observability.
//!
//! These metrics follow the naming conventions from ADR-0014.
//! Uses the global OpenTelemetry meter provider which must be initialized
//! by the host application (waddle-server).

use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};
use opentelemetry::KeyValue;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

static METER: OnceLock<Meter> = OnceLock::new();

/// Per-pod running total of MUC occupants (nicks) across every active
/// room on this process. Occupancy changes happen inside independent
/// per-room actors, so the pod-wide gauge value cannot be read cheaply
/// from any single room; instead each occupancy change contributes a
/// signed delta here and republishes the resulting total. See
/// [`adjust_muc_occupant_total`].
static MUC_OCCUPANT_TOTAL: AtomicI64 = AtomicI64::new(0);

fn meter() -> &'static Meter {
    METER.get_or_init(|| opentelemetry::global::meter("waddle-xmpp"))
}

// ============================================================================
// Counters (Cumulative)
// ============================================================================

/// Counter for XMPP stanzas processed.
pub fn stanzas_processed() -> Counter<u64> {
    meter()
        .u64_counter("xmpp.stanzas.processed")
        .with_description("Total XMPP stanzas processed")
        .with_unit("stanza")
        .build()
}

/// Counter for authentication attempts.
pub fn auth_attempts() -> Counter<u64> {
    meter()
        .u64_counter("xmpp.auth.attempts")
        .with_description("Total authentication attempts")
        .with_unit("attempt")
        .build()
}

/// Counter for MUC messages.
pub fn muc_messages() -> Counter<u64> {
    meter()
        .u64_counter("xmpp.muc.messages")
        .with_description("Total MUC messages sent")
        .with_unit("message")
        .build()
}

/// Counter for MUC presence events (joins/leaves).
pub fn muc_presence_events() -> Counter<u64> {
    meter()
        .u64_counter("xmpp.muc.presence")
        .with_description("Total MUC presence events (joins and leaves)")
        .with_unit("event")
        .build()
}

// ============================================================================
// Gauges (Current State)
// ============================================================================

/// Gauge for active XMPP connections.
pub fn connections_active() -> Gauge<i64> {
    meter()
        .i64_gauge("xmpp.connections.active")
        .with_description("Current number of active XMPP connections")
        .with_unit("connection")
        .build()
}

/// Gauge for active MUC rooms.
pub fn muc_rooms_active() -> Gauge<i64> {
    meter()
        .i64_gauge("xmpp.muc.rooms.active")
        .with_description("Current number of active MUC rooms")
        .with_unit("room")
        .build()
}

/// Gauge for MUC occupants.
pub fn muc_occupants() -> Gauge<i64> {
    meter()
        .i64_gauge("xmpp.muc.occupants")
        .with_description("Current number of MUC occupants")
        .with_unit("user")
        .build()
}

// ============================================================================
// Histograms (Latency)
// ============================================================================

/// Histogram for stanza processing latency.
pub fn stanza_latency() -> Histogram<f64> {
    meter()
        .f64_histogram("xmpp.stanza.latency")
        .with_description("XMPP stanza processing latency")
        .with_unit("ms")
        .build()
}

/// Gauge for actor mailbox depth.
pub fn actor_mailbox_depth() -> Gauge<i64> {
    meter()
        .i64_gauge("xmpp.actor.mailbox.depth")
        .with_description("Current actor mailbox depth")
        .with_unit("message")
        .build()
}

/// Histogram for actor mailbox request latency.
pub fn actor_mailbox_latency() -> Histogram<f64> {
    meter()
        .f64_histogram("xmpp.actor.mailbox.latency")
        .with_description("Actor mailbox request latency")
        .with_unit("ms")
        .build()
}

/// Counter for actor-path dropped requests.
pub fn actor_dropped_requests() -> Counter<u64> {
    meter()
        .u64_counter("xmpp.actor.requests.dropped")
        .with_description("Actor requests dropped due to backpressure or actor shutdown")
        .with_unit("request")
        .build()
}

/// Counter for actor-path mailbox/reply timeouts.
pub fn actor_request_timeouts() -> Counter<u64> {
    meter()
        .u64_counter("xmpp.actor.requests.timeout")
        .with_description("Actor requests timed out waiting on mailbox or reply")
        .with_unit("request")
        .build()
}

/// Counter for per-stanza dispatch wedge-backstop timeouts (#808).
///
/// Distinct axis from [`actor_request_timeouts`] (which is keyed by actor): this
/// counts inbound stanzas whose handler exceeded the per-connection
/// frame-loop budget, keyed by stanza kind and payload namespace, so the
/// namespace distribution reveals *which* handler family wedged.
pub fn stanza_handler_timeouts() -> Counter<u64> {
    meter()
        .u64_counter("xmpp.stanza.handler.timeout")
        .with_description("Inbound stanza handlers that exceeded the per-connection wedge backstop")
        .with_unit("stanza")
        .build()
}

/// Counter for actor restarts.
pub fn actor_restarts() -> Counter<u64> {
    meter()
        .u64_counter("xmpp.actor.restarts")
        .with_description("Actor restarts performed by runtime supervision policy")
        .with_unit("restart")
        .build()
}

// ============================================================================
// Metric Recording Helpers
// ============================================================================

/// Record a stanza being processed.
pub fn record_stanza(stanza_type: &str, direction: &str) {
    stanzas_processed().add(
        1,
        &[
            KeyValue::new("type", stanza_type.to_string()),
            KeyValue::new("direction", direction.to_string()),
        ],
    );
}

/// Record an authentication attempt.
pub fn record_auth_attempt(mechanism: &str, success: bool) {
    auth_attempts().add(
        1,
        &[
            KeyValue::new("mechanism", mechanism.to_string()),
            KeyValue::new("result", if success { "success" } else { "failure" }),
        ],
    );
}

/// Record connection count change.
pub fn record_connection_count(count: i64, transport: &str) {
    connections_active().record(count, &[KeyValue::new("transport", transport.to_string())]);
}

/// Publish the current number of active MUC rooms on this pod to the
/// `xmpp.muc.rooms.active` gauge. No attributes — the room JID is
/// unbounded and never a metric dimension.
pub fn record_muc_rooms_active(count: i64) {
    muc_rooms_active().record(count, &[]);
}

/// Record stanza processing latency in milliseconds.
pub fn record_stanza_latency(latency_ms: f64, stanza_type: &str) {
    stanza_latency().record(
        latency_ms,
        &[KeyValue::new("type", stanza_type.to_string())],
    );
}

/// Record actor mailbox depth.
pub fn record_actor_mailbox_depth(actor: &str, message_class: &str, depth: i64, max_capacity: i64) {
    actor_mailbox_depth().record(
        depth,
        &[
            KeyValue::new("actor", actor.to_string()),
            KeyValue::new("class", message_class.to_string()),
            KeyValue::new("max_capacity", max_capacity),
        ],
    );
}

/// Record actor request latency in milliseconds.
pub fn record_actor_mailbox_latency(
    actor: &str,
    operation: &str,
    message_class: &str,
    latency_ms: f64,
) {
    actor_mailbox_latency().record(
        latency_ms,
        &[
            KeyValue::new("actor", actor.to_string()),
            KeyValue::new("operation", operation.to_string()),
            KeyValue::new("class", message_class.to_string()),
        ],
    );
}

/// Record a dropped actor request.
pub fn record_actor_request_dropped(
    actor: &str,
    operation: &str,
    message_class: &str,
    reason: &str,
) {
    actor_dropped_requests().add(
        1,
        &[
            KeyValue::new("actor", actor.to_string()),
            KeyValue::new("operation", operation.to_string()),
            KeyValue::new("class", message_class.to_string()),
            KeyValue::new("reason", reason.to_string()),
        ],
    );
}

/// Record an actor request timeout.
pub fn record_actor_request_timeout(actor: &str, operation: &str, message_class: &str) {
    actor_request_timeouts().add(
        1,
        &[
            KeyValue::new("actor", actor.to_string()),
            KeyValue::new("operation", operation.to_string()),
            KeyValue::new("class", message_class.to_string()),
        ],
    );
}

/// Record a per-stanza dispatch wedge-backstop timeout (#808), keyed by stanza
/// kind (`iq`/`message`/`presence`) and the payload namespace.
pub fn record_stanza_handler_timeout(stanza_kind: &str, payload_ns: &str) {
    stanza_handler_timeouts().add(
        1,
        &[
            KeyValue::new("kind", stanza_kind.to_string()),
            KeyValue::new("payload_ns", payload_ns.to_string()),
        ],
    );
}

/// Record a supervised actor restart.
pub fn record_actor_restart(actor: &str, reason: &str) {
    actor_restarts().add(
        1,
        &[
            KeyValue::new("actor", actor.to_string()),
            KeyValue::new("reason", reason.to_string()),
        ],
    );
}

/// Record one MUC groupchat message accepted for room fanout.
pub fn record_muc_message() {
    muc_messages().add(1, &[]);
}

/// Record a MUC presence event (join or leave).
///
/// `event_type` is a bounded label (`"join"` / `"leave"`); the room JID
/// is deliberately not an attribute — it is unbounded and belongs on
/// spans/logs per the cardinality budget (`telemetry` module docs).
pub fn record_muc_presence(event_type: &str) {
    muc_presence_events().add(1, &[KeyValue::new("event", event_type.to_string())]);
}

/// Publish the pod-wide MUC occupant total to the `xmpp.muc.occupants`
/// gauge. The room JID is not an attribute (unbounded); this is the
/// single per-pod total across every active room.
pub fn record_muc_occupant_count(count: i64) {
    muc_occupants().record(count, &[]);
}

/// Apply a signed occupancy `delta` to the per-pod running total and
/// republish it to the occupants gauge. Call with `+1` when a join adds
/// a brand-new occupant, `-1` when a leave removes an occupant's last
/// session, and `0` for multi-session joins / partial leaves that leave
/// the occupant set unchanged (pass the pre/post occupant-count diff and
/// this holds automatically).
pub fn adjust_muc_occupant_total(delta: i64) {
    let total = MUC_OCCUPANT_TOTAL
        .fetch_add(delta, Ordering::Relaxed)
        .saturating_add(delta);
    record_muc_occupant_count(total);
}

// ============================================================================
// Extension Enrichment Metrics
// ============================================================================

/// Histogram for extension enrichment latency.
pub fn extension_enrichment_latency() -> Histogram<f64> {
    meter()
        .f64_histogram("xmpp.extensions.enrichment.latency")
        .with_description("Runtime extension link enrichment latency")
        .with_unit("ms")
        .build()
}

/// Counter for extension embeds added to messages.
pub fn extension_embeds_added() -> Counter<u64> {
    meter()
        .u64_counter("xmpp.extensions.embeds.added")
        .with_description("Extension embed elements added to messages")
        .with_unit("embed")
        .build()
}

/// Record an extension enrichment event.
pub fn record_extension_enrichment(latency_ms: f64, embeds: u64) {
    extension_enrichment_latency().record(latency_ms, &[]);
    if embeds > 0 {
        extension_embeds_added().add(embeds, &[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::test_support;

    #[tokio::test]
    async fn record_muc_presence_emits_bounded_event_label() {
        let guard = test_support::acquire().await;
        record_muc_presence("join");
        record_muc_presence("join");
        record_muc_presence("leave");

        assert_eq!(
            guard.counter_sum("xmpp.muc.presence", &[("event", "join")]),
            Some(2),
        );
        assert_eq!(
            guard.counter_sum("xmpp.muc.presence", &[("event", "leave")]),
            Some(1),
        );
    }

    #[tokio::test]
    async fn adjust_muc_occupant_total_publishes_running_total_gauge() {
        let guard = test_support::acquire().await;
        // A brand-new occupant, then a multi-session join (delta 0), then
        // a leave that removed the last session.
        adjust_muc_occupant_total(1);
        adjust_muc_occupant_total(0);
        adjust_muc_occupant_total(-1);

        assert!(
            guard
                .metric_names()
                .contains(&"xmpp.muc.occupants".to_string()),
            "occupants gauge must export after an adjustment",
        );
    }

    #[tokio::test]
    async fn record_muc_rooms_active_emits_gauge() {
        let guard = test_support::acquire().await;
        record_muc_rooms_active(3);

        assert!(
            guard
                .metric_names()
                .contains(&"xmpp.muc.rooms.active".to_string()),
            "rooms-active gauge must export after a recording",
        );
    }

    #[tokio::test]
    async fn record_connection_count_emits_transport_gauge() {
        let guard = test_support::acquire().await;
        record_connection_count(7, "websocket");

        assert!(
            guard
                .metric_names()
                .contains(&"xmpp.connections.active".to_string()),
            "connections gauge must export after a recording",
        );
    }

    #[tokio::test]
    async fn record_auth_attempt_labels_outcome() {
        let guard = test_support::acquire().await;
        record_auth_attempt("SCRAM-SHA-256", true);
        record_auth_attempt("SCRAM-SHA-256", false);

        assert_eq!(
            guard.counter_sum(
                "xmpp.auth.attempts",
                &[("mechanism", "SCRAM-SHA-256"), ("result", "success")],
            ),
            Some(1),
        );
        assert_eq!(
            guard.counter_sum(
                "xmpp.auth.attempts",
                &[("mechanism", "SCRAM-SHA-256"), ("result", "failure")],
            ),
            Some(1),
        );
    }

    #[tokio::test]
    async fn record_extension_enrichment_counts_only_added_embeds() {
        let guard = test_support::acquire().await;
        // A zero-embed pass records latency but must not touch the embed
        // counter; a two-embed pass adds two.
        record_extension_enrichment(1.5, 0);
        record_extension_enrichment(2.0, 2);

        assert_eq!(
            guard.counter_sum("xmpp.extensions.embeds.added", &[]),
            Some(2),
        );
        assert_eq!(
            guard.histogram_count("xmpp.extensions.enrichment.latency", &[]),
            Some(2),
        );
    }
}

// Note: GitHub API cache hit/miss and error metrics are tracked via structured
// tracing (warn!/debug! in extension runtime). OTel counters
// can be added here when the github crate gains an opentelemetry dependency.
