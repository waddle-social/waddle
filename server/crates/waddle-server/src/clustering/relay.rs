//! Per-node relay actor for the clustering swarm (ADR-0017 elements 5/6).
//!
//! Each node registers exactly **one** name in kademlia: its relay actor,
//! keyed by the node's per-instance `node_id`. Kademlia carries node
//! discovery only — entity→node resolution is Phase 3's Postgres claims
//! table, and per-entity DHT registration is ruled out by kameo 0.20's
//! hardcoded `MemoryStore` limits.
//!
//! The relay is **supervised**: kameo auto-unregisters an actor that stops or
//! panics (removing this node from the routing fabric while its Postgres
//! heartbeat stays fresh — a steady-state, cluster-wide degradation with no
//! self-healing path), so an owning task respawns it and **re-registers it
//! under the same name**. Sender-side `ActorNotRunning`/`UnknownActor`/
//! `BadActorType` errors trigger a bounded-backoff kademlia re-lookup — the
//! transport-layer refresh path, distinct from Phase 3's `NotOwner`
//! claims-refresh path.
//!
//! Phase 2 is **discovery only**: the relay's message set proves the
//! cross-node ask round-trip and the XML codec on the wire (spike exit
//! criteria); it is not wired into the stanza delivery path (Phase 4).

use super::codec::RemoteStanza;
use super::metrics;
use kameo::actor::{ActorRef, RemoteActorRef, Spawn};
use kameo::error::RemoteSendError;
use kameo::message::{Context, Message};
use kameo::{Actor, RemoteActor, Reply};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// The kademlia registration name for a node's relay actor — the node's ONLY
/// kademlia name (O(1) registrations per node, never per entity).
pub fn relay_name(node_id: &str) -> String {
    format!("waddle-relay/{node_id}")
}

/// Backoff between supervised respawn/re-registration attempts.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(1);

/// Bounded backoff schedule for sender-side relay re-lookup.
const LOOKUP_BACKOFF: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(400),
    Duration::from_millis(1_600),
];

/// The per-node relay actor. Phase 2 carries only the liveness/codec-proof
/// message set; the ordered per-peer relay channel semantics (sequencing, gap
/// detection, sticky failover) land with cross-node routing in Phase 4.
#[derive(Actor, RemoteActor)]
pub struct RelayActor {
    node_id: String,
}

impl RelayActor {
    pub fn new(node_id: String) -> Self {
        Self { node_id }
    }
}

/// Liveness probe: which node answers this relay name?
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayPing;

/// Reply to [`RelayPing`].
#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub struct RelayPong {
    pub node_id: String,
}

#[kameo::remote_message("waddle.clustering.relay.ping.v1")]
impl Message<RelayPing> for RelayActor {
    type Reply = RelayPong;

    async fn handle(
        &mut self,
        _msg: RelayPing,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> RelayPong {
        RelayPong {
            node_id: self.node_id.clone(),
        }
    }
}

/// Codec proof: carry a stanza across the wire and echo it back, exercising
/// the bounded XML codec's decode on the receiver and re-encode on the reply
/// path in one round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayEchoStanza {
    pub stanza: RemoteStanza,
}

/// Reply to [`RelayEchoStanza`].
#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub struct RelayEchoReply {
    pub node_id: String,
    pub stanza: RemoteStanza,
}

#[kameo::remote_message("waddle.clustering.relay.echo_stanza.v1")]
impl Message<RelayEchoStanza> for RelayActor {
    type Reply = RelayEchoReply;

    async fn handle(
        &mut self,
        msg: RelayEchoStanza,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> RelayEchoReply {
        RelayEchoReply {
            node_id: self.node_id.clone(),
            stanza: msg.stanza,
        }
    }
}

/// Spawn the node's relay actor under supervision: register it in kademlia
/// under [`relay_name`], respawn it if it ever stops unexpectedly, and
/// **re-register under the same name** on every respawn (kameo auto-registers
/// removal on actor stop, so re-registration is mandatory, not optional).
/// Stops cleanly when `stop_token` fires.
pub fn spawn_supervised(node_id: String, stop_token: CancellationToken) {
    tokio::spawn(async move {
        let name = relay_name(&node_id);
        let mut respawns: u64 = 0;
        loop {
            if stop_token.is_cancelled() {
                break;
            }
            let actor_ref: ActorRef<RelayActor> =
                RelayActor::spawn(RelayActor::new(node_id.clone()));
            match actor_ref.register(name.clone()).await {
                Ok(()) => {
                    if respawns > 0 {
                        metrics::record_relay_respawn();
                        tracing::warn!(
                            %name,
                            respawns,
                            "clustering relay actor respawned and re-registered under the same name"
                        );
                    } else {
                        tracing::info!(%name, "clustering relay actor registered");
                    }
                }
                Err(error) => {
                    tracing::warn!(%name, %error, "clustering relay registration failed; retrying");
                    actor_ref.kill();
                    tokio::time::sleep(RESPAWN_BACKOFF).await;
                    continue;
                }
            }

            tokio::select! {
                _ = stop_token.cancelled() => {
                    // Graceful stop: unregister the name proactively so peers
                    // stop resolving a dead relay, then stop the actor.
                    let _ = kameo::remote::unregister(name.clone()).await;
                    let _ = actor_ref.stop_gracefully().await;
                    break;
                }
                _ = actor_ref.wait_for_shutdown() => {
                    // Unexpected stop/panic: kameo has already unregistered
                    // the name. Respawn and re-register under the SAME name
                    // after a short backoff.
                    respawns += 1;
                    tokio::time::sleep(RESPAWN_BACKOFF).await;
                }
            }
        }
    });
}

/// A client handle to some node's relay, caching the resolved
/// `RemoteActorRef` and refreshing it via bounded-backoff kademlia re-lookup
/// when the transport reports the cached ref dead
/// (`ActorNotRunning`/`UnknownActor`/`BadActorType` — e.g. after a supervised
/// respawn minted a new `ActorId` under the same name).
pub struct RelayHandle {
    node_id: String,
    cached: Option<RemoteActorRef<RelayActor>>,
}

/// Failures asking a remote relay.
#[derive(Debug, thiserror::Error)]
pub enum RelayAskError {
    /// The relay name did not resolve in kademlia within the backoff budget.
    #[error("relay for node '{node_id}' not found in the swarm registry")]
    NotFound { node_id: String },
    /// The ask failed at the transport/handler layer.
    #[error("relay ask failed: {0}")]
    Send(String),
}

impl RelayHandle {
    pub fn new(node_id: String) -> Self {
        Self {
            node_id,
            cached: None,
        }
    }

