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
use super::NodeId;
use kameo::actor::{ActorRef, RemoteActorRef, Spawn};
use kameo::error::RemoteSendError;
use kameo::message::{Context, Message};
use kameo::{Actor, RemoteActor, Reply};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// The kademlia registration name for a node's relay actor — the node's ONLY
/// kademlia name (O(1) registrations per node, never per entity).
pub fn relay_name(node_id: &NodeId) -> String {
    format!("waddle-relay/{node_id}")
}

/// Backoff between supervised respawn/re-registration attempts.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(1);

/// Periodic same-name re-registration cadence. A registration performed
/// before the node's first peer connection stores its provider record only
/// locally (nobody can discover it), and kademlia's own republish is 30
/// minutes out — so the supervisor re-registers on this cadence (same-peer
/// metadata overwrite is permitted), bounding how long a freshly
/// (re)started node stays undiscoverable to roughly one interval after its
/// first peer connection.
const REREGISTER_INTERVAL: Duration = Duration::from_secs(15);

/// Upper bound on a fault-injected relay sleep.
const MAX_FAULT_SLEEP_MS: u64 = 60_000;

/// Bounded backoff schedule for sender-side relay re-lookup.
const LOOKUP_BACKOFF: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(400),
    Duration::from_millis(1_600),
];

/// The per-node relay actor. Phase 2 carries only the liveness/codec-proof
/// message set plus harness fault-injection; the ordered per-peer relay
/// channel semantics (sequencing, gap detection, sticky failover) land with
/// cross-node routing in Phase 4.
// The id is pinned via the derive's attribute (the default would be
// `module_path!()::RelayActor`, which silently changes on a module move and
// breaks mixed-build rolling deploys). The derive must stay — a manual
// `impl RemoteActor` skips the linkme `REMOTE_ACTORS` registration the swarm
// uses to validate registry records, silently breaking lookups.
#[derive(Actor, RemoteActor)]
#[remote_actor(id = "waddle.clustering.relay-actor.v1")]
pub struct RelayActor {
    node_id: NodeId,
    /// When false (production), the fault-injection messages are inert acks.
    fault_injection: bool,
}

impl RelayActor {
    pub fn new(node_id: NodeId, fault_injection: bool) -> Self {
        Self {
            node_id,
            fault_injection,
        }
    }
}

/// Liveness probe: which node answers this relay name?
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayPing;

/// Reply to [`RelayPing`].
#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub struct RelayPong {
    pub node_id: NodeId,
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
    pub node_id: NodeId,
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

/// Harness fault injection: crash the relay actor (simulating an unexpected
/// stop, so the supervised respawn + same-name re-registration and the
/// sender-side stale-ref recovery can be asserted cross-node). Inert unless
/// the node runs with `WADDLE_CLUSTERING_FAULT_INJECTION=true`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayCrash;

/// Harness fault injection: hold the relay's mailbox for `millis`
/// (single-threaded per-actor), so a receiver handler exceeding the sender's
/// transport `request_timeout` can be provoked. Inert unless fault injection
/// is enabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelaySleep {
    pub millis: u64,
}

/// Reply to the fault-injection messages: whether the fault was applied
/// (false = fault injection disabled on this node).
#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub struct RelayFaultAck {
    pub applied: bool,
}

#[kameo::remote_message("waddle.clustering.relay.crash.v1")]
impl Message<RelayCrash> for RelayActor {
    type Reply = RelayFaultAck;

    async fn handle(
        &mut self,
        _msg: RelayCrash,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> RelayFaultAck {
        if self.fault_injection {
            // Kill this actor: an unexpected stop from the supervisor's point
            // of view (kameo auto-unregisters, the supervisor respawns and
            // re-registers). The reply may or may not make it out first —
            // harness callers tolerate an error on this ask.
            ctx.actor_ref().kill();
            RelayFaultAck { applied: true }
        } else {
            RelayFaultAck { applied: false }
        }
    }
}

#[kameo::remote_message("waddle.clustering.relay.sleep.v1")]
impl Message<RelaySleep> for RelayActor {
    type Reply = RelayFaultAck;

