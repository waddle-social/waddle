//! The owned libp2p swarm: build, listen, dial, and drive the event loop.
//!
//! Node discovery only this phase. We build a swarm we own (tcp + quic,
//! Noise, yamux), install the process-global kameo `ActorSwarm` so remote
//! actor lookups resolve (used by the relay actors in a later slice), listen
//! on the configured multiaddrs, dial headless-DNS-resolved seed peers, and
//! run the event loop until the shared shutdown token fires. No stanza is
//! routed cross-node.

use super::allowlist::{diff_allowlist, AllowlistError, AllowlistStore, PostgresAllowlistStore};
use super::behaviour::{WaddleBehaviour, WaddleBehaviourEvent};
use super::lease::{
    KeypairSlotLease, LeaseError, LeaseIdentity, LeasedSlot, PostgresKeypairSlotLease,
};
use super::self_fence::ConnectedPeerCount;
use super::{dns, identity, metrics, relay, NodeId};
use crate::config::{ClusteringBootstrapConfig, ClusteringConfig};
use crate::db::Database;
use core::convert::Infallible;
use futures::StreamExt;
use kameo::remote::{self, registry};
use libp2p::identity::Keypair;
use libp2p::swarm::SwarmEvent;
use libp2p::{noise, tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder};
use relay::ConnectedTransportCounts;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

/// Buffer for resolved-peer multiaddrs flowing from the off-loop DNS resolver
/// back to the event loop for dialing.
const DIAL_CHANNEL_CAPACITY: usize = 64;

/// Buffer for enrolled-peer sets flowing from the off-loop allowlist reader
/// back to the event loop. Single-flight refreshes mean at most one set is
/// ever in flight; a small buffer keeps the sender from awaiting.
const ALLOWLIST_CHANNEL_CAPACITY: usize = 4;

/// Idle timeout for swarm connections: a cluster mesh wants long-lived peer
/// connections, and libp2p's ~10s default would close them between discovery
/// traffic and force a re-dial per interaction.
const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(300);

/// Poll cadence for the fault-gated cross-dial barrier. This is deliberately
/// much shorter than the harness's one-second dial interval so releasing the
/// barrier does not dominate the regression's wall clock.
const FAULT_DIAL_BARRIER_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Failures while building or starting the swarm. Human-facing `Display` text
/// is surfaced at server startup.
#[derive(Debug, thiserror::Error)]
pub enum SwarmError {
    /// A transport (TCP/Noise/yamux) failed to initialize.
    #[error("clustering swarm transport init failed: {0}")]
    Transport(String),
    /// The process-global kameo `ActorSwarm` was already initialized — only
    /// one swarm may exist per process (kameo `init_global` is a singleton).
    #[error(
        "clustering swarm already bootstrapped in this process (kameo init_global is a singleton)"
    )]
    AlreadyBootstrapped,
    /// A configured listen multiaddr could not be parsed.
    #[error("clustering listen address '{addr}' is not a valid multiaddr: {reason}")]
    ListenAddrInvalid { addr: String, reason: String },
    /// `listen_on` was rejected by the transport for a parsed multiaddr.
    #[error("clustering swarm failed to listen on '{addr}': {reason}")]
    Listen { addr: String, reason: String },
    /// The keypair-slot lease could not be acquired or its schema set up.
    #[error(transparent)]
    Lease(#[from] LeaseError),
    /// A leased pool slot's keypair could not be decoded.
    #[error(transparent)]
    Identity(#[from] identity::IdentityError),
    /// The peer allowlist could not be read at startup.
    #[error(transparent)]
    Allowlist(#[from] AllowlistError),
    /// Configured keypair-pool PeerIds are not fully enrolled.
    #[error(
        "clustering keypair pool is not fully enrolled in clustering_peer_allowlist: \
         missing_peer_ids=[{missing_peer_ids}] configured_keypairs={configured_keypairs} \
         enrolled_peer_rows={enrolled_peer_rows}"
    )]
    KeypairPoolNotEnrolled {
        configured_keypairs: usize,
        enrolled_peer_rows: usize,
        missing_peer_ids: String,
    },
    /// Clustering was enabled without a stable, pre-enrolled identity pool.
    #[error(
        "clustering requires WADDLE_CLUSTERING_KEYPAIR_POOL with at least one \
         pre-enrolled keypair; ephemeral identities cannot join the peer allowlist"
    )]
    KeypairPoolRequired,
    /// The configured node-id file could not be written.
    #[error("failed to write clustering node-id file '{path}': {reason}")]
    NodeIdFile { path: String, reason: String },
}

/// Identity of a running swarm: the libp2p `PeerId` plus the per-process
/// `node_id` that names this node's keypair-slot lease and relay registration.
#[derive(Debug, Clone)]
pub struct SwarmHandle {
    pub local_peer_id: PeerId,
    pub node_id: NodeId,
    /// Readable snapshot of the swarm's current connected-peer count (Phase
    /// 2 Slice 1's gauge, reused here per ADR-0017 Phase 3 Slice 2's
    /// isolation rule — see `self_fence::ConnectedPeerCount`'s doc comment
    /// for why a coarse peer count, not per-node reachability, is what the
    /// isolation check reads).
    pub connected_peers: ConnectedPeerCount,
}

/// Build the swarm, install the global `ActorSwarm`, start listening, spawn
/// the event loop and the supervised relay on `stop_token`, and return the
/// node's swarm identity.
///
/// `stop_token` is expected to be the clustering-scoped child token minted by
/// `clustering::clustering_scope_token` (a child of the process-wide shutdown
/// token): every task spawned here derives its own token from this same
/// value, so a clustering-internal self-fence (lease fencing loss or a blown
/// renewal deadline) cancels only this scope, while process shutdown still
/// propagates in and tears clustering down.
///
/// The node leases one keypair-pool slot from the Postgres control plane and
/// uses that stable PeerId (and heartbeats it). Clustering bring-up fails
/// without a configured pool because ephemeral identities cannot be
/// pre-enrolled in the symmetric peer allowlist.
pub async fn spawn(
    config: &ClusteringConfig,
    db: &Database,
    stop_token: CancellationToken,
    relay_bridges: RelayBridges,
) -> Result<SwarmHandle, SwarmError> {
    // Parse and validate the listen multiaddrs first, before building any
    // transport or touching the process-global `ActorSwarm` — fail fast on bad
    // config.
    let listen_addrs = config
        .listen_addrs
        .iter()
        .map(|addr| {
            addr.parse::<Multiaddr>()
                .map_err(
                    |error: libp2p::multiaddr::Error| SwarmError::ListenAddrInvalid {
                        addr: addr.clone(),
                        reason: error.to_string(),
                    },
                )
        })
        .collect::<Result<Vec<Multiaddr>, SwarmError>>()?;

    // One per-process node id: freshly generated every start, never reused
    // across restarts. Names this node's keypair-slot lease and its single
    // kademlia relay registration.
    let node_id = NodeId::generate();

    let (keypair, pending_lease) = node_keypair(config, db, &node_id).await?;

    // Everything fallible after the slot lease runs in `bring_up`; only once
    // it all succeeds does the slot's heartbeat start. A failure releases the
    // slot immediately instead of leaving it held until lease-TTL expiry —
    // repeated crash-restarts (each with a fresh node_id) would otherwise
    // chew through the pool one slot per attempt and starve replacements
    // with NoFreeSlot.
    match bring_up(
        config,
        db,
        node_id,
        keypair,
        listen_addrs,
        &stop_token,
        relay_bridges,
    )
    .await
    {
        Ok(handle) => {
            if let Some(pending) = pending_lease {
                tokio::spawn(run_heartbeat(
                    pending.lease,
                    pending.node,
                    pending.slot,
                    pending.acquired_at,
                    config.lease.heartbeat_interval,
                    config.lease.lease_ttl,
                    stop_token.clone(),
                ));
            }
            Ok(handle)
        }
        Err(error) => {
            if let Some(pending) = pending_lease {
                pending.release_best_effort("swarm bring-up failed").await;
            }
            Err(error)
        }
    }
}