    /// Resolve the relay ref, using the cache when warm and a bounded-backoff
    /// kademlia lookup when cold.
    async fn resolve(&mut self) -> Result<RemoteActorRef<RelayActor>, RelayAskError> {
        if let Some(cached) = &self.cached {
            return Ok(cached.clone());
        }
        let name = relay_name(&self.node_id);
        for backoff in LOOKUP_BACKOFF {
            match RemoteActorRef::<RelayActor>::lookup(name.clone()).await {
                Ok(Some(remote_ref)) => {
                    self.cached = Some(remote_ref.clone());
                    return Ok(remote_ref);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(%name, %error, "clustering relay lookup error; backing off");
                }
            }
            tokio::time::sleep(backoff).await;
        }
        Err(RelayAskError::NotFound {
            node_id: self.node_id.clone(),
        })
    }

    /// Ping the relay, refreshing a stale cached ref once: transport-layer
    /// errors that mean "this ActorId is gone" invalidate the cache and
    /// re-resolve via kademlia, then retry the ask exactly once.
    pub async fn ping(&mut self) -> Result<RelayPong, RelayAskError> {
        let remote_ref = self.resolve().await?;
        match remote_ref.ask(&RelayPing).await {
            Ok(pong) => Ok(pong),
            Err(error) if is_stale_ref_error(&error) => {
                tracing::debug!(
                    node_id = %self.node_id,
                    %error,
                    "clustering relay ref stale; re-resolving via kademlia"
                );
                self.cached = None;
                let remote_ref = self.resolve().await?;
                remote_ref
                    .ask(&RelayPing)
                    .await
                    .map_err(|error| RelayAskError::Send(error.to_string()))
            }
            Err(error) => Err(RelayAskError::Send(error.to_string())),
        }
    }

    /// Round-trip a stanza through the relay (codec proof), with the same
    /// stale-ref refresh-and-retry-once behaviour as [`Self::ping`].
    pub async fn echo_stanza(
        &mut self,
        stanza: RemoteStanza,
    ) -> Result<RelayEchoReply, RelayAskError> {
        let message = RelayEchoStanza { stanza };
        let remote_ref = self.resolve().await?;
        match remote_ref.ask(&message).await {
            Ok(reply) => Ok(reply),
            Err(error) if is_stale_ref_error(&error) => {
                self.cached = None;
                let remote_ref = self.resolve().await?;
                remote_ref
                    .ask(&message)
                    .await
                    .map_err(|error| RelayAskError::Send(error.to_string()))
            }
            Err(error) => Err(RelayAskError::Send(error.to_string())),
        }
    }
}

/// Transport-layer errors meaning the cached `RemoteActorRef` no longer names
/// a live actor — the explicit re-lookup trigger (ADR element 6). Distinct
/// from handler/timeout errors, which say nothing about registration state.
fn is_stale_ref_error<E>(error: &RemoteSendError<E>) -> bool {
    matches!(
        error,
        RemoteSendError::ActorNotRunning
            | RemoteSendError::ActorStopped
            | RemoteSendError::UnknownActor { .. }
            | RemoteSendError::BadActorType
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_name_is_node_scoped() {
        assert_eq!(relay_name("node-1"), "waddle-relay/node-1");
        assert_ne!(relay_name("node-1"), relay_name("node-2"));
    }

    #[test]
    fn stale_ref_errors_trigger_relookup_and_others_do_not() {
        assert!(is_stale_ref_error::<std::convert::Infallible>(
            &RemoteSendError::ActorNotRunning
        ));
        assert!(is_stale_ref_error::<std::convert::Infallible>(
            &RemoteSendError::BadActorType
        ));
        assert!(!is_stale_ref_error::<std::convert::Infallible>(
            &RemoteSendError::ReplyTimeout
        ));
        assert!(!is_stale_ref_error::<std::convert::Infallible>(
            &RemoteSendError::MailboxFull
        ));
    }
}
