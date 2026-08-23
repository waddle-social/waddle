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

use super::ordered_relay::{OrderedRelayClaimRole, OrderedRelayNackReason};

static METER: OnceLock<Meter> = OnceLock::new();

fn meter() -> &'static Meter {
    METER.get_or_init(|| opentelemetry::global::meter("waddle-clustering"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteCodecDropReason {
    TooLarge,
    TooDeep,
    TooManyAttributes,
    Malformed,
    NotAStanza,
    Serialize,
}

impl RemoteCodecDropReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::TooDeep => "too_deep",
            Self::TooManyAttributes => "too_many_attributes",
            Self::Malformed => "malformed",
            Self::NotAStanza => "not_a_stanza",
            Self::Serialize => "serialize",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderedRelayNackMetricReason {
    Gap,
    InFlight,
    NotOwnerOrigin,
    NotOwnerSender,
    NotOwnerTarget,
    Unreachable,
    TargetUnavailable,
    ParseFailure,
    UnsupportedEnvelope,
    Backpressure,
    MaybeCommitted,
    Diverted,
}

impl OrderedRelayNackMetricReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gap => "gap",
            Self::InFlight => "in_flight",
            Self::NotOwnerOrigin => "not_owner_origin",
            Self::NotOwnerSender => "not_owner_sender",
            Self::NotOwnerTarget => "not_owner_target",
            Self::Unreachable => "unreachable",
            Self::TargetUnavailable => "target_unavailable",
            Self::ParseFailure => "parse_failure",
            Self::UnsupportedEnvelope => "unsupported_envelope",
            Self::Backpressure => "backpressure",
            Self::MaybeCommitted => "maybe_committed",
            Self::Diverted => "diverted",
        }
    }

    #[must_use]
    pub const fn from_nack_reason(reason: &OrderedRelayNackReason) -> Self {
        match reason {
            OrderedRelayNackReason::Gap { .. } => Self::Gap,
            OrderedRelayNackReason::InFlight => Self::InFlight,
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Origin,
            } => Self::NotOwnerOrigin,
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Sender,
            } => Self::NotOwnerSender,
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Target,
            } => Self::NotOwnerTarget,
            OrderedRelayNackReason::Unreachable => Self::Unreachable,
            OrderedRelayNackReason::TargetUnavailable => Self::TargetUnavailable,
            OrderedRelayNackReason::ParseFailure => Self::ParseFailure,
            OrderedRelayNackReason::UnsupportedEnvelope => Self::UnsupportedEnvelope,
            OrderedRelayNackReason::Backpressure => Self::Backpressure,
            OrderedRelayNackReason::MaybeCommitted => Self::MaybeCommitted,
            OrderedRelayNackReason::Diverted(_) => Self::Diverted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanWorkQueue {
    SmHydration,
    RoomRelease,
    RoomHandoff,
    RoomAdoption,
}

impl OrphanWorkQueue {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SmHydration => "sm_hydration",
            Self::RoomRelease => "room_release",
            Self::RoomHandoff => "room_handoff",
            Self::RoomAdoption => "room_adoption",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanWorker {
    Sweep,
    SmHydration,
    RoomRelease,
}

impl OrphanWorker {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sweep => "sweep",
            Self::SmHydration => "sm_hydration",
            Self::RoomRelease => "room_release",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanWorkerFailureReason {
    Timeout,
    Panic,
    Cancelled,
}

impl OrphanWorkerFailureReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Panic => "panic",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanTerminalCleanupLane {
    SmHydration,
    RoomRelease,
}

impl OrphanTerminalCleanupLane {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SmHydration => "sm_hydration",
            Self::RoomRelease => "room_release",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanTerminalCleanupFailureReason {
    Error,
    Timeout,
}

impl OrphanTerminalCleanupFailureReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomOrphanReconciliationOutcome {
    Hydrated,
    Released,
    AlreadyLive,
    PendingRetry,
    LostRace,
    Failed,
}

impl RoomOrphanReconciliationOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hydrated => "hydrated",
            Self::Released => "released",
            Self::AlreadyLive => "already_live",
            Self::PendingRetry => "pending_retry",
            Self::LostRace => "lost_race",
            Self::Failed => "failed",
        }
    }
}