/// Per-node relay bridges bundled into one bring-up parameter (clippy
/// `too_many_arguments`): the cross-node XEP-0198 resume live-steal
/// handshake bridge (ADR-0017 Phase 3 Slice 6) and the MUC Demote ask's
/// local-claims bridge (Slice 7). Both are constructed empty at
/// `start_if_enabled` time — their respective registries don't exist yet
/// at that point in startup — and wired later once those registries are
/// built; see each field's own doc comment upstream
/// (`ClusteringHandles::resume_bridge`/`room_local_claims`) for the full
/// construction-order rationale.
pub struct RelayBridges {
    pub resume_bridge: Arc<super::resume_bridge::ResumeStealBridge>,
    pub room_local_claims: Arc<super::local_claims::RoomLocalClaims>,
    pub ordered_relay_delivery_bridge: Arc<super::route_bridge::OrderedRelayDeliveryBridge>,
}

/// The fallible remainder of swarm bring-up after the keypair-slot lease:
/// allowlist load, transport/behaviour build, global `ActorSwarm` install,
/// listeners, event loop, supervised relay, and the node-id file. Split out
/// of [`spawn`] so a failure anywhere in it releases the leased slot.
async fn bring_up(
    config: &ClusteringConfig,
    db: &Database,
    node_id: NodeId,
    keypair: Keypair,
    listen_addrs: Vec<Multiaddr>,
    stop_token: &CancellationToken,
    relay_bridges: RelayBridges,
) -> Result<SwarmHandle, SwarmError> {
    let RelayBridges {
        resume_bridge,
        room_local_claims,
        ordered_relay_delivery_bridge,
    } = relay_bridges;
    let local_peer_id = keypair.public().to_peer_id();
    ordered_relay_delivery_bridge.wire_origin_signer(keypair.clone());

    // Peer authorization (ADR element 3): load the enrolled peer set before
    // the swarm accepts anything. An empty allowlist is deny-all — correct for
    // a first node with no peers, but worth a loud note.
    let allowlist: Arc<dyn AllowlistStore> = Arc::new(PostgresAllowlistStore::new(db.clone()));
    let enrolled = allowlist.enrolled_peers().await?;
    verify_keypair_pool_enrollment(config, &enrolled)?;
    metrics::record_allowlist_size(enrolled.len() as i64);
    if enrolled.is_empty() {
        tracing::warn!(
            "clustering peer allowlist is empty: all peer connections will be rejected until \
             peers are enrolled (deny-all)"
        );
    } else if !enrolled.contains(&local_peer_id) {
        // Enrollment is symmetric and cluster-wide: a node absent from the
        // shared table is rejected by every peer, which otherwise presents
        // only as silent dial failures on the other nodes. Not fatal —
        // enrollment can land moments after boot and the refresh picks it up.
        tracing::warn!(
            %local_peer_id,
            "clustering: this node's PeerId is not in the peer allowlist — every enrolled \
             peer will reject its connections until it is enrolled"
        );
    }

    let messaging_config = remote::messaging::Config::default()
        .with_request_timeout(config.messaging.request_timeout)
        .with_max_concurrent_streams(config.messaging.max_concurrent_streams)
        .with_request_size_maximum(config.messaging.max_request_bytes)
        .with_response_size_maximum(config.messaging.max_response_bytes);

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|error| SwarmError::Transport(error.to_string()))?
        .with_quic()
        // Infallible construction: return the behaviour directly (libp2p's
        // `TryIntoBehaviour<B> for B` form, whose error is `Infallible`). The
        // fallible form requires exactly `Box<dyn Error + Send + Sync>`, so an
        // `Ok::<_, Infallible>` wrapper would be misread as the behaviour type.
        .with_behaviour(|key| {
            WaddleBehaviour::new(key.public().to_peer_id(), messaging_config, &enrolled)
        })
        .map_err(|never: Infallible| -> SwarmError { match never {} })?
        // A cluster mesh wants long-lived peer connections; libp2p's default
        // ~10s idle timeout would close them between discovery traffic and
        // force a re-dial per interaction.
        .with_swarm_config(|config| config.with_idle_connection_timeout(IDLE_CONNECTION_TIMEOUT))
        .build();

    // Install the process-global kameo `ActorSwarm` so `RemoteActorRef`
    // lookups resolve against this swarm (used by the relay actors in a later
    // slice). One swarm per process by construction.
    swarm
        .behaviour()
        .kameo
        .try_init_global()
        .map_err(|_| SwarmError::AlreadyBootstrapped)?;

    for multiaddr in listen_addrs {
        let display = multiaddr.to_string();
        swarm
            .listen_on(multiaddr)
            .map_err(|error| SwarmError::Listen {
                addr: display,
                reason: error.to_string(),
            })?;
    }

    let connected_peers = ConnectedPeerCount::new();
    let connected_transports = ConnectedTransportCounts::new();
    let fault_dial_barrier = config
        .fault_dial_barrier_dir
        .clone()
        .map(|root| FaultDialBarrier::new(root, local_peer_id));
    let bootstrap_peers = config.bootstrap_peers.clone();
    // The node's single kademlia registration: its supervised relay actor.
    // Its first registration may occur before the first peer connection, so
    // the event loop below also triggers a same-name re-registration whenever
    // a peer connects; the 15s periodic refresh remains the fallback.
    if config.fault_injection {
        tracing::warn!(
            "clustering fault injection is ENABLED: any enrolled peer can crash this node's \
             relay, stall its mailbox, or make it probe another peer — test harnesses only, \
             never production"
        );
    }
    let relay_registration = relay::spawn_supervised(relay::RelaySupervisorInputs {
        node_id: node_id.clone(),
        connected_peers: connected_peers.clone(),
        connected_transports: connected_transports.clone(),
        fault_injection: config.fault_injection,
        stop_token: stop_token.clone(),
        resume_bridge,
        room_local_claims,
        ordered_delivery_bridge: ordered_relay_delivery_bridge,
    });

    tokio::spawn(run_event_loop(EventLoopInputs {
        swarm,
        bootstrap_peers,
        dial_interval: config.dial_interval,
        allowlist: AllowlistRefresh {
            store: allowlist,
            current: enrolled,
            interval: config.allowlist_refresh_interval,
        },
        relay_registration,
        connected_peers: connected_peers.clone(),
        connected_transports,
        fault_dial_barrier,
        stop_token: stop_token.clone(),
    }));

    let handle = SwarmHandle {
        local_peer_id,
        node_id,
        connected_peers,
    };

    // Publish the node identity for the multi-process harness (mirrors the
    // WADDLE_HTTP_PORT_FILE convention).
    if let Some(path) = &config.node_id_file {
        let contents = format!("{} {}\n", handle.node_id, handle.local_peer_id);
        tokio::fs::write(path, contents)
            .await
            .map_err(|error| SwarmError::NodeIdFile {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
    }

    Ok(handle)
}

/// State for the periodic allowlist refresh driven by the event loop.
struct AllowlistRefresh {
    store: Arc<dyn AllowlistStore>,
    /// The enrolled set as currently applied to the swarm's allowed-peers
    /// behaviour.
    current: HashSet<PeerId>,
    interval: Duration,
}

/// Deterministic, fault-gated first-dial barrier for the multi-process
/// simultaneous cross-dial regression.
///
/// The harness creates `release`, then each node queues exactly one bootstrap
/// dial, writes `dialed-<local-peer-id>`, and temporarily stops polling its
/// swarm. Once both markers exist the harness creates `drive`; both swarms
/// resume with their outbound dial futures already queued. That makes the
/// two-transport condition an established precondition instead of a scheduler
/// coincidence.
struct FaultDialBarrier {
    root: PathBuf,
    marker: PathBuf,
    released: bool,
    dial_submitted: bool,
    driving: bool,
}

impl FaultDialBarrier {
    fn new(root: PathBuf, local_peer_id: PeerId) -> Self {
        let marker = root.join(format!("dialed-{local_peer_id}"));
        Self {
            root,
            marker,
            released: false,
            dial_submitted: false,
            driving: false,
        }
    }

    fn refresh(&mut self) {
        self.released |= self.root.join("release").is_file();
        self.driving |= self.root.join("drive").is_file();
    }

    fn allows_dial(&self) -> bool {
        self.released && (!self.dial_submitted || self.driving)
    }

    fn allows_swarm_poll(&self) -> bool {
        !self.dial_submitted || self.driving
    }

    async fn mark_dial_submitted(&mut self) {
        if self.dial_submitted {
            return;
        }
        self.dial_submitted = true;
        if let Err(error) = tokio::fs::write(&self.marker, b"queued\n").await {
            tracing::error!(
                path = %self.marker.display(),
                %error,
                "failed to publish clustering fault dial-barrier marker"
            );
        }
    }
}

/// A leased keypair slot whose heartbeat has not started yet: held across
/// the rest of swarm bring-up so a failure there releases the slot
/// immediately instead of leaving it blocked until lease-TTL expiry.
struct PendingLease {
    lease: PostgresKeypairSlotLease,
    node: LeaseIdentity,
    slot: LeasedSlot,
    /// When the slot was acquired: the heartbeat's initial "last successful
    /// renewal" instant, so the lease-deadline self-fence measures
    /// from the real acquire time rather than from whenever `bring_up`
    /// happens to finish and the heartbeat task is spawned.
    acquired_at: tokio::time::Instant,
}

impl PendingLease {
    /// Best-effort release: without it a failed bring-up holds the slot
    /// hostage until TTL expiry while this node crash-loops onto fresh
    /// slots, transiently shrinking the pool for every other node.
    async fn release_best_effort(self, context: &'static str) {
        if let Err(error) = self.lease.release(&self.node, self.slot).await {
            tracing::warn!(
                %error,
                slot = self.slot.slot_index,
                context,
                "failed to release keypair slot"
            );
        }
    }
}

fn verify_keypair_pool_enrollment(
    config: &ClusteringConfig,
    enrolled: &HashSet<PeerId>,
) -> Result<(), SwarmError> {
    if config.keypair_pool.is_empty() {
        return Err(SwarmError::KeypairPoolRequired);
    }

    let mut expected = Vec::with_capacity(config.keypair_pool.len());
    for entry in &config.keypair_pool {
        expected.push(
            identity::keypair_from_pool_entry(entry)?
                .public()
                .to_peer_id(),
        );
    }
    expected.sort_unstable();
    expected.dedup();

    let mut missing: Vec<_> = expected
        .iter()
        .copied()
        .filter(|peer_id| !enrolled.contains(peer_id))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    missing.sort_unstable();
    let missing_peer_ids = missing
        .into_iter()
        .map(|peer_id| peer_id.to_string())
        .collect::<Vec<_>>()
        .join(",");

    Err(SwarmError::KeypairPoolNotEnrolled {
        configured_keypairs: config.keypair_pool.len(),
        enrolled_peer_rows: enrolled.len(),
        missing_peer_ids,
    })
}

/// Resolve this node's libp2p keypair by leasing a configured keypair-pool slot
/// (returned as a [`PendingLease`] — the caller starts its heartbeat only after
/// the rest of bring-up succeeds, or releases it).
async fn node_keypair(
    config: &ClusteringConfig,
    db: &Database,
    node_id: &NodeId,
) -> Result<(Keypair, Option<PendingLease>), SwarmError> {
    if config.keypair_pool.is_empty() {
        return Err(SwarmError::KeypairPoolRequired);
    }

    let lease = PostgresKeypairSlotLease::new(db.clone());
    lease.ensure_schema().await?;
    let node = LeaseIdentity {
        node_id: node_id.clone(),
        node_epoch: uuid::Uuid::new_v4(),
    };
    let pool_size = config.keypair_pool.len();
    let slot = lease
        .acquire(&node, pool_size, config.lease.lease_ttl)
        .await?;
    // The acquire itself counts as the first successful renewal for the
    // lease-deadline self-fence.
    let acquired_at = tokio::time::Instant::now();
    // `acquire` only ever returns an in-range slot, so this index is valid.
    let entry = &config.keypair_pool[slot.slot_index as usize];
    let pending = PendingLease {
        lease,
        node,
        slot,
        acquired_at,
    };
    let keypair = match identity::keypair_from_pool_entry(entry) {
        Ok(keypair) => keypair,
        Err(error) => {
            pending
                .release_best_effort("keypair pool entry decode failed")
                .await;
            return Err(error.into());
        }
    };
    tracing::info!(
        slot = pending.slot.slot_index,
        pool_size,
        "clustering: leased keypair-pool slot for node identity"
    );

    Ok((keypair, Some(pending)))
}

/// Renew the keypair-slot lease on a timer until shutdown; release the slot on
/// graceful stop, and self-fence — cancel `stop_token` (the clustering scope,
/// never the process-wide token; see `clustering::clustering_scope_token`) —
/// on either a definitive fencing loss (CAS miss: another node already holds
/// the slot) or once `lease_ttl` has elapsed since the last successful renewal
/// without one landing (a node partitioned from Postgres but still alive on
/// libp2p, which would otherwise keep using a PeerId that another node may
/// legitimately have re-derived after stealing the now-expired slot).
///
/// Renewals are single-flight and a hung renewal is polled to completion
/// rather than timeout-dropped: abandoning an in-flight sqlx future mid-poll
/// makes the pool spawn an unbounded background `ping()` against the same
/// dead socket to vet the connection, so dropping one renewal per tick during
/// a sustained partition would wedge the entire shared pool (the server's hot
/// path) within `max_connections` ticks. Instead the deadline arm below is
/// the sole fencing trigger for a hang, and the one in-flight renewal is
/// dropped at most once per process — on fence or shutdown. Worst case on the
/// shutdown-mid-hang path is two abandoned connections (the dropped renewal
/// plus a timed-out release), still a one-time bounded cost, never per-tick.
/// The deadline is authoritative by construction (`biased` orders it ahead of
/// renewal completion): a renewal landing in the same poll as the deadline
/// loses the tie and the node fences conservatively.
async fn run_heartbeat<L>(
    lease: L,
    node: LeaseIdentity,
    slot: LeasedSlot,
    acquired_at: tokio::time::Instant,
    interval: Duration,
    lease_ttl: Duration,
    stop_token: CancellationToken,
) where
    L: KeypairSlotLease + Send + Sync + 'static,
{
    // The slot was acquired (heartbeat stamped) moments ago, so the first
    // renewal is deferred one full interval (`interval_at`) — same pattern
    // as the allowlist refresh timer.
    let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Last instant a renewal (or the initial acquire) definitely succeeded.
    // Only ever moves forward on `Ok`; a failed or hung renewal does not
    // reset it, so retries do not silently extend the deadline.
    let mut last_success = acquired_at;

    'fenced: loop {
        // Phase 1: wait for the next renewal to be due. The deadline arm
        // fires here when every attempt has been failing fast (quick DB
        // errors between ticks).
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => {
                release_slot_bounded(&lease, &node, slot, interval).await;
                return;
            }
            _ = tokio::time::sleep_until(last_success + lease_ttl) => {
                break 'fenced;
            }
            _ = timer.tick() => {}
        }

        // Phase 2: drive exactly one renewal to completion, racing only the
        // deadline and shutdown. A hung renewal parks here (single-flight —
        // no new attempt piles up behind it) until it resolves or the
        // deadline arm fences.
        let renewal = std::pin::pin!(lease.heartbeat(&node, slot, lease_ttl));
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => {
                // Dropping the hung renewal here is the shutdown-path
                // exception to the never-drop rule: one abandoned
                // connection, once.
                release_slot_bounded(&lease, &node, slot, interval).await;
                return;
            }
            _ = tokio::time::sleep_until(last_success + lease_ttl) => {
                break 'fenced;
            }
            result = renewal => {
                match result {
                    Ok(()) => {
                        last_success = tokio::time::Instant::now();
                    }
                    Err(LeaseError::FencingLoss { slot_index }) => {
                        tracing::error!(
                            slot = slot_index,
                            "clustering keypair-slot lease lost (fencing) — self-fencing clustering scope"
                        );
                        // The identity is no longer ours; stop clustering,
                        // not the whole process.
                        stop_token.cancel();
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(%error, slot = slot.slot_index, "clustering keypair-slot heartbeat error; will retry next tick");
                    }
                }
            }
        }
    }

    tracing::error!(
        slot = slot.slot_index,
        lease_ttl_ms = u64::try_from(lease_ttl.as_millis()).unwrap_or(u64::MAX),
        "clustering keypair-slot lease deadline exceeded without successful renewal — self-fencing clustering scope"
    );
    stop_token.cancel();
}

