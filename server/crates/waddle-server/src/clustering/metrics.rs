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