/// Set the current number of connected swarm peers.
pub fn record_connected_peers(count: i64) {
    static G: OnceLock<Gauge<i64>> = OnceLock::new();
    G.get_or_init(|| {
        meter()
            .i64_gauge("waddle.clustering.connected_peers")
            .with_description("Current number of connected clustering swarm peers")
            .with_unit("{peer}")
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
            .with_unit("{peer}")
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
            .with_unit("{dial}")
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
            .with_unit("{peer}")
            .build()
    })
    .record(count, &[]);
}

/// Count a remote-codec decode rejection (bounds violation or re-parse
/// failure). `reason` is a stable low-cardinality label
/// (`too_large`/`too_deep`/`too_many_attributes`/`malformed`/`not_a_stanza`/
/// `serialize`).
pub fn record_remote_codec_drop(reason: RemoteCodecDropReason) {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.remote_codec_drops")
            .with_description("Remote payloads rejected by the XML codec (NACKed, never silent)")
            .with_unit("{payload}")
            .build()
    })
    .add(
        1,
        &[opentelemetry::KeyValue::new("reason", reason.as_str())],
    );
}

/// Count a supervised relay-actor respawn (unexpected stop + mandatory
/// same-name re-registration).
pub fn record_relay_respawn() {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.relay_respawns")
            .with_description("Supervised relay-actor respawns with same-name re-registration")
            .with_unit("{respawn}")
            .build()
    })
    .add(1, &[]);
}

/// Count ordered relay ACKs produced by the Slice 2 receiver substrate.
pub fn record_ordered_relay_ack() {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.ordered_relay_acks")
            .with_description("Internal ordered-relay ACK replies")
            .with_unit("{reply}")
            .build()
    })
    .add(1, &[]);
}

/// Count ordered relay NACKs produced by the Slice 2 receiver substrate.
/// `reason` is a stable low-cardinality label.
pub fn record_ordered_relay_nack(reason: OrderedRelayNackMetricReason) {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.ordered_relay_nacks")
            .with_description("Internal ordered-relay NACK replies")
            .with_unit("{reply}")
            .build()
    })
    .add(
        1,
        &[opentelemetry::KeyValue::new("reason", reason.as_str())],
    );
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
            .f64_gauge("waddle.clustering.node_heartbeat_age")
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
            .f64_histogram("waddle.clustering.node_heartbeat_write_latency")
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
            .with_unit("{peer}")
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
            .f64_histogram("waddle.clustering.drain_duration")
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
            .with_unit("{claim}")
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
fn add_claims_abandoned_on_drain(count: u64) {
    waddle_xmpp::counter_add!(
        "waddle.clustering.claims_abandoned_on_drain",
        "{claim}",
        "Claims left held (not released) when a graceful drain's budget overran or a \
         per-entity seal/release failed -- alert on any nonzero value",
        count,
    );
}

pub fn record_claims_abandoned_on_drain(count: u64) {
    add_claims_abandoned_on_drain(count);
}

pub fn register_clustering_counters() {
    add_claims_abandoned_on_drain(0);
}

/// Count bounded proactive `RoomActor` orphan-reconciliation outcomes.
/// `outcome` is one of `hydrated`, `released`, `already_live`,
/// `pending_retry`, `lost_race`, or `failed`; callers
/// aggregate a whole sweep before recording, avoiding per-room warning noise
/// after a node loss.
pub fn record_room_orphan_reconciliation(outcome: RoomOrphanReconciliationOutcome, count: u64) {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.room_orphan_reconciliations")
            .with_description("Proactive RoomActor orphan-claim reconciliation outcomes")
            .with_unit("{claim}")
            .build()
    })
    .add(
        count,
        &[opentelemetry::KeyValue::new("outcome", outcome.as_str())],
    );
}