/// Best-effort, time-bounded slot release on shutdown: a hung release must
/// not stall clustering teardown indefinitely (the slot frees itself via TTL
/// expiry anyway). Dropping a hung release abandons at most one connection,
/// once, at shutdown.
async fn release_slot_bounded<L>(
    lease: &L,
    node: &LeaseIdentity,
    slot: LeasedSlot,
    budget: Duration,
) where
    L: KeypairSlotLease + Send + Sync,
{
    match tokio::time::timeout(budget, lease.release(node, slot)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, slot = slot.slot_index, "clustering keypair-slot release failed on shutdown");
        }
        Err(_) => {
            tracing::warn!(
                slot = slot.slot_index,
                "clustering keypair-slot release timed out on shutdown; slot frees via TTL expiry"
            );
        }
    }
}

/// Complete state handed to the long-lived swarm event loop.
struct EventLoopInputs {
    swarm: Swarm<WaddleBehaviour>,
    bootstrap_peers: Vec<ClusteringBootstrapConfig>,
    dial_interval: Duration,
    allowlist: AllowlistRefresh,
    relay_registration: relay::RelayRegistrationTrigger,
    connected_peers: ConnectedPeerCount,
    connected_transports: ConnectedTransportCounts,
    fault_dial_barrier: Option<FaultDialBarrier>,
    stop_token: CancellationToken,
}

