//! The owned libp2p swarm: build, listen, dial, and drive the event loop.
//!
//! Node discovery only this phase. We build a swarm we own (tcp + quic,
//! Noise, yamux), install the process-global kameo `ActorSwarm` so remote
//! actor lookups resolve (used by the relay actors in a later slice), listen
//! on the configured multiaddrs, dial headless-DNS-resolved seed peers, and
//! run the event loop until the shared shutdown token fires. No stanza is
//! routed cross-node.

use super::behaviour::{WaddleBehaviour, WaddleBehaviourEvent};
use super::{dns, identity, metrics};
use crate::config::{ClusteringBootstrapConfig, ClusteringConfig};
use core::convert::Infallible;
use futures::StreamExt;
use kameo::remote::{self, registry};
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
}

/// Build the swarm, install the global `ActorSwarm`, start listening, and
/// spawn the event loop on `stop_token`. Returns the local `PeerId`.
pub fn spawn(
    config: &ClusteringConfig,
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

    let keypair = identity::node_keypair();
    let local_peer_id = keypair.public().to_peer_id();

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
        .with_behaviour(|key| WaddleBehaviour::new(key.public().to_peer_id(), messaging_config))
        .map_err(|never: Infallible| match never {})?
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
    tokio::spawn(run_event_loop(swarm, bootstrap, stop_token));

    Ok(local_peer_id)
}

/// Drive the swarm until `stop_token` fires: dial seed peers on a timer, feed
/// swarm events to the handler, and stop cleanly on shutdown.
async fn run_event_loop(
    mut swarm: Swarm<WaddleBehaviour>,
    bootstrap: Option<ClusteringBootstrapConfig>,
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
            Some(addr) = dial_rx.recv() => {
                metrics::record_bootstrap_dial();
                if let Err(error) = swarm.dial(addr.clone()) {
                    tracing::debug!(%addr, %error, "clustering bootstrap dial failed");
                }
            }
            event = swarm.select_next_some() => {
                handle_swarm_event(event, &mut connected, &mut routing_peers);
            }
        }
    }
}

/// Update local peer bookkeeping and metrics from a single swarm event.
fn handle_swarm_event(
    event: SwarmEvent<WaddleBehaviourEvent>,
    connected: &mut HashSet<PeerId>,
    routing_peers: &mut HashSet<PeerId>,
) {
    match event {
        SwarmEvent::NewListenAddr { address, .. } => {
            tracing::info!(%address, "clustering swarm listening");
        }
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            if connected.insert(peer_id) {
                metrics::record_connected_peers(connected.len() as i64);
                tracing::debug!(%peer_id, "clustering peer connected");
            }
        }
        SwarmEvent::ConnectionClosed {
            peer_id,
            num_established,
            ..
        } => {
            // Only drop the peer when its last connection closes.
            if num_established == 0 && connected.remove(&peer_id) {
                metrics::record_connected_peers(connected.len() as i64);
                tracing::debug!(%peer_id, "clustering peer disconnected");
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
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClusteringConfig;

    // kameo's `init_global` is a process singleton, so exactly ONE test per
    // test binary may successfully bring up the swarm. This smoke test
    // exercises the whole bring-up: keypair generation, transport init (tcp +
    // quic + Noise + yamux), the global `ActorSwarm` install, and `listen_on`
    // binding an ephemeral port — then drives the event loop briefly and shuts
    // it down on the token.
    #[tokio::test]
    async fn swarm_spawn_brings_up_and_shuts_down() {
        let config = ClusteringConfig {
            enabled: true,
            listen_addrs: vec!["/ip4/127.0.0.1/tcp/0".to_string()],
            bootstrap: None,
            ..ClusteringConfig::default()
        };
        let stop = CancellationToken::new();

        let peer_id = spawn(&config, stop.clone()).expect("swarm brings up cleanly");
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
        let err = spawn(&config, CancellationToken::new()).expect_err("invalid addr rejected");
        assert!(matches!(err, SwarmError::ListenAddrInvalid { .. }));
    }
}
