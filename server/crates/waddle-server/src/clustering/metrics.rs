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

use opentelemetry::metrics::{Counter, Gauge, Meter};
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
