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
use super::{dns, identity, metrics};
use crate::config::{ClusteringBootstrapConfig, ClusteringConfig};
use crate::db::Database;
use core::convert::Infallible;
use futures::StreamExt;
use kameo::remote::{self, registry};
use libp2p::identity::Keypair;
use libp2p::swarm::SwarmEvent;
use libp2p::{noise, tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

/// How often the event loop re-resolves the headless-DNS name and dials any
/// seed peers, to pick up pod churn (rolling restarts, scale changes).
const BOOTSTRAP_DIAL_INTERVAL: Duration = Duration::from_secs(15);

/// Buffer for resolved-peer multiaddrs flowing from the off-loop DNS resolver
/// back to the event loop for dialing.
const DIAL_CHANNEL_CAPACITY: usize = 64;

/// Buffer for enrolled-peer sets flowing from the off-loop allowlist reader
/// back to the event loop. Single-flight refreshes mean at most one set is
/// ever in flight; a small buffer keeps the sender from awaiting.
const ALLOWLIST_CHANNEL_CAPACITY: usize = 4;

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
}

/// Build the swarm, install the global `ActorSwarm`, start listening, and
/// spawn the event loop on `stop_token`. Returns the local `PeerId`.
///
/// When a keypair pool is configured the node leases one pool slot from the
/// Postgres control plane and uses that keypair (and heartbeats it); otherwise
/// it falls back to an ephemeral per-process identity.
pub async fn spawn(
    config: &ClusteringConfig,
    db: &Database,
    stop_token: CancellationToken,
) -> Result<PeerId, SwarmError> {
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

    let keypair = node_keypair(config, db, &stop_token).await?;
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

    let bootstrap = config.bootstrap.clone();
    tokio::spawn(run_event_loop(
        swarm,
        bootstrap,
        AllowlistRefresh {
            store: allowlist,
            current: enrolled,
            interval: config.allowlist_refresh_interval,
        },
        stop_token,
    ));

    Ok(local_peer_id)
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
    stop_token: &CancellationToken,
) -> Result<Keypair, SwarmError> {
    if config.keypair_pool.is_empty() {
        tracing::warn!(
            "clustering: no keypair pool configured — using an ephemeral per-process identity \
             (no stable or revocable PeerId; configure WADDLE_CLUSTERING_KEYPAIR_POOL for production)"
        );
        return Ok(identity::ephemeral_keypair());
    }

    let lease = PostgresKeypairSlotLease::new(db.clone());
    lease.ensure_schema().await?;
    let node = LeaseIdentity {
        node_id: uuid::Uuid::new_v4().to_string(),
        node_epoch: uuid::Uuid::new_v4().to_string(),
    };
    let pool_size = config.keypair_pool.len();
    let slot = lease
        .acquire(&node, pool_size, config.lease.lease_ttl)
        .await?;
    // `acquire` only ever returns an in-range slot, so this index is valid.
    let entry = &config.keypair_pool[slot.slot_index as usize];
    let keypair = identity::keypair_from_pool_entry(entry)?;
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
async fn run_heartbeat(
    lease: PostgresKeypairSlotLease,
    node: LeaseIdentity,
    slot: LeasedSlot,
    interval: Duration,
    lease_ttl: Duration,
    stop_token: CancellationToken,
) {
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
    bootstrap: Option<ClusteringBootstrapConfig>,
    mut allowlist: AllowlistRefresh,
    stop_token: CancellationToken,
) {
    // Peers we currently hold a connection to (authoritative from connection
    // events) and peers observed to enter the kademlia routing table.
    let mut connected: HashSet<PeerId> = HashSet::new();
    let mut routing_peers: HashSet<PeerId> = HashSet::new();

    // `interval` fires its first tick immediately, so the first dial round
    // happens right away rather than after a full interval.
    let mut dial_timer = tokio::time::interval(BOOTSTRAP_DIAL_INTERVAL);
    dial_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut allowlist_timer = tokio::time::interval(allowlist.interval);
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

    loop {
        tokio::select! {
            _ = stop_token.cancelled() => {
                tracing::info!("clustering swarm event loop stopping (shutdown)");
                break;
            }
            _ = dial_timer.tick() => {
                if let Some(ref bootstrap) = bootstrap {
                    if !dns_in_flight.swap(true, Ordering::AcqRel) {
                        let bootstrap = bootstrap.clone();
                        let dial_tx = dial_tx.clone();
                        let in_flight = Arc::clone(&dns_in_flight);
                        tokio::spawn(async move {
                            for addr in dns::resolve_bootstrap_peers(&bootstrap).await {
                                if dial_tx.send(addr).await.is_err() {
                                    break;
                                }
                            }
                            in_flight.store(false, Ordering::Release);
                        });
                    }
                }
            }
            _ = allowlist_timer.tick() => {
                if !allowlist_in_flight.swap(true, Ordering::AcqRel) {
                    let store = Arc::clone(&allowlist.store);
                    let allowlist_tx = allowlist_tx.clone();
                    let in_flight = Arc::clone(&allowlist_in_flight);
                    tokio::spawn(async move {
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
                        in_flight.store(false, Ordering::Release);
                    });
                }
            }
            Some(enrolled) = allowlist_rx.recv() => {
                apply_allowlist(&mut swarm, &mut allowlist, enrolled);
            }
            Some(addr) = dial_rx.recv() => {
                metrics::record_bootstrap_dial();
                if let Err(error) = swarm.dial(addr.clone()) {
                    tracing::debug!(%addr, %error, "clustering bootstrap dial failed");
                }
            }
            event = swarm.select_next_some() => {
                handle_swarm_event(&mut swarm, event, &mut connected, &mut routing_peers);
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
) {
    match event {
        SwarmEvent::NewListenAddr { address, .. } => {
            tracing::info!(%address, "clustering swarm listening");
        }
        SwarmEvent::ConnectionEstablished {
            peer_id,
            connection_id,
            ..
        } => {
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
        // Only drop the peer when its last connection closes (num_established 0).
        SwarmEvent::ConnectionClosed {
            peer_id,
            num_established: 0,
            ..
        } => {
            track_peer_disconnected(peer_id, connected);
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
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            return;
        };
        let config = ClusteringConfig {
            enabled: true,
            listen_addrs: vec!["/ip4/127.0.0.1/tcp/0".to_string()],
            bootstrap: None,
            ..ClusteringConfig::default()
        };
        let db = Database::from_config(
            "clustering-swarm-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url),
        )
        .await
        .expect("open test postgres");
        let stop = CancellationToken::new();

        let peer_id = spawn(&config, &db, stop.clone())
            .await
            .expect("swarm brings up cleanly");
        assert!(!peer_id.to_string().is_empty());

        // Let the event loop run a beat so a NewListenAddr is processed, then
        // signal shutdown; the loop must stop on the token.
        tokio::time::sleep(Duration::from_millis(200)).await;
        stop.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn spawn_rejects_invalid_listen_multiaddr() {
        let config = ClusteringConfig {
            enabled: true,
            listen_addrs: vec!["not-a-multiaddr".to_string()],
            bootstrap: None,
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