/// Drive the swarm until `stop_token` (the clustering scope, a child of the
/// process shutdown token) fires: dial seed peers on a timer, refresh the
/// peer allowlist on a timer (revoking removed peers), feed swarm events to
/// the handler, and stop cleanly on shutdown — whether that shutdown is the
/// whole process draining or just this node self-fencing its lease.
async fn run_event_loop(inputs: EventLoopInputs) {
    let EventLoopInputs {
        mut swarm,
        bootstrap_peers,
        dial_interval,
        mut allowlist,
        relay_registration,
        connected_peers,
        connected_transports,
        mut fault_dial_barrier,
        stop_token,
    } = inputs;
    // Peers we currently hold a connection to (authoritative from connection
    // events), peers observed to enter the kademlia routing table, and the
    // remote address of every live connection (so the bootstrap dial loop can
    // skip endpoints it is already connected to instead of churning a fresh
    // connection every interval).
    let mut connected: HashSet<PeerId> = HashSet::new();
    let mut routing_peers: HashSet<PeerId> = HashSet::new();
    let mut conn_addrs: HashMap<libp2p::swarm::ConnectionId, Multiaddr> = HashMap::new();
    // Which peer answered at each dialed address, learned from outbound
    // establishments and pruned when the owner's last connection closes (so
    // the map stays bounded by connected peers under pod churn, and a fully
    // disconnected peer stays redialable). Lets the dial loop skip a seed
    // whose owner is connected *inbound-only* — `conn_addrs` alone cannot,
    // because an inbound connection's remote address is an ephemeral source
    // port that never matches the seed addr, so every interval would churn an
    // unnecessary additional connection.
    let mut addr_owners: HashMap<Multiaddr, PeerId> = HashMap::new();

    // `interval` fires its first tick immediately, so the first dial round
    // happens right away rather than after a full interval.
    let mut dial_timer = tokio::time::interval(dial_interval);
    dial_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut fault_dial_barrier_timer = tokio::time::interval(FAULT_DIAL_BARRIER_POLL_INTERVAL);
    fault_dial_barrier_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // The startup path already loaded the enrolled set synchronously before
    // this loop started, so the first periodic refresh is deferred one full
    // interval (`interval_at`) instead of immediately re-reading the table.
    let mut allowlist_timer = tokio::time::interval_at(
        tokio::time::Instant::now() + allowlist.interval,
        allowlist.interval,
    );
    allowlist_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // DNS resolution runs in a spawned task and hands resolved multiaddrs back
    // over this channel, so a slow lookup never stalls swarm polling (which
    // must keep running to service handshakes, keepalives, and kademlia). We
    // hold `dial_tx` for the loop's lifetime, so `recv()` never closes.
    let (dial_tx, mut dial_rx) = tokio::sync::mpsc::channel::<Multiaddr>(DIAL_CHANNEL_CAPACITY);

    // Single-flight guard for the DNS resolver. `tokio::net::lookup_host` runs
    // an uncancellable `getaddrinfo` on the blocking pool; without this, a DNS
    // outage would spawn a new stuck task every interval and eventually
    // saturate the blocking pool, starving unrelated `spawn_blocking` work
    // process-wide. At most one resolver runs at a time; ticks are skipped
    // while one is in flight and resume once it returns.
    let dns_in_flight = Arc::new(AtomicBool::new(false));

    // Allowlist refreshes follow the same off-loop + single-flight shape: the
    // DB read runs in a spawned task and the fresh enrolled set comes back
    // over a channel, so a slow query never stalls swarm polling.
    let (allowlist_tx, mut allowlist_rx) =
        tokio::sync::mpsc::channel::<HashSet<PeerId>>(ALLOWLIST_CHANNEL_CAPACITY);
    let allowlist_in_flight = Arc::new(AtomicBool::new(false));

    // Resets its single-flight flag on drop, so a panic in a guarded task can
    // never permanently wedge that background operation (for the allowlist
    // that would mean this node stops applying peer revocations).
    struct InFlightGuard(Arc<AtomicBool>);

    impl InFlightGuard {
        /// Claim the flag; `None` while a prior task is still in flight.
        fn try_claim(flag: &Arc<AtomicBool>) -> Option<Self> {
            (!flag.swap(true, Ordering::AcqRel)).then(|| Self(Arc::clone(flag)))
        }
    }

    impl Drop for InFlightGuard {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }

    loop {
        let dial_allowed = fault_dial_barrier
            .as_ref()
            .is_none_or(FaultDialBarrier::allows_dial);
        let swarm_poll_allowed = fault_dial_barrier
            .as_ref()
            .is_none_or(FaultDialBarrier::allows_swarm_poll);
        tokio::select! {
            _ = stop_token.cancelled() => {
                tracing::info!("clustering swarm event loop stopping (shutdown)");
                break;
            }
            _ = fault_dial_barrier_timer.tick(), if fault_dial_barrier.is_some() => {
                if let Some(barrier) = fault_dial_barrier.as_mut() {
                    barrier.refresh();
                }
            }
            _ = dial_timer.tick(), if dial_allowed => {
                if !bootstrap_peers.is_empty() {
                    if let Some(guard) = InFlightGuard::try_claim(&dns_in_flight) {
                        let seeds = bootstrap_peers.clone();
                        let dial_tx = dial_tx.clone();
                        tokio::spawn(async move {
                            let _guard = guard;
                            for addr in dns::resolve_bootstrap_peers(&seeds).await {
                                if dial_tx.send(addr).await.is_err() {
                                    break;
                                }
                            }
                        });
                    }
                }
            }
            _ = allowlist_timer.tick() => {
                if let Some(guard) = InFlightGuard::try_claim(&allowlist_in_flight) {
                    let store = Arc::clone(&allowlist.store);
                    let allowlist_tx = allowlist_tx.clone();
                    tokio::spawn(async move {
                        let _guard = guard;
                        match store.enrolled_peers().await {
                            Ok(enrolled) => {
                                let _ = allowlist_tx.send(enrolled).await;
                            }
                            Err(error) => {
                                // Keep the last-known set on a read failure —
                                // never fail open, never mass-revoke on a
                                // transient DB error.
                                tracing::warn!(%error, "clustering allowlist refresh failed; keeping current set");
                            }
                        }
                    });
                }
            }
            Some(enrolled) = allowlist_rx.recv() => {
                apply_allowlist(&mut swarm, &mut allowlist, enrolled);
            }
            Some(addr) = dial_rx.recv(), if dial_allowed => {
                // Skip endpoints we already hold a connection to: redialing a
                // connected peer every interval would just mint an
                // unnecessary additional connection. Two checks: an outbound
                // connection's remote address matches the seed addr directly;
                // an inbound-only peer is recognized via the addr's last
                // known owner.
                if conn_addrs.values().any(|connected_addr| *connected_addr == addr) {
                    continue;
                }
                if addr_owners
                    .get(&addr)
                    .is_some_and(|owner| connected.contains(owner))
                {
                    continue;
                }
                metrics::record_bootstrap_dial();
                match swarm.dial(addr.clone()) {
                    Ok(()) => {
                        if let Some(barrier) = fault_dial_barrier.as_mut() {
                            barrier.mark_dial_submitted().await;
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%addr, %error, "clustering bootstrap dial failed");
                    }
                }
            }
            event = swarm.select_next_some(), if swarm_poll_allowed => {
                handle_swarm_event(
                    &mut swarm,
                    event,
                    SwarmEventBookkeeping {
                        connected: &mut connected,
                        routing_peers: &mut routing_peers,
                        conn_addrs: &mut conn_addrs,
                        addr_owners: &mut addr_owners,
                        relay_registration: &relay_registration,
                        connected_peers: &connected_peers,
                        connected_transports: &connected_transports,
                    },
                );
            }
        }
    }
}

