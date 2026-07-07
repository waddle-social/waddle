//! Observability for the clustering swarm subsystem (ADR-0017 Phase 2).
//!
//! Uses the global OpenTelemetry meter provider (initialized by the host in
//! `telemetry.rs`), mirroring `waddle_xmpp::metrics`. Three instruments this
//! phase:
//! - a connected-peer gauge (authoritative from swarm connection events),
//! - a kademlia routing-table-size gauge (observed from kameo registry
//!   `RoutingUpdated` events — the `kademlia` field itself is private in kameo
//!   0.20, so this is the peer set we have seen enter the routing table), and
//! - a bootstrap-dial retry counter.
//!
//! ADR-0017 Phase 3's element-12 load-model instruments land here alongside
//! their first callers, per the repo's dead-code hard rule — not
//! forward-declared ahead of time. `PostgresClaimStore::fence` has zero
//! production callers this slice (Slice 1 lands the method, not a wired
//! caller), so its claims-table point-read counter does not land here
//! either — same reasoning, one level up: the instrument arrives with
//! `fence`'s first production caller in a later slice. Routing-cache
//! hit/miss and NotOwner NACK sent/received have no caller yet
//! (cross-node stanza routing is out of Phase 3 scope) and land later too.

use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};
use std::sync::OnceLock;

static METER: OnceLock<Meter> = OnceLock::new();

fn meter() -> &'static Meter {
    METER.get_or_init(|| opentelemetry::global::meter("waddle-clustering"))
}

/// Set the current number of connected swarm peers.
pub fn record_connected_peers(count: i64) {
    static G: OnceLock<Gauge<i64>> = OnceLock::new();
    G.get_or_init(|| {
        meter()
            .i64_gauge("waddle.clustering.connected_peers")
            .with_description("Current number of connected clustering swarm peers")
            .with_unit("peer")
            .build()
    })
    .record(count, &[]);
}

/// Set the current size of the (observed) kademlia routing table.
pub fn record_routing_table_size(count: i64) {
    static G: OnceLock<Gauge<i64>> = OnceLock::new();
    G.get_or_init(|| {
        meter()
            .i64_gauge("waddle.clustering.routing_table_size")
            .with_description("Observed kademlia routing-table peer count")
            .with_unit("peer")
            .build()
    })
    .record(count, &[]);
}

/// Increment the bootstrap-dial retry counter (one per dial attempt to a
/// headless-DNS-resolved seed peer).
pub fn record_bootstrap_dial() {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.bootstrap_dials")
            .with_description("Total bootstrap dial attempts to headless-DNS peers")
            .with_unit("dial")
            .build()
    })
    .add(1, &[]);
}

/// Set the current number of enrolled allowlist peers.
pub fn record_allowlist_size(count: i64) {
    static G: OnceLock<Gauge<i64>> = OnceLock::new();
    G.get_or_init(|| {
        meter()
            .i64_gauge("waddle.clustering.allowlist_size")
            .with_description("Currently enrolled clustering peer-allowlist entries")
            .with_unit("peer")
            .build()
    })
    .record(count, &[]);
}

/// Count a remote-codec decode rejection (bounds violation or re-parse
/// failure). `reason` is a stable low-cardinality label
/// (`too_large`/`too_deep`/`too_many_attributes`/`malformed`/`not_a_stanza`/
/// `serialize`).
pub fn record_remote_codec_drop(reason: &'static str) {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.remote_codec_drops")
            .with_description("Remote payloads rejected by the XML codec (NACKed, never silent)")
            .with_unit("payload")
            .build()
    })
    .add(1, &[opentelemetry::KeyValue::new("reason", reason)]);
}

/// Count a supervised relay-actor respawn (unexpected stop + mandatory
/// same-name re-registration).
pub fn record_relay_respawn() {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.relay_respawns")
            .with_description("Supervised relay-actor respawns with same-name re-registration")
            .with_unit("respawn")
            .build()
    })
    .add(1, &[]);
}