pub fn record_room_orphan_pending_backlog(depth: usize, oldest_age_ms: u64) {
    static DEPTH: OnceLock<Gauge<u64>> = OnceLock::new();
    DEPTH
        .get_or_init(|| {
            meter()
                .u64_gauge("waddle.clustering.room_orphan_pending_depth")
                .with_description("Reclaimed room epochs awaiting adoption or release")
                .with_unit("{claim}")
                .build()
        })
        .record(depth as u64, &[]);
    static AGE: OnceLock<Gauge<u64>> = OnceLock::new();
    AGE.get_or_init(|| {
        meter()
            .u64_gauge("waddle.clustering.room_orphan_pending_oldest_age")
            .with_description("Age of the oldest pending reclaimed room epoch")
            .with_unit("ms")
            .build()
    })
    .record(oldest_age_ms, &[]);
}

/// Report the bounded orphan-reaper work queues. `queue` is the stable
/// low-cardinality value `sm_hydration`, `room_release`, or `room_handoff`.
pub fn record_orphan_work_queue_depth(queue: OrphanWorkQueue, depth: usize) {
    static G: OnceLock<Gauge<u64>> = OnceLock::new();
    G.get_or_init(|| {
        meter()
            .u64_gauge("waddle.clustering.orphan_work_queue_depth")
            .with_description("Current deduplicated orphan-reaper work queue depth")
            .with_unit("{item}")
            .build()
    })
    .record(
        depth as u64,
        &[opentelemetry::KeyValue::new("queue", queue.as_str())],
    );
}

/// Count work rejected by a bounded orphan-reaper queue or reservation gate.
/// `queue` is one of the stable low-cardinality values `sm_hydration`,
/// `room_release`, or `room_adoption`. A nonzero value means ownership
/// remains fenced-safe but recovery is deferred to a later sweep or
/// node-incarnation expiry.
pub fn record_orphan_work_queue_backpressure(queue: OrphanWorkQueue) {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.orphan_work_queue_backpressure")
            .with_description("Orphan-reaper work rejected because its bounded queue was full")
            .with_unit("{item}")
            .build()
    })
    .add(1, &[opentelemetry::KeyValue::new("queue", queue.as_str())]);
}

pub fn record_orphan_worker_failure(worker: OrphanWorker, reason: OrphanWorkerFailureReason) {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.orphan_worker_failures")
            .with_description("Orphan-reaper worker tasks that exited unexpectedly")
            .with_unit("{failure}")
            .build()
    })
    .add(
        1,
        &[
            opentelemetry::KeyValue::new("worker", worker.as_str()),
            opentelemetry::KeyValue::new("reason", reason.as_str()),
        ],
    );
}

/// Count terminal orphan-cleanup attempts that could not prove completion.
/// `lane` is the stable low-cardinality value `sm_hydration` or
/// `room_release`; `reason` is `error` or `timeout`. Unlike queue depth, this
/// remains actionable after the terminal carrier has consumed its local work
/// inventory.
pub fn record_orphan_terminal_cleanup_failure(
    lane: OrphanTerminalCleanupLane,
    reason: OrphanTerminalCleanupFailureReason,
) {
    static C: OnceLock<Counter<u64>> = OnceLock::new();
    C.get_or_init(|| {
        meter()
            .u64_counter("waddle.clustering.orphan_terminal_cleanup_failures")
            .with_description("Terminal orphan claim cleanup attempts that did not complete")
            .with_unit("{failure}")
            .build()
    })
    .add(
        1,
        &[
            opentelemetry::KeyValue::new("lane", lane.as_str()),
            opentelemetry::KeyValue::new("reason", reason.as_str()),
        ],
    );
}