/// Apply a freshly read enrolled set to the swarm's allowed-peers behaviour:
/// newly enrolled peers become dialable/acceptable, and removed peers are
/// revoked — `disallow_peer` also closes their live connections, so a revoked
/// peer's swarm access ends within one refresh interval (ADR element 3).
fn apply_allowlist(
    swarm: &mut Swarm<WaddleBehaviour>,
    allowlist: &mut AllowlistRefresh,
    enrolled: HashSet<PeerId>,
) {
    let diff = diff_allowlist(&allowlist.current, &enrolled);
    if diff.added.is_empty() && diff.removed.is_empty() {
        return;
    }
    for peer in &diff.added {
        swarm.behaviour_mut().allowed.allow_peer(*peer);
    }
    for peer in &diff.removed {
        swarm.behaviour_mut().allowed.disallow_peer(*peer);
        tracing::warn!(peer_id = %peer, "clustering peer revoked: closing live connections");
    }
    if !diff.removed.is_empty() {
        metrics::record_peers_revoked(diff.removed.len() as u64);
    }
    if enrolled.is_empty() {
        // Reachable only via a genuinely emptied table (an all-rows-invalid
        // read fails the refresh instead) — i.e. a deliberate full-cluster
        // revocation by the enrollment authority. Say so loudly.
        tracing::warn!(
            "clustering peer allowlist is now empty: deny-all — every peer connection revoked"
        );
    }
    metrics::record_allowlist_size(enrolled.len() as i64);
    tracing::info!(
        added = diff.added.len(),
        revoked = diff.removed.len(),
        enrolled = enrolled.len(),
        "clustering peer allowlist refreshed"
    );
    allowlist.current = enrolled;
}

/// Record a newly connected peer, updating the connected-peer gauge (and the
/// readable [`ConnectedPeerCount`] ADR-0017 Phase 3 Slice 2's isolation
/// check reads) on the first connection to that peer. Returns whether this
/// was the peer's first live connection.
fn track_peer_connected(
    peer_id: PeerId,
    connected: &mut HashSet<PeerId>,
    connected_peers: &ConnectedPeerCount,
) -> bool {
    if connected.insert(peer_id) {
        metrics::record_connected_peers(connected.len() as i64);
        connected_peers.set(connected.len() as i64);
        tracing::debug!(%peer_id, "clustering peer connected");
        true
    } else {
        false
    }
}

/// Apply libp2p's authoritative live-connection count after an establishment
/// before updating peer-level first-connection bookkeeping.
fn track_peer_connection_established(
    peer_id: PeerId,
    num_established: u32,
    connected: &mut HashSet<PeerId>,
    connected_peers: &ConnectedPeerCount,
    connected_transports: &ConnectedTransportCounts,
) -> bool {
    connected_transports.set(peer_id, num_established);
    track_peer_connected(peer_id, connected, connected_peers)
}

/// Record a peer whose last connection closed, updating the connected-peer
/// gauge (and [`ConnectedPeerCount`]) if it was tracked.
fn track_peer_disconnected(
    peer_id: PeerId,
    connected: &mut HashSet<PeerId>,
    connected_peers: &ConnectedPeerCount,
) {
    if connected.remove(&peer_id) {
        metrics::record_connected_peers(connected.len() as i64);
        connected_peers.set(connected.len() as i64);
        tracing::debug!(%peer_id, "clustering peer disconnected");
    }
}

/// Apply libp2p's connection-close count to peer-level bookkeeping. Returns
/// whether the event closed the peer's last live transport connection.
fn track_peer_connection_closed(
    peer_id: PeerId,
    num_established: u32,
    connected: &mut HashSet<PeerId>,
    connected_peers: &ConnectedPeerCount,
    connected_transports: &ConnectedTransportCounts,
) -> bool {
    connected_transports.set(peer_id, num_established);
    if num_established == 0 {
        track_peer_disconnected(peer_id, connected, connected_peers);
        true
    } else {
        false
    }
}

struct SwarmEventBookkeeping<'a> {
    connected: &'a mut HashSet<PeerId>,
    routing_peers: &'a mut HashSet<PeerId>,
    conn_addrs: &'a mut HashMap<libp2p::swarm::ConnectionId, Multiaddr>,
    addr_owners: &'a mut HashMap<Multiaddr, PeerId>,
    relay_registration: &'a relay::RelayRegistrationTrigger,
    connected_peers: &'a ConnectedPeerCount,
    connected_transports: &'a ConnectedTransportCounts,
}

