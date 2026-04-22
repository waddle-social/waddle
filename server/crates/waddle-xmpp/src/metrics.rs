//! XMPP metrics for observability.
//!
//! These metrics follow the naming conventions from ADR-0014.
//! Uses the global OpenTelemetry meter provider which must be initialized
//! by the host application (waddle-server).

use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};
use opentelemetry::KeyValue;
use std::sync::OnceLock;

static METER: OnceLock<Meter> = OnceLock::new();

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

/// Record a MUC presence event (join or leave).
pub fn record_muc_presence(event_type: &str, room: &str) {
    muc_presence_events().add(
        1,
        &[
            KeyValue::new("event", event_type.to_string()),
            KeyValue::new("room", room.to_string()),
        ],
    );
}

/// Update the MUC occupants gauge.
pub fn record_muc_occupant_count(count: i64, room: &str) {
    muc_occupants().record(count, &[KeyValue::new("room", room.to_string())]);
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

// Note: GitHub API cache hit/miss and error metrics are tracked via structured
// tracing (warn!/debug! in extension runtime). OTel counters
// can be added here when the github crate gains an opentelemetry dependency.

// ============================================================================
// S2S Metrics
// ============================================================================

/// Counter for S2S connection attempts.
pub fn s2s_connection_attempts() -> Counter<u64> {
    meter()
        .u64_counter("xmpp.s2s.connection.attempts")
        .with_description("Total S2S connection attempts")
        .with_unit("connection")
        .build()
}

/// Gauge for active S2S connections.
pub fn s2s_connections_active() -> Gauge<i64> {
    meter()
        .i64_gauge("xmpp.s2s.connections.active")
        .with_description("Current number of active S2S connections")
        .with_unit("connection")
        .build()
}

/// Counter for S2S TLS handshakes completed.
pub fn s2s_tls_handshakes() -> Counter<u64> {
    meter()
        .u64_counter("xmpp.s2s.tls.established")
        .with_description("Total S2S TLS handshakes completed")
        .with_unit("handshake")
        .build()
}

/// Record an S2S connection attempt.
pub fn record_s2s_connection_attempt() {
    s2s_connection_attempts().add(1, &[]);
}

/// Record S2S connection count change.
pub fn record_s2s_connection_count(count: i64) {
    s2s_connections_active().record(count, &[]);
}

/// Record S2S TLS handshake completion.
pub fn record_s2s_tls_established() {
    s2s_tls_handshakes().add(1, &[]);
}