    async fn handle(
        &mut self,
        msg: RelaySleep,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> RelayFaultAck {
        if self.fault_injection {
            // Clamped so even the harness (or a compromised harness peer)
            // cannot wedge the relay's single-threaded mailbox indefinitely —
            // a wedged-but-live actor has no respawn path.
            let millis = msg.millis.min(MAX_FAULT_SLEEP_MS);
            tokio::time::sleep(Duration::from_millis(millis)).await;
            RelayFaultAck { applied: true }
        } else {
            RelayFaultAck { applied: false }
        }
    }
}

/// Spawn the node's relay actor under supervision: register it in kademlia
/// under [`relay_name`], respawn it if it ever stops unexpectedly, and
/// **re-register under the same name** on every respawn (kameo auto-registers
/// removal on actor stop, so re-registration is mandatory, not optional).
/// Stops cleanly when `stop_token` fires.
pub fn spawn_supervised(node_id: NodeId, fault_injection: bool, stop_token: CancellationToken) {
    tokio::spawn(async move {
        let name = relay_name(&node_id);
        let mut respawns: u64 = 0;
        loop {
            if stop_token.is_cancelled() {
                break;
            }
            let actor_ref: ActorRef<RelayActor> =
                RelayActor::spawn(RelayActor::new(node_id.clone(), fault_injection));
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

            let mut reregister = tokio::time::interval(REREGISTER_INTERVAL);
            // Skip the immediate first tick — we just registered above.
            reregister.tick().await;

            loop {
                tokio::select! {
                    _ = stop_token.cancelled() => {
                        // Graceful stop: stopping the actor triggers kameo's
                        // own (panic-free, fire-and-forget) unregistration.
                        // Deliberately NOT `remote::unregister(..).await`
                        // here: that future panics if the swarm's command
                        // channel is dropped first, which this shutdown
                        // races by construction (the event loop stops on the
                        // same token).
                        let _ = actor_ref.stop_gracefully().await;
                        return;
                    }
                    _ = actor_ref.wait_for_shutdown() => {
                        // Unexpected stop/panic: kameo has already
                        // unregistered the name. Respawn and re-register
                        // under the SAME name after a short backoff.
                        respawns += 1;
                        tokio::time::sleep(RESPAWN_BACKOFF).await;
                        break;
                    }
                    _ = reregister.tick() => {
                        // Same-name refresh so a registration that predated
                        // our first peer connection becomes discoverable
                        // (see REREGISTER_INTERVAL).
                        if let Err(error) = actor_ref.register(name.clone()).await {
                            tracing::debug!(%name, %error, "clustering relay periodic re-registration failed");
                        }
                    }
                }
            }
        }
    });
}

/// A client handle to some node's relay, caching the resolved
/// `RemoteActorRef` and refreshing it via bounded-backoff kademlia re-lookup
/// when the transport reports the cached ref dead
/// (`ActorNotRunning`/`ActorStopped`/`UnknownActor`/`BadActorType` — e.g.
/// after a supervised respawn minted a new `ActorId` under the same name).
///
/// Every ask carries the ADR element-5 receiver-side timeouts
/// (`mailbox_timeout`/`reply_timeout`), defaulting to the compiled
/// [`ClusteringMessagingConfig`](crate::config::ClusteringMessagingConfig)
/// defaults; callers with a runtime config apply it via
/// [`Self::with_ask_timeouts`].
pub struct RelayHandle {
    node_id: NodeId,
    cached: Option<RemoteActorRef<RelayActor>>,
    mailbox_timeout: Duration,
    reply_timeout: Duration,
}

/// Failures asking a remote relay.
#[derive(Debug, thiserror::Error)]
pub enum RelayAskError {
    /// The relay name did not resolve in kademlia within the backoff budget.
    #[error("relay for node '{node_id}' not found in the swarm registry")]
    NotFound { node_id: NodeId },
    /// The ask failed at the transport/handler layer.
    #[error("relay ask failed: {0}")]
    Send(String),
}

impl RelayHandle {
    pub fn new(node_id: NodeId) -> Self {
        let defaults = crate::config::ClusteringMessagingConfig::default();
        Self {
            node_id,
            cached: None,
            mailbox_timeout: defaults.mailbox_timeout,
            reply_timeout: defaults.reply_timeout,
        }
    }