/// Update local peer bookkeeping and metrics from a single swarm event.
fn handle_swarm_event(
    swarm: &mut Swarm<WaddleBehaviour>,
    event: SwarmEvent<WaddleBehaviourEvent>,
    state: SwarmEventBookkeeping<'_>,
) {
    match event {
        SwarmEvent::NewListenAddr { address, .. } => {
            tracing::info!(%address, "clustering swarm listening");
        }
        SwarmEvent::ConnectionEstablished {
            peer_id,
            connection_id,
            endpoint,
            num_established,
            ..
        } => {
            state
                .conn_addrs
                .insert(connection_id, endpoint.get_remote_address().clone());
            // Feed outbound (dialed) addresses to the behaviours: kademlia
            // only inserts a peer into its routing table once it knows a
            // dialable address for it. kameo's mDNS dev-bootstrap does this on
            // every discovery; our owned dialing must do it explicitly or the
            // routing table stays empty and every lookup fails. Inbound remote
            // addresses are ephemeral ports, never dialable — skip those.
            if endpoint.is_dialer() {
                swarm.add_peer_address(peer_id, endpoint.get_remote_address().clone());
                // Remember who owns this dialable address so the dial loop
                // can skip it while the peer stays connected (even if only
                // through an inbound connection).
                state
                    .addr_owners
                    .insert(endpoint.get_remote_address().clone(), peer_id);
            }
            // libp2p may establish multiple authenticated transport
            // connections to one PeerId when both sides dial concurrently.
            // Keep every connection: independently choosing and closing a
            // "duplicate" on each endpoint can make the peers retain opposite
            // links, after which both closes tear down the entire relationship.
            // Peer bookkeeping remains first/last-connection based.
            if track_peer_connection_established(
                peer_id,
                num_established.get(),
                state.connected,
                state.connected_peers,
                state.connected_transports,
            ) {
                state.relay_registration.trigger();
            }
        }
        SwarmEvent::ConnectionClosed {
            peer_id,
            connection_id,
            num_established,
            ..
        } => {
            state.conn_addrs.remove(&connection_id);
            // Only drop the peer when its last connection closes. Its
            // addr-ownership entries go with it: a fully disconnected peer
            // must be redialable (the skip would be wrong), and pruning here
            // keeps the map bounded by *connected* peers under pod churn
            // instead of accumulating every address ever dialed.
            if track_peer_connection_closed(
                peer_id,
                num_established,
                state.connected,
                state.connected_peers,
                state.connected_transports,
            ) {
                state.addr_owners.retain(|_, owner| *owner != peer_id);
            }
        }
        SwarmEvent::Behaviour(WaddleBehaviourEvent::Kameo(remote::Event::Registry(
            registry::Event::RoutingUpdated {
                peer,
                is_new_peer,
                old_peer,
                ..
            },
        ))) => {
            let mut changed = false;
            if is_new_peer {
                changed |= state.routing_peers.insert(peer);
            }
            if let Some(evicted) = old_peer {
                changed |= state.routing_peers.remove(&evicted);
            }
            if changed {
                metrics::record_routing_table_size(state.routing_peers.len() as i64);
                tracing::debug!(%peer, "clustering kademlia routing table updated");
            }
        }
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            tracing::debug!(?peer_id, %error, "clustering outgoing connection error");
        }
        // A failing or closed listener silently degrades the node (peers can
        // no longer dial it) — surface it loudly rather than letting it fall
        // through the wildcard.
        SwarmEvent::ListenerError { listener_id, error } => {
            tracing::warn!(?listener_id, %error, "clustering swarm listener error");
        }
        SwarmEvent::ListenerClosed {
            listener_id,
            addresses,
            reason,
        } => {
            tracing::warn!(
                ?listener_id,
                ?addresses,
                ?reason,
                "clustering swarm listener closed"
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClusteringConfig;
    use crate::db::{DatabaseConfig, DatabaseDriver};
    use base64::Engine as _;

    fn generated_pool_entry() -> (String, PeerId) {
        let keypair = libp2p::identity::ed25519::Keypair::generate();
        let seed = keypair.secret().as_ref().to_vec();
        let entry = base64::engine::general_purpose::STANDARD.encode(seed);
        let peer_id = libp2p::identity::Keypair::from(keypair)
            .public()
            .to_peer_id();
        (entry, peer_id)
    }

    // An in-memory SQLite handle for tests that fail before any DB touch
    // (listen-addr parsing precedes allowlist and lease access).
    async fn scratch_db() -> Database {
        Database::from_config(
            "clustering-swarm-test",
            &DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:".to_string()),
        )
        .await
        .expect("open scratch sqlite")
    }

    #[test]
    fn additional_connection_keeps_first_peer_bookkeeping_without_double_counting() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut connected = HashSet::new();
        let connected_peers = ConnectedPeerCount::new();
        let connected_transports = ConnectedTransportCounts::new();

        assert!(
            track_peer_connection_established(
                peer,
                1,
                &mut connected,
                &connected_peers,
                &connected_transports,
            ),
            "the first of two cross-dial connections marks the peer connected"
        );
        assert_eq!(connected_transports.get(&peer), 1);
        assert!(
            !track_peer_connection_established(
                peer,
                2,
                &mut connected,
                &connected_peers,
                &connected_transports,
            ),
            "the oppositely ordered cross-dial connection is retained without \
             counting a second peer"
        );
        assert_eq!(connected, HashSet::from([peer]));
        assert_eq!(connected_peers.get(), 1);
        assert_eq!(connected_transports.get(&peer), 2);

        // Closing either transport while the other remains must not remove
        // the peer. Only libp2p's last-connection event removes it.
        assert!(!track_peer_connection_closed(
            peer,
            1,
            &mut connected,
            &connected_peers,
            &connected_transports,
        ));
        assert_eq!(connected, HashSet::from([peer]));
        assert_eq!(connected_peers.get(), 1);
        assert_eq!(connected_transports.get(&peer), 1);
        assert!(track_peer_connection_closed(
            peer,
            0,
            &mut connected,
            &connected_peers,
            &connected_transports,
        ));
        assert!(connected.is_empty());
        assert_eq!(connected_peers.get(), 0);
        assert_eq!(connected_transports.get(&peer), 0);
    }

    #[test]
    fn keypair_pool_enrollment_preflight_rejects_empty_pool() {
        let config = ClusteringConfig::default();
        let error = verify_keypair_pool_enrollment(&config, &HashSet::new())
            .expect_err("empty keypair pool must fail clustered startup");
        assert!(
            matches!(error, SwarmError::KeypairPoolRequired),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn keypair_pool_enrollment_preflight_accepts_fully_enrolled_pool() {
        let (first_entry, first_peer) = generated_pool_entry();
        let (second_entry, second_peer) = generated_pool_entry();
        let config = ClusteringConfig {
            keypair_pool: vec![first_entry, second_entry],
            ..ClusteringConfig::default()
        };
        let enrolled = HashSet::from([first_peer, second_peer]);

        verify_keypair_pool_enrollment(&config, &enrolled).expect("all pool peers enrolled");
    }

    #[test]
    fn keypair_pool_enrollment_preflight_rejects_missing_pool_peer() {
        let (first_entry, first_peer) = generated_pool_entry();
        let (second_entry, second_peer) = generated_pool_entry();
        let config = ClusteringConfig {
            keypair_pool: vec![first_entry, second_entry],
            ..ClusteringConfig::default()
        };
        let enrolled = HashSet::from([first_peer]);

        let error = verify_keypair_pool_enrollment(&config, &enrolled)
            .expect_err("missing pool peer must fail startup");
        assert!(
            matches!(error, SwarmError::KeypairPoolNotEnrolled { .. }),
            "unexpected error: {error:?}"
        );
        assert!(
            error.to_string().contains(&second_peer.to_string()),
            "error should name the missing peer id: {error}"
        );
    }

    // kameo's `init_global` is a process singleton, so exactly ONE test per
    // test binary may successfully bring up the swarm. This smoke test
    // exercises the whole bring-up: keypair generation, allowlist load
    // (deny-all empty set), transport init (tcp + quic + Noise + yamux), the
    // global `ActorSwarm` install, and `listen_on` binding an ephemeral port —
    // then drives the event loop briefly and shuts it down on the token.
    // Postgres-gated: allowlist enforcement is unconditional and its store is
    // Postgres-only (as clustering itself is).
    #[tokio::test]
    async fn swarm_spawn_brings_up_and_shuts_down() {
        // Hold the shared table lock so a concurrently seeded (possibly
        // invalid) allowlist row from the allowlist tests cannot fail this
        // startup load.
        let _allowlist_guard = crate::clustering::allowlist_table_lock().lock().await;
        let _keypair_slot_guard = crate::clustering::keypair_slot_table_lock().lock().await;
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            return;
        };
        let (pool_entry, peer_id) = generated_pool_entry();
        let config = ClusteringConfig {
            enabled: true,
            listen_addrs: vec!["/ip4/127.0.0.1/tcp/0".to_string()],
            bootstrap_peers: Vec::new(),
            keypair_pool: vec![pool_entry],
            ..ClusteringConfig::default()
        };
        let db = Database::from_config(
            "clustering-swarm-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url),
        )
        .await
        .expect("open test postgres");
        let allowlist = PostgresAllowlistStore::new(db.clone());
        allowlist
            .ensure_schema()
            .await
            .expect("provision allowlist schema");
        PostgresKeypairSlotLease::new(db.clone())
            .ensure_schema()
            .await
            .expect("provision keypair-slot schema");
        let conn = db.guard().await.expect("guard allowlist seed");
        conn.execute("DELETE FROM clustering_peer_allowlist", ())
            .await
            .expect("clean allowlist table");
        conn.execute("DELETE FROM clustering_keypair_slots", ())
            .await
            .expect("clean keypair-slot table");
        conn.execute(
            "INSERT INTO clustering_peer_allowlist (peer_id) VALUES (?)",
            crate::db_params![peer_id.to_string()],
        )
        .await
        .expect("enroll smoke-test peer id");
        drop(conn);
        let stop = CancellationToken::new();

        let handle = spawn(
            &config,
            &db,
            stop.clone(),
            RelayBridges {
                resume_bridge: crate::clustering::resume_bridge::ResumeStealBridge::new(),
                room_local_claims: crate::clustering::local_claims::RoomLocalClaims::new(),
                ordered_relay_delivery_bridge:
                    crate::clustering::route_bridge::OrderedRelayDeliveryBridge::new(
                        stop.clone(),
                        &config.messaging,
                    ),
            },
        )
        .await
        .expect("swarm brings up cleanly");
        assert!(!handle.local_peer_id.to_string().is_empty());

        // Relay round-trip through the remote registry + ask path (kameo
        // short-circuits asks whose target peer is the local node, so this
        // exercises registration, kademlia lookup, payload serde, and the
        // handler without a second process; the true cross-node round-trip is
        // a Slice 6 harness case). Registration is async — retry until the
        // relay is discoverable so a slow CI runner cannot flake the test.
        let mut relay_handle = relay::RelayHandle::new(handle.node_id.clone(), stop.clone());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let pong = loop {
            match relay_handle.ping().await {
                Ok(pong) => break pong,
                Err(error) => {
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "relay never answered ping: {error}"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        };
        assert_eq!(pong.node_id, handle.node_id);

        // Codec proof over the ask path: a thread-carrying message survives
        // the RemoteStanza round-trip.
        let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
        message.thread = Some(xmpp_parsers::message::Thread {
            id: "relay-echo-thread".to_string(),
            parent: None,
        });
        let echoed = relay_handle
            .echo_stanza(super::super::codec::RemoteStanza(
                waddle_xmpp::Stanza::Message(message),
            ))
            .await
            .expect("relay echoes stanza");
        match echoed.stanza.0 {
            waddle_xmpp::Stanza::Message(message) => {
                assert_eq!(
                    message.thread.expect("thread survives").id,
                    "relay-echo-thread"
                );
            }
            other => panic!("expected message, got {}", other.name()),
        }

        stop.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn spawn_rejects_invalid_listen_multiaddr() {
        let config = ClusteringConfig {
            enabled: true,
            listen_addrs: vec!["not-a-multiaddr".to_string()],
            bootstrap_peers: Vec::new(),
            ..ClusteringConfig::default()
        };
        // Listen-addr validation fails before any DB access, so a SQLite
        // scratch handle is fine here.
        let db = scratch_db().await;
        let err = spawn(
            &config,
            &db,
            CancellationToken::new(),
            RelayBridges {
                resume_bridge: crate::clustering::resume_bridge::ResumeStealBridge::new(),
                room_local_claims: crate::clustering::local_claims::RoomLocalClaims::new(),
                ordered_relay_delivery_bridge:
                    crate::clustering::route_bridge::OrderedRelayDeliveryBridge::new(
                        CancellationToken::new(),
                        &config.messaging,
                    ),
            },
        )
        .await
        .expect_err("invalid addr rejected");
        assert!(matches!(err, SwarmError::ListenAddrInvalid { .. }));
    }

    // A `KeypairSlotLease` double that always fails renewal with a generic
    // (non-fencing) database error, so `run_heartbeat` never sees
    // `LeaseError::FencingLoss` and the only self-fence path exercised is the
    // lease-deadline check.
    struct PartitionedLease {
        release_called: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl KeypairSlotLease for PartitionedLease {
        async fn ensure_schema(&self) -> Result<(), LeaseError> {
            Ok(())
        }

        async fn acquire(
            &self,
            _identity: &LeaseIdentity,
            _pool_size: usize,
            _lease_ttl: Duration,
        ) -> Result<LeasedSlot, LeaseError> {
            Ok(LeasedSlot { slot_index: 0 })
        }

        async fn heartbeat(
            &self,
            _identity: &LeaseIdentity,
            _slot: LeasedSlot,
            _lease_ttl: Duration,
        ) -> Result<(), LeaseError> {
            Err(LeaseError::Database(crate::db::DatabaseError::QueryFailed(
                "simulated Postgres partition".to_string(),
            )))
        }

        async fn release(
            &self,
            _identity: &LeaseIdentity,
            _slot: LeasedSlot,
        ) -> Result<(), LeaseError> {
            self.release_called.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    // A node that can no longer renew its lease (e.g. partitioned from
    // Postgres, still alive on libp2p) must self-fence once `lease_ttl` has
    // elapsed since the last successful renewal — never retry forever, which
    // would let another node's legitimate steal-and-reuse of the same slot
    // produce two live nodes sharing one PeerId indefinitely.
    #[tokio::test(start_paused = true)]
    async fn heartbeat_self_fences_once_lease_deadline_blown_without_postgres() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_millis(200);
        let release_called = Arc::new(AtomicBool::new(false));
        let lease = PartitionedLease {
            release_called: Arc::clone(&release_called),
        };
        let node = LeaseIdentity {
            node_id: NodeId::generate(),
            node_epoch: uuid::Uuid::new_v4(),
        };
        let slot = LeasedSlot { slot_index: 0 };
        let stop_token = CancellationToken::new();
        let acquired_at = tokio::time::Instant::now();

        tokio::spawn(run_heartbeat(
            lease,
            node,
            slot,
            acquired_at,
            interval,
            lease_ttl,
            stop_token.clone(),
        ));

        // A few failing renewals inside the TTL window must not self-fence.
        tokio::time::advance(interval * 3).await;
        tokio::task::yield_now().await;
        assert!(
            !stop_token.is_cancelled(),
            "must not self-fence before the lease deadline elapses"
        );

        // Once the deadline is blown, the next failing tick self-fences the
        // clustering scope (not a graceful release — the slot may already be
        // held by whoever legitimately stole it).
        tokio::time::advance(lease_ttl).await;
        tokio::task::yield_now().await;
        assert!(
            stop_token.is_cancelled(),
            "must self-fence once the lease deadline is exceeded"
        );
        assert!(
            !release_called.load(Ordering::SeqCst),
            "deadline self-fence is not a graceful release"
        );
    }

    // A `KeypairSlotLease` double whose renewal HANGS forever (a black-holed
    // connection stuck in TCP retransmission), rather than erroring — the
    // failure mode a timeout-free `Err`-only deadline check can never see.
    struct HangingLease {
        release_called: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl KeypairSlotLease for HangingLease {
        async fn ensure_schema(&self) -> Result<(), LeaseError> {
            Ok(())
        }

        async fn acquire(
            &self,
            _identity: &LeaseIdentity,
            _pool_size: usize,
            _lease_ttl: Duration,
        ) -> Result<LeasedSlot, LeaseError> {
            Ok(LeasedSlot { slot_index: 0 })
        }

        async fn heartbeat(
            &self,
            _identity: &LeaseIdentity,
            _slot: LeasedSlot,
            _lease_ttl: Duration,
        ) -> Result<(), LeaseError> {
            std::future::pending().await
        }

        async fn release(
            &self,
            _identity: &LeaseIdentity,
            _slot: LeasedSlot,
        ) -> Result<(), LeaseError> {
            self.release_called.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    // A renewal that never resolves must still self-fence at the deadline:
    // the deadline select arm, not an `Err` from the renewal, is the fencing
    // trigger, so a hang cannot suppress it.
    #[tokio::test(start_paused = true)]
    async fn heartbeat_self_fences_when_renewal_hangs() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_millis(200);
        let release_called = Arc::new(AtomicBool::new(false));
        let lease = HangingLease {
            release_called: Arc::clone(&release_called),
        };
        let node = LeaseIdentity {
            node_id: NodeId::generate(),
            node_epoch: uuid::Uuid::new_v4(),
        };
        let slot = LeasedSlot { slot_index: 0 };
        let stop_token = CancellationToken::new();
        let acquired_at = tokio::time::Instant::now();

        tokio::spawn(run_heartbeat(
            lease,
            node,
            slot,
            acquired_at,
            interval,
            lease_ttl,
            stop_token.clone(),
        ));

        // The first tick starts a renewal that hangs; inside the TTL window
        // that must not fence.
        tokio::time::advance(interval * 3).await;
        tokio::task::yield_now().await;
        assert!(
            !stop_token.is_cancelled(),
            "a hung renewal must not self-fence before the lease deadline"
        );

        // The deadline arm fires at `acquired_at + lease_ttl` even though the
        // renewal never produced an `Err`.
        tokio::time::advance(lease_ttl).await;
        tokio::task::yield_now().await;
        assert!(
            stop_token.is_cancelled(),
            "a hung renewal must self-fence once the lease deadline is exceeded"
        );
        assert!(
            !release_called.load(Ordering::SeqCst),
            "deadline self-fence is not a graceful release"
        );
    }

    // A `KeypairSlotLease` double whose first renewal succeeds and every
    // later one hangs, so `last_success` moves off `acquired_at` — pinning
    // that the deadline is measured from the last SUCCESSFUL renewal, not
    // from acquisition.
    struct SucceedsOnceThenHangsLease {
        renewals: Arc<std::sync::atomic::AtomicU32>,
    }

    #[async_trait::async_trait]
    impl KeypairSlotLease for SucceedsOnceThenHangsLease {
        async fn ensure_schema(&self) -> Result<(), LeaseError> {
            Ok(())
        }

        async fn acquire(
            &self,
            _identity: &LeaseIdentity,
            _pool_size: usize,
            _lease_ttl: Duration,
        ) -> Result<LeasedSlot, LeaseError> {
            Ok(LeasedSlot { slot_index: 0 })
        }

        async fn heartbeat(
            &self,
            _identity: &LeaseIdentity,
            _slot: LeasedSlot,
            _lease_ttl: Duration,
        ) -> Result<(), LeaseError> {
            if self.renewals.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(())
            } else {
                std::future::pending().await
            }
        }

        async fn release(
            &self,
            _identity: &LeaseIdentity,
            _slot: LeasedSlot,
        ) -> Result<(), LeaseError> {
            Ok(())
        }
    }

    // Advance paused time in sub-interval steps, yielding between steps so
    // the heartbeat task actually runs at each timer firing. A single coarse
    // `advance` past several deadlines fires all their wakers before the task
    // is polled once, and the biased deadline arm then wins over a tick that
    // "happened" earlier — correct fencing behavior for a starved runtime,
    // but not the scheduling the test means to simulate.
    async fn advance_in_steps(total: Duration, step: Duration) {
        let mut advanced = Duration::ZERO;
        while advanced < total {
            tokio::time::advance(step).await;
            tokio::task::yield_now().await;
            advanced += step;
        }
    }

    // The deadline restarts from each successful renewal: after one success
    // at ~1 interval, a subsequent hang must fence at `success + lease_ttl`,
    // strictly LATER than `acquired_at + lease_ttl` — a regression to
    // measuring from acquisition fences early and fails the mid-window
    // assertion.
    #[tokio::test(start_paused = true)]
    async fn heartbeat_deadline_measures_from_last_successful_renewal() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_millis(200);
        let step = Duration::from_millis(10);
        let lease = SucceedsOnceThenHangsLease {
            renewals: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        let node = LeaseIdentity {
            node_id: NodeId::generate(),
            node_epoch: uuid::Uuid::new_v4(),
        };
        let slot = LeasedSlot { slot_index: 0 };
        let stop_token = CancellationToken::new();
        let acquired_at = tokio::time::Instant::now();

        tokio::spawn(run_heartbeat(
            lease,
            node,
            slot,
            acquired_at,
            interval,
            lease_ttl,
            stop_token.clone(),
        ));
        // First poll at t=0 so the renewal timer is based at acquisition.
        tokio::task::yield_now().await;

        // t=60ms: the first renewal (tick at 50ms) succeeded, resetting the
        // deadline to ~t=250-260ms; every later renewal hangs.
        advance_in_steps(Duration::from_millis(60), step).await;
        assert!(!stop_token.is_cancelled());

        // t=220ms: past `acquired_at + lease_ttl` (200ms) but inside the
        // renewed window — measuring from acquisition would fence here.
        advance_in_steps(Duration::from_millis(160), step).await;
        assert!(
            !stop_token.is_cancelled(),
            "deadline must restart from the successful renewal, not acquisition"
        );

        // t=280ms: past the renewed deadline — the hang must fence now.
        advance_in_steps(Duration::from_millis(60), step).await;
        assert!(
            stop_token.is_cancelled(),
            "a hang after a successful renewal must fence at success + lease_ttl"
        );
    }
}
