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
    stanza_latency().record(latency_ms, &[KeyValue::new("type", stanza_type.to_string())]);
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