    /// Apply the deployment's configured receiver-side ask timeouts (ADR
    /// element 5: both must sit under the transport `request_timeout`, which
    /// config parsing already validates).
    pub fn with_ask_timeouts(mut self, mailbox_timeout: Duration, reply_timeout: Duration) -> Self {
        self.mailbox_timeout = mailbox_timeout;
        self.reply_timeout = reply_timeout;
        self
    }

    /// Resolve the relay ref, using the cache when warm and a bounded-backoff
    /// kademlia lookup when cold.
    async fn resolve(&mut self) -> Result<RemoteActorRef<RelayActor>, RelayAskError> {
        if let Some(cached) = &self.cached {
            return Ok(cached.clone());
        }
        let name = relay_name(&self.node_id);
        let mut backoffs = LOOKUP_BACKOFF.iter();
        loop {
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
            // Back off between attempts only — no trailing sleep after the
            // final miss.
            match backoffs.next() {
                Some(backoff) => tokio::time::sleep(*backoff).await,
                None => {
                    return Err(RelayAskError::NotFound {
                        node_id: self.node_id.clone(),
                    })
                }
            }
        }
    }

    /// Ping the relay, refreshing a stale cached ref once: transport-layer
    /// errors that mean "this ActorId is gone" invalidate the cache and
    /// re-resolve via kademlia, then retry the ask exactly once.
    pub async fn ping(&mut self) -> Result<RelayPong, RelayAskError> {
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&RelayPing)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
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
                    .mailbox_timeout(self.mailbox_timeout)
                    .reply_timeout(self.reply_timeout)
                    .await
                    .map_err(|error| RelayAskError::Send(error.to_string()))
            }
            Err(error) => Err(RelayAskError::Send(error.to_string())),
        }
    }

    /// Harness: ask the relay to crash (fault injection). The ask may fail —
    /// the actor can die before the reply leaves — so the result only says
    /// whether the *request* went out; callers assert recovery separately.
    pub async fn crash(&mut self) -> Result<(), RelayAskError> {
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&RelayCrash)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
            // A dead-before-reply error is the expected outcome of a crash.
            Ok(_) | Err(_) => Ok(()),
        }
    }

    /// Harness: ask the relay to sleep for `millis` inside its handler,
    /// WITHOUT the stale-ref retry (the point is to observe the sender-side
    /// transport timeout, not to recover from it).
    pub async fn sleep(&mut self, millis: u64) -> Result<RelayFaultAck, RelayAskError> {
        let remote_ref = self.resolve().await?;
        remote_ref
            .ask(&RelaySleep { millis })
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
            .map_err(|error| RelayAskError::Send(error.to_string()))
    }

    /// Round-trip a stanza through the relay (codec proof), with the same
    /// stale-ref refresh-and-retry-once behaviour as [`Self::ping`].
    pub async fn echo_stanza(
        &mut self,
        stanza: RemoteStanza,
    ) -> Result<RelayEchoReply, RelayAskError> {
        let message = RelayEchoStanza { stanza };
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&message)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(error) if is_stale_ref_error(&error) => {
                self.cached = None;
                let remote_ref = self.resolve().await?;
                remote_ref
                    .ask(&message)
                    .mailbox_timeout(self.mailbox_timeout)
                    .reply_timeout(self.reply_timeout)
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
        let (a, b) = (
            NodeId::new("node-1".to_string()),
            NodeId::new("node-2".to_string()),
        );
        assert_eq!(relay_name(&a), "waddle-relay/node-1");
        assert_ne!(relay_name(&a), relay_name(&b));
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