/// Set the current age (in milliseconds) since this node's last
/// successfully committed node-lease heartbeat (ADR-0017 Phase 3 Slice 2,
/// element 12: "a heartbeat-write-latency histogram + alert watches the
/// cause, not just the heartbeat-age symptom" — this gauge is the symptom
/// side). First caller: `self_fence::run_node_lease`.
pub fn record_node_heartbeat_age_ms(age_ms: f64) {
    static G: OnceLock<Gauge<f64>> = OnceLock::new();
    G.get_or_init(|| {
        meter()
            .f64_gauge("waddle.clustering.node_heartbeat_age_ms")
            .with_description("Milliseconds since this node's last successful node-lease heartbeat")
            .with_unit("ms")
            .build()
    })
    .record(age_ms, &[]);
}

/// Record one node-lease heartbeat CAS statement's wall-clock latency (pool
/// wait + statement execution) — the cause-side instrument element 12
/// names alongside the age gauge above. First caller:
/// `self_fence::run_node_lease`.
pub fn record_node_heartbeat_write_latency_ms(latency_ms: f64) {
    static H: OnceLock<Histogram<f64>> = OnceLock::new();
    H.get_or_init(|| {
        meter()
            .f64_histogram("waddle.clustering.node_heartbeat_write_latency_ms")
            .with_description("Node-lease heartbeat CAS statement latency (pool wait + execution)")
            .with_unit("ms")
            .build()
    })
    .record(latency_ms, &[]);
}

/// Count peers revoked by an allowlist refresh (live connections closed).
pub fn record_peers_revoked(count: u64) {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.peers_revoked")
            .with_description("Peers revoked by allowlist refresh (live connections closed)")
            .with_unit("peer")
            .build()
    })
    .add(count, &[]);
}

/// Record one graceful-shutdown drain's total wall-clock duration (ADR-0017
/// Phase 3 Slice 10 — the ADR's own Phase 3 Implementation Plan drain text,
/// NOT element 12, which is unrelated DB pool-size/capacity-planning
/// configurability): from the moment `mark_draining` is issued to the
/// moment the batched `release_many` call (or the abandonment path)
/// completes. First caller: `clustering::drain::run_shutdown_drain`.
/// Compare against `claimReleaseBudget` (`WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS`)
/// to see how close a deploy's drains run to the configured budget.
pub fn record_drain_duration_ms(duration_ms: f64) {
    static H: OnceLock<Histogram<f64>> = OnceLock::new();
    H.get_or_init(|| {
        meter()
            .f64_histogram("waddle.clustering.drain_duration_ms")
            .with_description(
                "Wall-clock duration of a graceful per-entity claim drain, from mark_draining \
                 to the batched release_many call (or abandonment) completing",
            )
            .with_unit("ms")
            .build()
    })
    .record(duration_ms, &[]);
}

/// Count claims successfully released as part of a graceful drain (ADR-0017
/// Phase 3 Slice 10 — the ADR's own Phase 3 Implementation Plan drain
/// text, not element 12; see [`record_drain_duration_ms`]'s doc comment).
/// First caller: `clustering::drain::run_shutdown_drain`.
pub fn record_claims_released_on_drain(count: u64) {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.claims_released_on_drain")
            .with_description("Claims successfully released as part of a graceful drain")
            .with_unit("claim")
            .build()
    })
    .add(count, &[]);
}

/// Count claims left held (un-released) because the graceful drain's budget
/// overran, a per-entity seal failed/timed out, or the batched
/// `release_many` call itself errored (ADR-0017 Phase 3 Slice 10 — the
/// ADR's own Phase 3 Implementation Plan drain text, not element 12; see
/// [`record_drain_duration_ms`]'s doc comment). Fenced-safe either way —
/// an abandoned claim is simply reclaimed
/// later by another node's orphan reaper, or by this node itself on its
/// next successful re-registration. **Any nonzero value here is alert
/// -worthy** (the phase plan's own "alert on nonzero abandonment"
/// deliverable): unlike a released claim, an abandoned one silently
/// degrades the "~1 move per entity per deploy" property until the
/// abandoned claim's lease naturally lapses. First caller:
/// `clustering::drain::run_shutdown_drain`.
pub fn record_claims_abandoned_on_drain(count: u64) {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.claims_abandoned_on_drain")
            .with_description(
                "Claims left held (not released) when a graceful drain's budget overran or a \
                 per-entity seal/release failed — alert on any nonzero value",
            )
            .with_unit("claim")
            .build()
    })
    .add(count, &[]);
}