/// Report each bounded SM candidate page, including whether another page was
/// visible at scan time and how many malformed stale rows were quarantined.
pub fn record_sm_orphan_candidate_page(candidates: usize, has_more: bool, quarantined: usize) {
    static H: OnceLock<Histogram<u64>> = OnceLock::new();
    H.get_or_init(|| {
        meter()
            .u64_histogram("waddle.clustering.sm_orphan_candidate_page_size")
            .with_description("Bounded SM orphan candidates returned per cursor page")
            .with_unit("{claim}")
            .build()
    })
    .record(
        candidates as u64,
        &[opentelemetry::KeyValue::new("has_more", has_more)],
    );
    if quarantined > 0 {
        static C: OnceLock<Counter<u64>> = OnceLock::new();
        C.get_or_init(|| {
            meter()
                .u64_counter("waddle.clustering.sm_orphan_malformed_quarantined")
                .with_description("Malformed stale SM claim rows quarantined by the bounded scan")
                .with_unit("{claim}")
                .build()
        })
        .add(quarantined as u64, &[]);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        OrderedRelayNackMetricReason, OrphanTerminalCleanupFailureReason,
        OrphanTerminalCleanupLane, OrphanWorkQueue, OrphanWorker, OrphanWorkerFailureReason,
        RemoteCodecDropReason, RoomOrphanReconciliationOutcome,
    };
    use waddle_xmpp::telemetry::test_support;

    /// Exported (name, unit) pairs for every clustering instrument.
    ///
    /// Pinned because Grafana Cloud's OTLP→Prometheus normalization
    /// appends the unit to the series name: a bare-noun unit such as
    /// `peer` produces `waddle_clustering_connected_peers_peer`, and a
    /// name that also embeds its unit produces a double suffix like
    /// `..._heartbeat_age_ms_milliseconds`. Counts therefore use UCUM
    /// curly-brace annotations (dropped by the translator) and
    /// duration instruments carry the unit only on the instrument,
    /// never in the name.
    const EXPECTED: &[(&str, &str)] = &[
        ("waddle.clustering.connected_peers", "{peer}"),
        ("waddle.clustering.routing_table_size", "{peer}"),
        ("waddle.clustering.bootstrap_dials", "{dial}"),
        ("waddle.clustering.allowlist_size", "{peer}"),
        ("waddle.clustering.remote_codec_drops", "{payload}"),
        ("waddle.clustering.relay_respawns", "{respawn}"),
        ("waddle.clustering.ordered_relay_acks", "{reply}"),
        ("waddle.clustering.ordered_relay_nacks", "{reply}"),
        ("waddle.clustering.node_heartbeat_age", "ms"),
        ("waddle.clustering.node_heartbeat_write_latency", "ms"),
        ("waddle.clustering.peers_revoked", "{peer}"),
        ("waddle.clustering.drain_duration", "ms"),
        ("waddle.clustering.claims_released_on_drain", "{claim}"),
        ("waddle.clustering.claims_abandoned_on_drain", "{claim}"),
        ("waddle.clustering.room_orphan_reconciliations", "{claim}"),
        ("waddle.clustering.room_orphan_pending_depth", "{claim}"),
        ("waddle.clustering.room_orphan_pending_oldest_age", "ms"),
        ("waddle.clustering.orphan_work_queue_depth", "{item}"),
        ("waddle.clustering.orphan_work_queue_backpressure", "{item}"),
        ("waddle.clustering.orphan_worker_failures", "{failure}"),
        (
            "waddle.clustering.orphan_terminal_cleanup_failures",
            "{failure}",
        ),
        ("waddle.clustering.sm_orphan_candidate_page_size", "{claim}"),
        (
            "waddle.clustering.sm_orphan_malformed_quarantined",
            "{claim}",
        ),
    ];

    fn record_every_instrument() {
        super::record_connected_peers(1);
        super::record_routing_table_size(1);
        super::record_bootstrap_dial();
        super::record_allowlist_size(1);
        super::record_remote_codec_drop(RemoteCodecDropReason::Malformed);
        super::record_relay_respawn();
        super::record_ordered_relay_ack();
        super::record_ordered_relay_nack(OrderedRelayNackMetricReason::ParseFailure);
        super::record_node_heartbeat_age_ms(1.0);
        super::record_node_heartbeat_write_latency_ms(1.0);
        super::record_peers_revoked(1);
        super::record_drain_duration_ms(1.0);
        super::record_claims_released_on_drain(1);
        super::record_claims_abandoned_on_drain(1);
        super::record_room_orphan_reconciliation(RoomOrphanReconciliationOutcome::Released, 1);
        super::record_room_orphan_pending_backlog(1, 1);
        super::record_orphan_work_queue_depth(OrphanWorkQueue::RoomRelease, 1);
        super::record_orphan_work_queue_backpressure(OrphanWorkQueue::RoomRelease);
        super::record_orphan_worker_failure(
            OrphanWorker::RoomRelease,
            OrphanWorkerFailureReason::Timeout,
        );
        super::record_orphan_terminal_cleanup_failure(
            OrphanTerminalCleanupLane::RoomRelease,
            OrphanTerminalCleanupFailureReason::Error,
        );
        super::record_sm_orphan_candidate_page(1, false, 1);
    }

    #[tokio::test]
    async fn clustering_instrument_names_and_units_are_pinned() {
        let guard = test_support::acquire().await;
        record_every_instrument();

        for (name, unit) in EXPECTED {
            assert_eq!(
                guard.metric_unit(name).as_deref(),
                Some(*unit),
                "instrument {name} must export UCUM unit {unit}"
            );
        }

        let mut exported: Vec<String> = guard
            .metric_names()
            .into_iter()
            .filter(|name| name.starts_with("waddle.clustering."))
            .collect();
        exported.sort();
        exported.dedup();
        let mut expected: Vec<String> = EXPECTED
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        expected.sort();
        assert_eq!(
            exported, expected,
            "every clustering instrument must be pinned in EXPECTED"
        );
    }

    #[tokio::test]
    async fn clustering_instrument_names_never_embed_their_unit() {
        let guard = test_support::acquire().await;
        record_every_instrument();

        for name in guard
            .metric_names()
            .into_iter()
            .filter(|name| name.starts_with("waddle.clustering."))
        {
            for suffix in ["_ms", "_seconds", "_bytes", "_millis"] {
                assert!(
                    !name.ends_with(suffix),
                    "{name} embeds unit suffix {suffix}; carry the unit on the \
                     instrument instead so OTLP→Prometheus does not double it"
                );
            }
        }
    }

    #[tokio::test]
    async fn claims_abandoned_on_drain_is_zero_registered() {
        let guard = test_support::acquire().await;
        super::register_clustering_counters();

        assert_eq!(
            guard.counter_sum("waddle.clustering.claims_abandoned_on_drain", &[]),
            Some(0),
        );
        assert_eq!(
            guard
                .metric_unit("waddle.clustering.claims_abandoned_on_drain")
                .as_deref(),
            Some("{claim}"),
        );
    }

    #[tokio::test]
    async fn legacy_clustering_label_values_stay_byte_identical() {
        let guard = test_support::acquire().await;

        for reason in [
            RemoteCodecDropReason::TooLarge,
            RemoteCodecDropReason::TooDeep,
            RemoteCodecDropReason::TooManyAttributes,
            RemoteCodecDropReason::Malformed,
            RemoteCodecDropReason::NotAStanza,
            RemoteCodecDropReason::Serialize,
        ] {
            super::record_remote_codec_drop(reason);
        }
        for reason in [
            OrderedRelayNackMetricReason::Gap,
            OrderedRelayNackMetricReason::InFlight,
            OrderedRelayNackMetricReason::NotOwnerOrigin,
            OrderedRelayNackMetricReason::NotOwnerSender,
            OrderedRelayNackMetricReason::NotOwnerTarget,
            OrderedRelayNackMetricReason::Unreachable,
            OrderedRelayNackMetricReason::TargetUnavailable,
            OrderedRelayNackMetricReason::ParseFailure,
            OrderedRelayNackMetricReason::UnsupportedEnvelope,
            OrderedRelayNackMetricReason::Backpressure,
            OrderedRelayNackMetricReason::MaybeCommitted,
            OrderedRelayNackMetricReason::Diverted,
        ] {
            super::record_ordered_relay_nack(reason);
        }
        for queue in [
            OrphanWorkQueue::SmHydration,
            OrphanWorkQueue::RoomRelease,
            OrphanWorkQueue::RoomHandoff,
            OrphanWorkQueue::RoomAdoption,
        ] {
            super::record_orphan_work_queue_depth(queue, 1);
            super::record_orphan_work_queue_backpressure(queue);
        }
        for worker in [
            OrphanWorker::Sweep,
            OrphanWorker::SmHydration,
            OrphanWorker::RoomRelease,
        ] {
            super::record_orphan_worker_failure(worker, OrphanWorkerFailureReason::Timeout);
        }
        for lane in [
            OrphanTerminalCleanupLane::SmHydration,
            OrphanTerminalCleanupLane::RoomRelease,
        ] {
            super::record_orphan_terminal_cleanup_failure(
                lane,
                OrphanTerminalCleanupFailureReason::Error,
            );
        }
        for outcome in [
            RoomOrphanReconciliationOutcome::Hydrated,
            RoomOrphanReconciliationOutcome::Released,
            RoomOrphanReconciliationOutcome::AlreadyLive,
            RoomOrphanReconciliationOutcome::PendingRetry,
            RoomOrphanReconciliationOutcome::LostRace,
            RoomOrphanReconciliationOutcome::Failed,
        ] {
            super::record_room_orphan_reconciliation(outcome, 1);
        }

        let codec_labels: Vec<Vec<(String, String)>> = guard
            .counter_samples("waddle.clustering.remote_codec_drops")
            .expect("codec samples")
            .into_iter()
            .map(|(_, attrs)| attrs)
            .collect();
        for label in [
            "too_large",
            "too_deep",
            "too_many_attributes",
            "malformed",
            "not_a_stanza",
            "serialize",
        ] {
            assert!(codec_labels.contains(&vec![("reason".to_string(), label.to_string())]));
        }

        let nack_labels: Vec<Vec<(String, String)>> = guard
            .counter_samples("waddle.clustering.ordered_relay_nacks")
            .expect("nack samples")
            .into_iter()
            .map(|(_, attrs)| attrs)
            .collect();
        for label in [
            "gap",
            "in_flight",
            "not_owner_origin",
            "not_owner_sender",
            "not_owner_target",
            "unreachable",
            "target_unavailable",
            "parse_failure",
            "unsupported_envelope",
            "backpressure",
            "maybe_committed",
            "diverted",
        ] {
            assert!(nack_labels.contains(&vec![("reason".to_string(), label.to_string())]));
        }

        let queue_labels: Vec<Vec<(String, String)>> = guard
            .counter_samples("waddle.clustering.orphan_work_queue_backpressure")
            .expect("queue samples")
            .into_iter()
            .map(|(_, attrs)| attrs)
            .collect();
        for label in [
            "sm_hydration",
            "room_release",
            "room_handoff",
            "room_adoption",
        ] {
            assert!(queue_labels.contains(&vec![("queue".to_string(), label.to_string())]));
        }

        let worker_labels: Vec<Vec<(String, String)>> = guard
            .counter_samples("waddle.clustering.orphan_worker_failures")
            .expect("worker samples")
            .into_iter()
            .map(|(_, attrs)| attrs)
            .collect();
        for worker in ["sweep", "sm_hydration", "room_release"] {
            assert!(worker_labels.contains(&vec![
                ("reason".to_string(), "timeout".to_string()),
                ("worker".to_string(), worker.to_string()),
            ]));
        }

        let terminal_labels: Vec<Vec<(String, String)>> = guard
            .counter_samples("waddle.clustering.orphan_terminal_cleanup_failures")
            .expect("terminal cleanup samples")
            .into_iter()
            .map(|(_, attrs)| attrs)
            .collect();
        for lane in ["sm_hydration", "room_release"] {
            assert!(terminal_labels.contains(&vec![
                ("lane".to_string(), lane.to_string()),
                ("reason".to_string(), "error".to_string()),
            ]));
        }

        let reconciliation_labels: Vec<Vec<(String, String)>> = guard
            .counter_samples("waddle.clustering.room_orphan_reconciliations")
            .expect("reconciliation samples")
            .into_iter()
            .map(|(_, attrs)| attrs)
            .collect();
        for outcome in [
            "hydrated",
            "released",
            "already_live",
            "pending_retry",
            "lost_race",
            "failed",
        ] {
            assert!(
                reconciliation_labels.contains(&vec![("outcome".to_string(), outcome.to_string())])
            );
        }
    }
}
