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
use super::{dns, identity, metrics, relay, NodeId};
use crate::config::{ClusteringBootstrapConfig, ClusteringConfig};
use crate::db::Database;
use core::convert::Infallible;
use futures::StreamExt;
use kameo::remote::{self, registry};
use libp2p::identity::Keypair;
use libp2p::swarm::SwarmEvent;
use libp2p::{noise, tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder};
use std::collections::{HashMap, HashSet};
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
}

/// Build the swarm, install the global `ActorSwarm`, start listening, spawn
/// the event loop and the supervised relay on `stop_token`, and return the
/// node's swarm identity.
///
/// When a keypair pool is configured the node leases one pool slot from the
/// Postgres control plane and uses that keypair (and heartbeats it); otherwise
/// it falls back to an ephemeral per-process identity.
pub async fn spawn(
    config: &ClusteringConfig,
    db: &Database,
    stop_token: CancellationToken,
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

    let keypair = node_keypair(config, db, &node_id, &stop_token).await?;
    let local_peer_id = keypair.public().to_peer_id();

    // Peer authorization (ADR element 3): load the enrolled peer set before
    // the swarm accepts anything. An empty allowlist is deny-all — correct for
    // a first node with no peers, but worth a loud note.
    let allowlist: Arc<dyn AllowlistStore> = Arc::new(PostgresAllowlistStore::new(db.clone()));
    allowlist.ensure_schema().await?;
    let enrolled = allowlist.enrolled_peers().await?;
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

    let bootstrap_peers = config.bootstrap_peers.clone();
    tokio::spawn(run_event_loop(
        swarm,
        bootstrap_peers,
        config.dial_interval,
        AllowlistRefresh {
            store: allowlist,
            current: enrolled,
            interval: config.allowlist_refresh_interval,
        },
        stop_token.clone(),
    ));

    // The node's single kademlia registration: its supervised relay actor.
    // Must start after the event loop is polling (registration flows through
    // the swarm command channel serviced by the loop).
    if config.fault_injection {
        tracing::warn!(
            "clustering fault injection is ENABLED: any enrolled peer can crash this node's \
             relay or stall its mailbox — test harnesses only, never production"
        );
    }
    relay::spawn_supervised(node_id.clone(), config.fault_injection, stop_token);

    let handle = SwarmHandle {
        local_peer_id,
        node_id,
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

/// Resolve this node's libp2p keypair: lease a pool slot (and start its
/// heartbeat) when a keypair pool is configured, else generate an ephemeral
/// identity.
async fn node_keypair(
    config: &ClusteringConfig,
    db: &Database,
    node_id: &NodeId,
    stop_token: &CancellationToken,
) -> Result<Keypair, SwarmError> {
    if config.keypair_pool.is_empty() {
        tracing::warn!(
            "clustering: no keypair pool configured — using an ephemeral per-process identity. \
             An ephemeral PeerId can never be pre-enrolled on the peer allowlist, so this node \
             CANNOT join or form a cluster (allowlist enforcement is symmetric and unconditional); \
             configure WADDLE_CLUSTERING_KEYPAIR_POOL and enroll every node's PeerId for any \
             multi-node deployment"
        );
        return Ok(identity::ephemeral_keypair());
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
    // `acquire` only ever returns an in-range slot, so this index is valid.
    let entry = &config.keypair_pool[slot.slot_index as usize];
    let keypair = match identity::keypair_from_pool_entry(entry) {
        Ok(keypair) => keypair,
        Err(error) => {
            // Best-effort release: without it a malformed pool entry holds
            // the slot hostage until lease-TTL expiry while this node
            // crash-loops onto fresh slots, transiently shrinking the pool
            // for every other node.
            if let Err(release_error) = lease.release(&node, slot).await {
                tracing::warn!(
                    %release_error,
                    slot = slot.slot_index,
                    "failed to release keypair slot after decode failure"
                );
            }
            return Err(error.into());
        }
    };
    tracing::info!(
        slot = slot.slot_index,
        pool_size,
        "clustering: leased keypair-pool slot for node identity"
    );

    tokio::spawn(run_heartbeat(
        lease,
        node,
        slot,
        config.lease.heartbeat_interval,
        config.lease.lease_ttl,
        stop_token.clone(),
    ));

    Ok(keypair)
}

/// Renew the keypair-slot lease on a timer until shutdown; release the slot on
/// graceful stop, and self-fence (cancel the swarm) on fencing loss so a
/// superseded identity stops serving.
async fn run_heartbeat<L>(
    lease: L,
    node: LeaseIdentity,
    slot: LeasedSlot,
    interval: Duration,
    lease_ttl: Duration,
    stop_token: CancellationToken,
) where
    L: KeypairSlotLease + Send + Sync + 'static,
{
    let mut timer = tokio::time::interval(interval);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = stop_token.cancelled() => {
                if let Err(error) = lease.release(&node, slot).await {
                    tracing::warn!(%error, slot = slot.slot_index, "clustering keypair-slot release failed on shutdown");
                }
                break;
            }
            _ = timer.tick() => {
                match lease.heartbeat(&node, slot, lease_ttl).await {
                    Ok(()) => {}
                    Err(LeaseError::FencingLoss { slot_index }) => {
                        tracing::error!(
                            slot = slot_index,
                            "clustering keypair-slot lease lost (fencing) — self-fencing swarm"
                        );
                        // The identity is no longer ours; stop the swarm.
                        stop_token.cancel();
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(%error, slot = slot.slot_index, "clustering keypair-slot heartbeat error; will retry next tick");
                    }
                }
            }
        }
    }
}

/// Drive the swarm until `stop_token` fires: dial seed peers on a timer,
/// refresh the peer allowlist on a timer (revoking removed peers), feed swarm
/// events to the handler, and stop cleanly on shutdown.
async fn run_event_loop(
    mut swarm: Swarm<WaddleBehaviour>,
    bootstrap_peers: Vec<ClusteringBootstrapConfig>,
    dial_interval: Duration,
    mut allowlist: AllowlistRefresh,
    stop_token: CancellationToken,
) {
    // Peers we currently hold a connection to (authoritative from connection
    // events), peers observed to enter the kademlia routing table, and the
    // remote address of every live connection (so the bootstrap dial loop can
    // skip endpoints it is already connected to instead of churning a fresh
    // connection every interval that the duplicate-PeerId defense then
    // closes).
    let mut connected: HashSet<PeerId> = HashSet::new();
    let mut routing_peers: HashSet<PeerId> = HashSet::new();
    let mut conn_addrs: HashMap<libp2p::swarm::ConnectionId, Multiaddr> = HashMap::new();
    // Which peer answered at each dialed address, learned from outbound
    // establishments and pruned when the owner's last connection closes (so
    // the map stays bounded by connected peers under pod churn, and a fully
    // disconnected peer stays redialable). Lets the dial loop skip a seed
    // whose owner is connected *inbound-only* — `conn_addrs` alone cannot,
    // because an inbound connection's remote address is an ephemeral source
    // port that never matches the seed addr, so every interval would churn a
    // duplicate connection for the duplicate-PeerId defense to close.
    let mut addr_owners: HashMap<Multiaddr, PeerId> = HashMap::new();

    // `interval` fires its first tick immediately, so the first dial round
    // happens right away rather than after a full interval.
    let mut dial_timer = tokio::time::interval(dial_interval);
    dial_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
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
        tokio::select! {
            _ = stop_token.cancelled() => {
                tracing::info!("clustering swarm event loop stopping (shutdown)");
                break;
            }
            _ = dial_timer.tick() => {
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
            Some(addr) = dial_rx.recv() => {
                // Skip endpoints we already hold a connection to: redialing a
                // connected peer every interval would just mint a duplicate
                // connection for the duplicate-PeerId defense to close. Two
                // checks: an outbound connection's remote address matches the
                // seed addr directly; an inbound-only peer is recognized via
                // the addr's last known owner.
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
                if let Err(error) = swarm.dial(addr.clone()) {
                    tracing::debug!(%addr, %error, "clustering bootstrap dial failed");
                }
            }
            event = swarm.select_next_some() => {
                handle_swarm_event(
                    &mut swarm,
                    event,
                    &mut connected,
                    &mut routing_peers,
                    &mut conn_addrs,
                    &mut addr_owners,
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

/// Record a newly connected peer, updating the connected-peer gauge on the
/// first connection to that peer.
fn track_peer_connected(peer_id: PeerId, connected: &mut HashSet<PeerId>) {
    if connected.insert(peer_id) {
        metrics::record_connected_peers(connected.len() as i64);
        tracing::debug!(%peer_id, "clustering peer connected");
    }
}

/// Record a peer whose last connection closed, updating the connected-peer
/// gauge if it was tracked.
fn track_peer_disconnected(peer_id: PeerId, connected: &mut HashSet<PeerId>) {
    if connected.remove(&peer_id) {
        metrics::record_connected_peers(connected.len() as i64);
        tracing::debug!(%peer_id, "clustering peer disconnected");
    }
}

/// Update local peer bookkeeping and metrics from a single swarm event. Takes
/// `&mut swarm` so it can close duplicate connections (duplicate-PeerId
/// defense-in-depth, ADR element 3).
fn handle_swarm_event(
    swarm: &mut Swarm<WaddleBehaviour>,
    event: SwarmEvent<WaddleBehaviourEvent>,
    connected: &mut HashSet<PeerId>,
    routing_peers: &mut HashSet<PeerId>,
    conn_addrs: &mut HashMap<libp2p::swarm::ConnectionId, Multiaddr>,
    addr_owners: &mut HashMap<Multiaddr, PeerId>,
) {
    match event {
        SwarmEvent::NewListenAddr { address, .. } => {
            tracing::info!(%address, "clustering swarm listening");
        }
        SwarmEvent::ConnectionEstablished {
            peer_id,
            connection_id,
            endpoint,
            ..
        } => {
            conn_addrs.insert(connection_id, endpoint.get_remote_address().clone());
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
                addr_owners.insert(endpoint.get_remote_address().clone(), peer_id);
            }
            // Duplicate-PeerId defense-in-depth (ADR element 3): the
            // keypair-slot lease already guarantees a unique PeerId per live
            // node, so a second concurrent connection to an already-connected
            // peer is unexpected — keep the first, reject the new one.
            if connected.contains(&peer_id) {
                tracing::warn!(
                    %peer_id,
                    "clustering: rejecting duplicate connection to already-connected peer"
                );
                swarm.close_connection(connection_id);
            } else {
                track_peer_connected(peer_id, connected);
            }
        }
        SwarmEvent::ConnectionClosed {
            peer_id,
            connection_id,
            num_established,
            ..
        } => {
            conn_addrs.remove(&connection_id);
            // Only drop the peer when its last connection closes. Its
            // addr-ownership entries go with it: a fully disconnected peer
            // must be redialable (the skip would be wrong), and pruning here
            // keeps the map bounded by *connected* peers under pod churn
            // instead of accumulating every address ever dialed.
            if num_established == 0 {
                track_peer_disconnected(peer_id, connected);
                addr_owners.retain(|_, owner| *owner != peer_id);
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
                changed |= routing_peers.insert(peer);
            }
            if let Some(evicted) = old_peer {
                changed |= routing_peers.remove(&evicted);
            }
            if changed {
                metrics::record_routing_table_size(routing_peers.len() as i64);
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
        let _guard = crate::clustering::allowlist_table_lock().lock().await;
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            return;
        };
        let config = ClusteringConfig {
            enabled: true,
            listen_addrs: vec!["/ip4/127.0.0.1/tcp/0".to_string()],
            bootstrap_peers: Vec::new(),
            ..ClusteringConfig::default()
        };
        let db = Database::from_config(
            "clustering-swarm-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url),
        )
        .await
        .expect("open test postgres");
        let stop = CancellationToken::new();

        let handle = spawn(&config, &db, stop.clone())
            .await
            .expect("swarm brings up cleanly");
        assert!(!handle.local_peer_id.to_string().is_empty());

        // Relay round-trip through the remote registry + ask path (kameo
        // short-circuits asks whose target peer is the local node, so this
        // exercises registration, kademlia lookup, payload serde, and the
        // handler without a second process; the true cross-node round-trip is
        // a Slice 6 harness case). Registration is async — retry until the
        // relay is discoverable so a slow CI runner cannot flake the test.
        let mut relay_handle = relay::RelayHandle::new(handle.node_id.clone());
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
        let err = spawn(&config, &db, CancellationToken::new())
            .await
            .expect_err("invalid addr rejected");
        assert!(matches!(err, SwarmError::ListenAddrInvalid { .. }));
    }
}
