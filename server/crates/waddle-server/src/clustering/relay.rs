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
//! under the same name**. Sender-side no-effect stale-ref errors
//! (`ActorNotRunning`/`UnknownActor`/`BadActorType`) trigger a bounded-backoff
//! kademlia re-lookup for non-idempotent delivery paths; `ActorStopped` is
//! treated as maybe committed so callers do not duplicate user-visible work.
//!
//! Phase 4 wires DM/MUC/presence routing through this relay. Kademlia still
//! discovers node relays only; entity ownership remains in Postgres claims.

use super::codec::RemoteStanza;
use super::local_claims::RoomLocalClaims;
use super::metrics;
use super::ordered_relay::{
    OrderedRelayMucProxyKind, OrderedRelayNack, OrderedRelayNackReason, OrderedRelayPayload,
    OrderedRelayReceiverState, OrderedRelayReply, OrderedRelayReservation, RemoteStanzaEnvelope,
};
use super::resume_bridge::ResumeStealBridge;
use super::route_bridge::{
    OrderedRelayDeliveryBridge, RemoteResourceOutboundFrame, RemoteResourceRegistrationId,
    RemoteResourceRouteOutcome, RemoteResourceRouteTarget, RemoteResourceSocketGeneration,
    RemoteResourceStateSnapshot, RemoteResourceStateUpdate, RemoteUserSideEffect,
};
use super::self_fence::LocallyClaimedEntities;
use super::trace_context::RelayTraceContext;
use super::NodeId;
use kameo::actor::{ActorRef, RemoteActorRef, Spawn};
use kameo::error::RemoteSendError;
use kameo::message::{Context, Message};
use kameo::{Actor, RemoteActor, Reply};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use waddle_xmpp::ownership::{ClaimEpoch, Entity};

/// Bound on this node's own wait for a local force-detach to complete when
/// answering a [`RelayResumeSteal`] ask (ADR-0017 Phase 3 Slice 6).
/// Independent of the *asking* node's own resume-handshake retry budget
/// (`ClusteringResumeHandshakeConfig::timeout`) — this is purely a
/// defensive bound so a wedged local connection cannot leak this handler's
/// task indefinitely; a timeout here answers `NotLiveLocally` via
/// [`super::resume_bridge::ResumeStealBridge::request_forced_detach`]'s own
/// bounded wait, which the asker retries exactly like any other transient
/// race.
const LOCAL_FORCE_DETACH_ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// The kademlia registration name for a node's relay actor — the node's ONLY
/// kademlia name (O(1) registrations per node, never per entity).
pub fn relay_name(node_id: &NodeId) -> String {
    format!("waddle-relay/{node_id}")
}

/// Named root span for one inbound relay dispatch (#1483).
///
/// Every substantive `#[kameo::remote_message]` handler on [`RelayActor`]
/// runs with no active local span (the receive path has no upstream
/// instrumentation), so the kameo `actor.handle_message` spans its work
/// mints would root the trace — exactly the shape the #1438 span-noise
/// sampler drops. Opening this dedicated root before any actor message is
/// sent keeps the receiving node's half of the delivery traceable: the
/// actor spans become parented children and survive the sampler. The
/// sending node's half joins this one whenever the message carried a
/// [`RelayTraceContext`] (#1485): the extracted **remote** parent is
/// applied here, so a cross-node delivery is a single trace.
///
/// `otel.kind = "consumer"`: this is the receive side of a cross-node
/// message, per OTel messaging semantics.
///
/// `parent: None` is load-bearing and stays the no-context fallback: the
/// handler itself executes inside kameo's own (suppressed) root
/// `actor.handle_message` span, and a child of a locally-unsampled parent
/// is dropped by the sampler too — with no propagated context this span
/// must start a fresh root trace. A propagated parent is safe because it
/// is *remote*, a case the sampler leaves to the delegate sampler.
///
/// Re-parenting must happen here, before the span is entered or cloned:
/// `tracing-opentelemetry` refuses to move a span whose builder was
/// already consumed.
///
/// The span name is documented in `telemetry::span_noise` and must never
/// be added to its suppression lists.
fn relay_dispatch_span(message: &'static str, trace: &RelayTraceContext) -> tracing::Span {
    let span = tracing::info_span!(
        parent: None,
        "clustering.relay.dispatch",
        otel.kind = "consumer",
        relay.message = message,
        jid = tracing::field::Empty,
        stream_id = tracing::field::Empty,
        channel = tracing::field::Empty,
        sequence = tracing::field::Empty,
        origin_node = tracing::field::Empty,
        entity = tracing::field::Empty,
    );
    trace.parent_span(&span);
    span
}

/// The single seam through which every delegated relay reply task is
/// spawned (#1483): binding `ctx.spawn` to the dispatch span here means
/// a handler cannot delegate work outside its root span without
/// bypassing this helper — and a test pins that no handler does.
fn spawn_in_dispatch_span<R, F>(
    ctx: &mut Context<RelayActor, R>,
    span: tracing::Span,
    future: F,
) -> kameo::reply::DelegatedReply<R::Value>
where
    R: Reply + ?Sized,
    F: std::future::Future<Output = R::Value> + Send + 'static,
{
    ctx.spawn(future.instrument(span))
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

/// Lightweight signal the swarm event loop uses to make a relay registration
/// visible immediately after a peer connection forms. The periodic refresh
/// remains the fallback; this just removes the startup discovery window.
#[derive(Clone)]
pub struct RelayRegistrationTrigger {
    tx: mpsc::Sender<()>,
}

impl RelayRegistrationTrigger {
    pub fn trigger(&self) {
        let _ = self.tx.try_send(());
    }
}

enum RelayRegisterAttempt {
    Registered,
    Cancelled,
    Failed(String),
}

async fn register_relay_actor(
    actor_ref: &ActorRef<RelayActor>,
    name: &str,
    stop_token: &CancellationToken,
) -> RelayRegisterAttempt {
    // Race registration against cancellation instead of plain `.await`:
    // kameo's swarm-command reply future panics (`.expect(..)` on a dropped
    // oneshot sender) if the event loop observes this SAME stop token, drops
    // `Swarm<WaddleBehaviour>`, and the pending register command's reply is
    // abandoned as a result. `biased` is load-bearing: the event loop drops
    // the swarm only AFTER observing this token, so polling the cancellation
    // arm first guarantees `register` is never polled against an already-closed
    // swarm command channel.
    match tokio::select! {
        biased;
        _ = stop_token.cancelled() => return RelayRegisterAttempt::Cancelled,
        result = actor_ref.register(name.to_string()) => result,
    } {
        Ok(()) => RelayRegisterAttempt::Registered,
        Err(error) => RelayRegisterAttempt::Failed(error.to_string()),
    }
}

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
    /// ADR-0017 Phase 3 Slice 6: this node's bridge to its own live
    /// `ConnectionRegistry`, answering [`RelayResumeSteal`] asks.
    resume_bridge: Arc<ResumeStealBridge>,
    /// ADR-0017 Phase 3 Slice 7: this node's `RoomActor` claims, answering
    /// [`Demote`] asks — the two-part demotion protocol's part (a)
    /// receiving side.
    room_local_claims: Arc<RoomLocalClaims>,
    /// ADR-0017 Phase 4 Slice 3: bridge to local full-JID delivery services.
    ordered_delivery_bridge: Arc<OrderedRelayDeliveryBridge>,
    /// ADR-0017 Phase 4 Slice 2: internal ordered-relay receiver state. This
    /// validates sequence ACK/NACK behavior only; no production delivery actor
    /// is called from this substrate.
    ordered_receiver: Arc<Mutex<OrderedRelayReceiverState>>,
}

impl RelayActor {
    pub fn new(
        node_id: NodeId,
        fault_injection: bool,
        resume_bridge: Arc<ResumeStealBridge>,
        room_local_claims: Arc<RoomLocalClaims>,
        ordered_delivery_bridge: Arc<OrderedRelayDeliveryBridge>,
    ) -> Self {
        Self {
            node_id,
            fault_injection,
            resume_bridge,
            room_local_claims,
            ordered_delivery_bridge,
            ordered_receiver: Arc::new(Mutex::new(OrderedRelayReceiverState::default())),
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

/// Ordered relay substrate ask: validate one sequenced typed stanza envelope
/// and return an internal ACK/NACK. This deliberately does not call any
/// production delivery actor or mutate client-visible XEP state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayDeliverOrdered {
    pub envelope: RemoteStanzaEnvelope,
    /// Sender's W3C trace context (#1485), telemetry only: absent from
    /// an older node's encoding, defaulted on decode, and never read for
    /// relay semantics. Stamped at the send seam by [`RelayHandle`].
    #[serde(default)]
    pub trace: RelayTraceContext,
}

async fn finish_ordered_reservation(
    receiver: Arc<Mutex<OrderedRelayReceiverState>>,
    delivery_bridge: Arc<OrderedRelayDeliveryBridge>,
    reservation: OrderedRelayReservation,
) -> OrderedRelayReply {
    match reservation {
        OrderedRelayReservation::Reserved(reserved) => {
            let envelope = reserved.envelope().clone();
            let delivery_timeout = delivery_bridge.reserved_delivery_effect_timeout();
            match tokio::time::timeout(
                delivery_timeout,
                delivery_bridge.deliver_reserved(&envelope),
            )
            .await
            {
                Ok(Ok(client_replies)) => receiver
                    .lock()
                    .await
                    .commit_reserved_with_replies(*reserved, client_replies),
                Ok(Err(reason)) => receiver.lock().await.abort_reserved(*reserved, reason),
                Err(_) => {
                    tracing::warn!(
                        timeout_ms = delivery_timeout.as_millis(),
                        channel = ?envelope.channel,
                        sequence = envelope.sequence.0,
                        "ordered relay: reserved receiver delivery effect timed out"
                    );
                    if is_idempotent_join_presence_envelope(&envelope) {
                        receiver.lock().await.abort_reserved_without_diversion(
                            *reserved,
                            OrderedRelayNackReason::MaybeCommitted,
                        )
                    } else {
                        receiver
                            .lock()
                            .await
                            .abort_reserved(*reserved, OrderedRelayNackReason::MaybeCommitted)
                    }
                }
            }
        }
        OrderedRelayReservation::Completed(reply) => reply,
    }
}

fn is_idempotent_join_presence_envelope(envelope: &RemoteStanzaEnvelope) -> bool {
    matches!(
        &envelope.payload,
        OrderedRelayPayload::MucProxy {
            kind: OrderedRelayMucProxyKind::JoinPresence,
            ..
        }
    )
}

// #1597 HARD RULE: bump this version suffix on EVERY change to the
// ordered-relay envelope wire format (`RemoteStanzaEnvelope` or
// anything it contains — payload variants, recipient/lane shapes,
// claim fields). A peer running an older build then fails the ask
// with `UnknownMessage` — provably before its handler ran — which the
// sender maps to an `UnsupportedEnvelope` NACK: sequence rolled back,
// channel kept, no sticky diversion. Reusing an old id instead makes
// the old peer fail mid-decode with `DeserializeMessage`, which is
// indistinguishable from a reply-decode failure after a committed
// effect and therefore poisons the channel for the mixed-version
// window. The version bump is what makes rolling deploys safe.
// v2: room-lane recipient split + UnsupportedEnvelope NACK (#1597).
#[kameo::remote_message("waddle.clustering.relay.deliver_ordered.v2")]
impl Message<RelayDeliverOrdered> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<OrderedRelayReply>;

    async fn handle(
        &mut self,
        msg: RelayDeliverOrdered,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span("deliver_ordered", &msg.trace);
        span.record("channel", tracing::field::debug(&msg.envelope.channel));
        span.record("sequence", msg.envelope.sequence.0);
        span.record(
            "origin_node",
            tracing::field::display(&msg.envelope.asserted_origin_node),
        );
        let receiver = Arc::clone(&self.ordered_receiver);
        let delivery_bridge = Arc::clone(&self.ordered_delivery_bridge);
        // The reservation must stay inline (mailbox-ordered); only the
        // delegated delivery moves onto the instrumented reply task.
        let reservation = receiver.lock().await.reserve(msg.envelope);
        spawn_in_dispatch_span(ctx, span, async move {
            let reply = finish_ordered_reservation(receiver, delivery_bridge, reservation).await;
            record_ordered_relay_reply(&reply);
            reply
        })
    }
}

fn record_ordered_relay_reply(reply: &OrderedRelayReply) {
    match reply {
        OrderedRelayReply::Ack(_) => metrics::record_ordered_relay_ack(),
        OrderedRelayReply::Nack(nack) => {
            metrics::record_ordered_relay_nack(nack.reason.metric_label());
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayRegisterRemoteUserResource {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub socket_generation: RemoteResourceSocketGeneration,
    pub socket_node: NodeId,
    pub state: RemoteResourceStateSnapshot,
    /// Sender's W3C trace context (#1485), telemetry only: absent from
    /// an older node's encoding, defaulted on decode, and never read for
    /// relay semantics. Stamped at the send seam by [`RelayHandle`].
    #[serde(default)]
    pub trace: RelayTraceContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayRemoteResourceRegistrationStatus {
    Registered,
    StaleRegistration,
    NotOwner,
    /// The authoritative owner's child actor is processing a short-lived
    /// operation.  The socket node may retry this bounded transient.
    Busy,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub struct RelayRemoteResourceRegistrationReply {
    pub status: RelayRemoteResourceRegistrationStatus,
}

#[kameo::remote_message("waddle.clustering.relay.remote_resource_register.v2")]
impl Message<RelayRegisterRemoteUserResource> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<RelayRemoteResourceRegistrationReply>;

    async fn handle(
        &mut self,
        msg: RelayRegisterRemoteUserResource,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span("remote_resource_register", &msg.trace);
        span.record("jid", tracing::field::display(&msg.jid));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            bridge.register_remote_user_resource_on_owner(msg).await
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayUnregisterRemoteUserResource {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub socket_generation: RemoteResourceSocketGeneration,
    /// Sender's W3C trace context (#1485), telemetry only: absent from
    /// an older node's encoding, defaulted on decode, and never read for
    /// relay semantics. Stamped at the send seam by [`RelayHandle`].
    #[serde(default)]
    pub trace: RelayTraceContext,
}

#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub struct RelayRemoteResourceUnregisterReply {
    pub status: RelayRemoteResourceUnregisterStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayRemoteResourceUnregisterStatus {
    Unregistered,
    RecordedRetry,
    NotRegistered,
    Failed,
}

#[kameo::remote_message("waddle.clustering.relay.remote_resource_unregister.v2")]
impl Message<RelayUnregisterRemoteUserResource> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<RelayRemoteResourceUnregisterReply>;

    async fn handle(
        &mut self,
        msg: RelayUnregisterRemoteUserResource,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span("remote_resource_unregister", &msg.trace);
        span.record("jid", tracing::field::display(&msg.jid));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            bridge.unregister_remote_user_resource_on_owner(msg).await
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayUpdateRemoteUserResource {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub socket_generation: RemoteResourceSocketGeneration,
    pub update: RemoteResourceStateUpdate,
    /// Sender's W3C trace context (#1485), telemetry only: absent from
    /// an older node's encoding, defaulted on decode, and never read for
    /// relay semantics. Stamped at the send seam by [`RelayHandle`].
    #[serde(default)]
    pub trace: RelayTraceContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayRemoteResourceUpdateStatus {
    Updated,
    StaleRegistration,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub struct RelayRemoteResourceUpdateReply {
    pub status: RelayRemoteResourceUpdateStatus,
}

#[kameo::remote_message("waddle.clustering.relay.remote_resource_update.v1")]
impl Message<RelayUpdateRemoteUserResource> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<RelayRemoteResourceUpdateReply>;

    async fn handle(
        &mut self,
        msg: RelayUpdateRemoteUserResource,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span("remote_resource_update", &msg.trace);
        span.record("jid", tracing::field::display(&msg.jid));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            bridge.update_remote_user_resource_on_owner(msg).await
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayRemoteUserSideEffect {
    pub source_jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub socket_generation: RemoteResourceSocketGeneration,
    pub effect: RemoteUserSideEffect,
    /// Sender's W3C trace context (#1485), telemetry only: absent from
    /// an older node's encoding, defaulted on decode, and never read for
    /// relay semantics. Stamped at the send seam by [`RelayHandle`].
    #[serde(default)]
    pub trace: RelayTraceContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayRemoteUserSideEffectStatus {
    Applied,
    StaleRegistration,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub struct RelayRemoteUserSideEffectReply {
    pub status: RelayRemoteUserSideEffectStatus,
    /// Frozen XEP-0280 targets, returned to the ingress node for shadow
    /// capture after the remote owner performs its authoritative enumeration.
    #[serde(default)]
    pub carbon_recipients: Vec<jid::FullJid>,
}

#[kameo::remote_message("waddle.clustering.relay.remote_user_side_effect.v1")]
impl Message<RelayRemoteUserSideEffect> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<RelayRemoteUserSideEffectReply>;

    async fn handle(
        &mut self,
        msg: RelayRemoteUserSideEffect,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span("remote_user_side_effect", &msg.trace);
        span.record("jid", tracing::field::display(&msg.source_jid));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            bridge.apply_remote_user_side_effect_on_owner(msg).await
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayRouteRemoteResourceStanza {
    pub source_jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub socket_generation: RemoteResourceSocketGeneration,
    pub target: RemoteResourceRouteTarget,
    /// Sender's W3C trace context (#1485), telemetry only: absent from
    /// an older node's encoding, defaulted on decode, and never read for
    /// relay semantics. Stamped at the send seam by [`RelayHandle`].
    #[serde(default)]
    pub trace: RelayTraceContext,
}

#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub struct RelayRouteRemoteResourceStanzaReply {
    pub outcome: RemoteResourceRouteOutcome,
    pub replies: Vec<RemoteStanza>,
}

#[kameo::remote_message("waddle.clustering.relay.remote_resource_route.v1")]
impl Message<RelayRouteRemoteResourceStanza> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<RelayRouteRemoteResourceStanzaReply>;

    async fn handle(
        &mut self,
        msg: RelayRouteRemoteResourceStanza,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span("remote_resource_route", &msg.trace);
        span.record("jid", tracing::field::display(&msg.source_jid));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            bridge.route_remote_resource_stanza_on_owner(msg).await
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayDeliverRemoteResourceFrame {
    pub frame: RemoteResourceOutboundFrame,
    /// Sender's W3C trace context (#1485), telemetry only: absent from
    /// an older node's encoding, defaulted on decode, and never read for
    /// relay semantics. Stamped at the send seam by [`RelayHandle`].
    #[serde(default)]
    pub trace: RelayTraceContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayRemoteResourceFrameStatus {
    Delivered,
    Backpressure,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub struct RelayRemoteResourceFrameReply {
    pub status: RelayRemoteResourceFrameStatus,
}

#[kameo::remote_message("waddle.clustering.relay.remote_resource_frame.v1")]
impl Message<RelayDeliverRemoteResourceFrame> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<RelayRemoteResourceFrameReply>;

    async fn handle(
        &mut self,
        msg: RelayDeliverRemoteResourceFrame,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span("remote_resource_frame", &msg.trace);
        span.record("jid", tracing::field::display(&msg.frame.jid));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            bridge.deliver_remote_resource_frame_on_socket(msg).await
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayForceDetachRemoteUserResource {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub origin: waddle_xmpp::registry::ForceDetachOrigin,
    pub requester_bare_jid: jid::BareJid,
    /// Sender's W3C trace context (#1485), telemetry only: absent from
    /// an older node's encoding, defaulted on decode, and never read for
    /// relay semantics. Stamped at the send seam by [`RelayHandle`].
    #[serde(default)]
    pub trace: RelayTraceContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayRemoteResourceForceDetachStatus {
    Detached,
    NotLive,
    Refused,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Reply)]
pub struct RelayForceDetachRemoteUserResourceReply {
    pub outcome: waddle_xmpp::registry::ForceDetachOutcome,
    pub status: RelayRemoteResourceForceDetachStatus,
}

#[kameo::remote_message("waddle.clustering.relay.remote_resource_force_detach.v2")]
impl Message<RelayForceDetachRemoteUserResource> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<RelayForceDetachRemoteUserResourceReply>;

    async fn handle(
        &mut self,
        msg: RelayForceDetachRemoteUserResource,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span("remote_resource_force_detach", &msg.trace);
        span.record("jid", tracing::field::display(&msg.jid));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            bridge
                .force_detach_remote_user_resource_on_socket(msg)
                .await
        })
    }
}

/// Cross-node XEP-0198 resume live-steal handshake ask (ADR-0017 Phase 3
/// Slice 6, element 8's "live, owned elsewhere" branch). Sent by the
/// resuming node to the node currently claiming `stream_id`, asking it to
/// force-detach (identity-checked defense in depth) so a persisted snapshot
/// becomes readable and the resuming node can proceed with
/// `steal_for_resume`. This is Slice 6's entire addition to the relay
/// message set — every other message on this actor predates it (Phase 2,
/// discovery-only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayResumeSteal {
    pub stream_id: waddle_xmpp::pending_delivery::SmSessionId,
    pub requester_bare_jid: jid::BareJid,
    /// Sender's W3C trace context (#1485), telemetry only: absent from
    /// an older node's encoding, defaulted on decode, and never read for
    /// relay semantics. Stamped at the send seam by [`RelayHandle`].
    #[serde(default)]
    pub trace: RelayTraceContext,
}

/// Reply to [`RelayResumeSteal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Reply)]
pub enum RelayResumeStealReply {
    /// Identity matched: this node's own live connection (if any) for
    /// `stream_id` force-detached, sent `<conflict/>`, and closed — a
    /// persisted snapshot should now be readable.
    Detached,
    /// This node's own defense-in-depth identity check rejected the
    /// requester — the asker must not proceed to `steal_for_resume`.
    IdentityMismatch,
    /// This node has no live local connection for `stream_id` right now (a
    /// race with a concurrent detach/expiry, or the connection did not
    /// answer within this node's own bounded wait) — the asker should
    /// re-check persistence and retry.
    NotLiveLocally,
}

#[kameo::remote_message("waddle.clustering.relay.resume_steal.v1")]
impl Message<RelayResumeSteal> for RelayActor {
    // Council-adjudicated FIX 2: `DelegatedReply`, not a bare
    // `RelayResumeStealReply`. `resume_bridge.request_forced_detach` awaits
    // the local connection's own force-detach ack, up to
    // `LOCAL_FORCE_DETACH_ACK_TIMEOUT` (10s) — kameo actors process their
    // mailbox strictly sequentially, so awaiting that inline here would
    // head-of-line-block every OTHER message this node's relay answers
    // (`RelayPing`, `RelayEchoStanza`, concurrent `RelayResumeSteal` asks
    // for unrelated sessions, ...) for up to 10 seconds per ask. kameo 0.20
    // ships exactly the mechanism this needs:
    // `Context::spawn` (`ctx.spawn(future)`) delegates the reply to a
    // detached `tokio::spawn`ed task and returns immediately, freeing the
    // mailbox for the next message while the force-detach wait proceeds
    // concurrently. This is the intended, documented kameo 0.20 pattern
    // (see `kameo::message::Context::{reply_sender, spawn}`'s doc comments
    // and examples) — no second actor/registration is needed, so the
    // O(1)-kademlia-registrations-per-node claim (this module's own doc
    // comment) is unaffected.
    type Reply = kameo::reply::DelegatedReply<RelayResumeStealReply>;

    async fn handle(
        &mut self,
        msg: RelayResumeSteal,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span("resume_steal", &msg.trace);
        span.record("stream_id", tracing::field::display(&msg.stream_id));
        span.record("jid", tracing::field::display(&msg.requester_bare_jid));
        let resume_bridge = Arc::clone(&self.resume_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            match resume_bridge
                .request_forced_detach(
                    &msg.stream_id,
                    &msg.requester_bare_jid,
                    LOCAL_FORCE_DETACH_ACK_TIMEOUT,
                )
                .await
            {
                super::resume_bridge::LocalForcedDetachOutcome::Detached => {
                    RelayResumeStealReply::Detached
                }
                super::resume_bridge::LocalForcedDetachOutcome::IdentityMismatch => {
                    RelayResumeStealReply::IdentityMismatch
                }
                super::resume_bridge::LocalForcedDetachOutcome::NotLiveLocally => {
                    RelayResumeStealReply::NotLiveLocally
                }
            }
        })
    }
}

/// Two-part demotion protocol, part (a) (ADR-0017 Phase 3 Slice 7, element
/// 7): best-effort, acked notification sent by the node that just won a
/// steal CAS to the node it stole `entity` from. `new_epoch` is carried
/// for observability/logging only — the recipient does not need it to act
/// (a hard local demote is unconditional and idempotent), but it lets a
/// receiving node's log line show which epoch superseded its own.
///
/// Deliberately narrow, mirroring [`RelayResumeSteal`]'s exact shape: a
/// small, wire-bounded, fully typed payload (an [`Entity`]/[`ClaimEpoch`],
/// never raw strings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Demote {
    pub entity: Entity,
    pub new_epoch: ClaimEpoch,
    /// Sender's W3C trace context (#1485), telemetry only: absent from
    /// an older node's encoding, defaulted on decode, and never read for
    /// relay semantics. Stamped at the send seam by [`RelayHandle`].
    #[serde(default)]
    pub trace: RelayTraceContext,
}

/// Reply to [`Demote`]: always `Acked` — the recipient's local demote
/// ([`super::self_fence::LocallyClaimedEntities::demote`]) is
/// unconditional, idempotent, and infallible by contract (best-effort,
/// must succeed even against a wedged actor). There is nothing for the
/// asker to retry on, unlike [`RelayResumeStealReply`]'s multi-outcome
/// shape — the guaranteed correctness backstop is the fenced pre-fan-out
/// check, not this ask's outcome, so a richer reply would invite a caller
/// to (wrongly) treat this ask as load-bearing for correctness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Reply)]
pub enum DemoteReply {
    Acked,
}

#[kameo::remote_message("waddle.clustering.relay.demote.v1")]
impl Message<Demote> for RelayActor {
    type Reply = DemoteReply;

    async fn handle(&mut self, msg: Demote, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        let span = relay_dispatch_span("demote", &msg.trace);
        span.record("entity", tracing::field::debug(&msg.entity));
        async {
            self.room_local_claims.demote(&msg.entity).await;
            DemoteReply::Acked
        }
        .instrument(span)
        .await
    }
}

/// #1594: server-originated request to re-assert one live SFU
/// participant's media grants on the node holding `room`'s claim.
///
/// Sent by a node whose LiveKit `participant_joined` webhook found no
/// local room actor (the webhook URL is load-balanced, so with more
/// than one replica most joins land on a non-owning node). The
/// receiver re-derives the occupant's XEP-0045 voice from its OWN room
/// actor and pushes the grants — converging a token minted before a
/// voice change in milliseconds instead of the reconciler's next tick.
///
/// Mirrors [`Demote`]'s shape: a small, fully typed payload discovered
/// via the claim store and asked directly on the owner's relay — not
/// the ordered MUC proxy, which requires a client-stanza origin a
/// webhook does not have (and whose ordering an idempotent re-assert
/// does not need).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayReassertMediaGrants {
    pub room: jid::BareJid,
    pub participant: jid::FullJid,
    /// Sender's W3C trace context (#1485), telemetry only: absent from
    /// an older node's encoding, defaulted on decode, and never read for
    /// relay semantics. Stamped at the send seam by [`RelayHandle`].
    #[serde(default)]
    pub trace: RelayTraceContext,
}

/// Reply to [`RelayReassertMediaGrants`]. Only the first two variants
/// are authoritative; the last two mean "could not determine", which
/// the asker maps to a LiveKit webhook retry — never to an
/// authorization decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Reply)]
pub enum RelayReassertMediaGrantsReply {
    /// The owner's room actor answered with the occupant's voice and
    /// the grants were pushed to the SFU.
    Applied,
    /// The owner's room actor answered authoritatively that the
    /// participant is not an occupant; they were evicted from the call.
    NotOccupantEvicted,
    /// The receiver holds no room actor for `room` — the asker's claim
    /// read was stale or the actor is mid-(re)spawn. Terminal on the
    /// receiver (never re-relayed) so a stale claim cannot bounce a
    /// webhook around the cluster.
    NotOwner,
    /// The receiver could not execute the re-assert (services or state
    /// unavailable, local lookup flaked).
    Unavailable,
}

#[kameo::remote_message("waddle.clustering.relay.reassert_media_grants.v1")]
impl Message<RelayReassertMediaGrants> for RelayActor {
    // Delegated for the same reason as [`RelayResumeSteal`]: the
    // owner-side room-actor ask is bounded but must not head-of-line
    // block this node's relay mailbox while it resolves.
    type Reply = kameo::reply::DelegatedReply<RelayReassertMediaGrantsReply>;

    async fn handle(
        &mut self,
        msg: RelayReassertMediaGrants,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span("reassert_media_grants", &msg.trace);
        span.record("entity", tracing::field::display(&msg.room));
        span.record("jid", tracing::field::display(msg.participant.to_bare()));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            use super::route_bridge::LocalMediaGrantReassertion;
            match bridge
                .reassert_media_grants_local(&msg.room, &msg.participant)
                .await
            {
                LocalMediaGrantReassertion::Applied => RelayReassertMediaGrantsReply::Applied,
                LocalMediaGrantReassertion::EvictedNonOccupant => {
                    RelayReassertMediaGrantsReply::NotOccupantEvicted
                }
                LocalMediaGrantReassertion::NoLocalRoomActor => {
                    RelayReassertMediaGrantsReply::NotOwner
                }
                LocalMediaGrantReassertion::Unavailable => {
                    RelayReassertMediaGrantsReply::Unavailable
                }
            }
        })
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
pub fn spawn_supervised(
    node_id: NodeId,
    fault_injection: bool,
    stop_token: CancellationToken,
    resume_bridge: Arc<ResumeStealBridge>,
    room_local_claims: Arc<RoomLocalClaims>,
    ordered_delivery_bridge: Arc<OrderedRelayDeliveryBridge>,
) -> RelayRegistrationTrigger {
    let (trigger_tx, mut trigger_rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let name = relay_name(&node_id);
        let mut respawns: u64 = 0;
        let mut trigger_closed = false;
        loop {
            if stop_token.is_cancelled() {
                break;
            }
            let actor_ref: ActorRef<RelayActor> = RelayActor::spawn(RelayActor::new(
                node_id.clone(),
                fault_injection,
                Arc::clone(&resume_bridge),
                Arc::clone(&room_local_claims),
                Arc::clone(&ordered_delivery_bridge),
            ));
            match register_relay_actor(&actor_ref, &name, &stop_token).await {
                RelayRegisterAttempt::Registered => {
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
                RelayRegisterAttempt::Cancelled => {
                    actor_ref.kill();
                    return;
                }
                RelayRegisterAttempt::Failed(error) => {
                    tracing::warn!(%name, %error, "clustering relay registration failed; retrying");
                    actor_ref.kill();
                    // Cancellation-aware backoff: graceful shutdown must not
                    // wait out the retry delay.
                    tokio::select! {
                        _ = stop_token.cancelled() => return,
                        _ = tokio::time::sleep(RESPAWN_BACKOFF) => {}
                    }
                    continue;
                }
            }

            let mut reregister = tokio::time::interval(REREGISTER_INTERVAL);
            // Delayed ticks must not burst-catch-up (re-registration is
            // idempotent but each burst tick is a wasted DHT write) — same
            // policy as the swarm event-loop timers.
            reregister.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
                        // under the SAME name after a short backoff (which a
                        // graceful shutdown must not wait out).
                        respawns += 1;
                        tokio::select! {
                            _ = stop_token.cancelled() => return,
                            _ = tokio::time::sleep(RESPAWN_BACKOFF) => {}
                        }
                        break;
                    }
                    _ = reregister.tick() => {
                        // Same-name refresh so a registration that predated
                        // our first peer connection becomes discoverable
                        // (see REREGISTER_INTERVAL). Guarded against the same
                        // cancellation race as the post-(re)spawn
                        // registration above: the `biased` inner `select!`
                        // polls the cancellation arm first, so `register` is
                        // never polled once cancellation is visible — and the
                        // event loop drops the swarm only after observing the
                        // same token, so a closed swarm command channel can
                        // never panic here (`is_cancelled()` merely
                        // short-circuits the common case). Falling through to
                        // the outer loop after a lost race is sufficient: the
                        // next iteration's `stop_token.cancelled()` arm fires
                        // immediately.
                        if !stop_token.is_cancelled() {
                            match register_relay_actor(&actor_ref, &name, &stop_token).await {
                                RelayRegisterAttempt::Registered => {}
                                RelayRegisterAttempt::Cancelled => return,
                                RelayRegisterAttempt::Failed(error) => {
                                    tracing::debug!(%name, %error, "clustering relay periodic re-registration failed");
                                }
                            }
                        }
                    }
                    trigger = trigger_rx.recv(), if !trigger_closed => {
                        match trigger {
                            Some(()) => {
                                match register_relay_actor(&actor_ref, &name, &stop_token).await {
                                    RelayRegisterAttempt::Registered => {
                                        tracing::debug!(%name, "clustering relay re-registered after peer connection");
                                    }
                                    RelayRegisterAttempt::Cancelled => return,
                                    RelayRegisterAttempt::Failed(error) => {
                                        tracing::debug!(%name, %error, "clustering relay peer-triggered re-registration failed");
                                    }
                                }
                            }
                            None => {
                                trigger_closed = true;
                            }
                        }
                    }
                }
            }
        }
    });
    RelayRegistrationTrigger { tx: trigger_tx }
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
///
/// `resolve`/`ping`/`crash`/`sleep`/`echo_stanza`/`resume_steal` all await
/// kameo swarm-command replies and share the same theoretical panic-on-drop
/// hazard as `spawn_supervised`'s `register` calls (see there) if awaited
/// concurrently with the local swarm being torn down.
///
/// **Cancellation-safety paydown (ADR-0017 Phase 3 Slice 6, coordinator
/// ruling — paid down in this slice rather than carried into Phase 4)**:
/// `RelayHandle` now owns a `stop_token: CancellationToken` (the same
/// clustering-scope token `spawn_supervised` already receives), and every
/// public ask method races its whole body (resolve + send + await-reply)
/// against that token in a `biased` `select!`, exactly mirroring
/// `spawn_supervised`'s pattern: cancellation is polled first, so an ask
/// already in flight during local clustering shutdown returns
/// [`RelayAskError::Cancelled`] instead of ever polling a future against an
/// already-torn-down swarm. This slice is `RelayHandle`'s first production
/// (non-harness) caller — the cross-node XEP-0198 resume live-steal
/// handshake ([`RelayHandle::resume_steal`]) — which is exactly the
/// "a caller awaits these methods from a task that also watches this node's
/// clustering stop token" case the previous doc comment named as the
/// trigger for paying this down.
/// **Trace propagation (#1485)**: every ask method stamps the sending
/// task's active span context onto its message
/// ([`RelayTraceContext::capture`]) immediately before the send, so the
/// receiving node's `clustering.relay.dispatch` root joins the sender's
/// trace. This is the single seam — construction sites leave the field at
/// its `Default` (no context) — and it is telemetry only: a message whose
/// context is absent or unparsable is relayed exactly as before.
pub struct RelayHandle {
    node_id: NodeId,
    cached: Option<RemoteActorRef<RelayActor>>,
    mailbox_timeout: Duration,
    reply_timeout: Duration,
    stop_token: CancellationToken,
}

/// Failures asking a remote relay.
#[derive(Debug, thiserror::Error)]
pub enum RelayAskError {
    /// The relay name did not resolve in kademlia within the backoff budget.
    #[error("relay for node '{node_id}' not found in the swarm registry")]
    NotFound { node_id: NodeId },
    /// The ask failed at the transport/handler layer. `failure` is the typed,
    /// matchable classification; `message` is the rendered kameo error, kept
    /// for human-facing diagnostics only.
    #[error("relay ask failed ({failure:?}): {message}")]
    Send {
        failure: RelaySendFailure,
        effect: RelaySendEffect,
        message: String,
    },
    /// This handle's clustering-scope stop token fired before the ask
    /// completed (ADR-0017 Phase 3 Slice 6 cancellation-safety paydown).
    #[error("relay ask cancelled: clustering shutdown in progress")]
    Cancelled,
}

/// Typed classification of a failed relay ask. kameo's [`RemoteSendError`]
/// is generic over each message's handler-error type, so this enum is the
/// stable shape callers can match on (typed-payloads rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaySendFailure {
    /// The cached `ActorId` is dead or wrong — the stale-ref re-lookup set.
    StaleRef,
    /// The receiver's mailbox refused the message (full / enqueue timeout).
    MailboxFull,
    /// The reply budget elapsed before the handler answered.
    ReplyTimeout,
    /// The handler itself returned an error.
    Handler,
    /// Message/reply (de)serialization failed at either end, or the peer
    /// does not know this message type (protocol mismatch).
    Codec,
    /// Transport-level failure: dialing, network timeout, closed connection,
    /// unsupported protocols, IO, or an unbootstrapped swarm.
    Transport,
}

/// Whether a failed remote ask could have reached the handler far enough to
/// perform its durable/local effect before the sender observed the failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaySendEffect {
    /// The failure happened before the handler could run.
    NoEffect,
    /// The handler may have run; retry/fallback may duplicate user-visible
    /// effects.
    MaybeCommitted,
}

fn classify<E>(error: &RemoteSendError<E>) -> RelaySendFailure {
    match error {
        RemoteSendError::ActorNotRunning
        | RemoteSendError::ActorStopped
        | RemoteSendError::UnknownActor { .. }
        | RemoteSendError::BadActorType => RelaySendFailure::StaleRef,
        RemoteSendError::MailboxFull => RelaySendFailure::MailboxFull,
        RemoteSendError::ReplyTimeout => RelaySendFailure::ReplyTimeout,
        RemoteSendError::HandlerError(_) => RelaySendFailure::Handler,
        RemoteSendError::UnknownMessage { .. }
        | RemoteSendError::SerializeMessage(_)
        | RemoteSendError::DeserializeMessage(_)
        | RemoteSendError::SerializeReply(_)
        | RemoteSendError::SerializeHandlerError(_)
        | RemoteSendError::DeserializeHandlerError(_) => RelaySendFailure::Codec,
        RemoteSendError::SwarmNotBootstrapped
        | RemoteSendError::DialFailure
        | RemoteSendError::NetworkTimeout
        | RemoteSendError::ConnectionClosed
        | RemoteSendError::UnsupportedProtocols
        | RemoteSendError::Io(_) => RelaySendFailure::Transport,
    }
}

fn classify_effect<E>(error: &RemoteSendError<E>) -> RelaySendEffect {
    match error {
        RemoteSendError::ActorNotRunning
        | RemoteSendError::UnknownActor { .. }
        | RemoteSendError::UnknownMessage { .. }
        | RemoteSendError::BadActorType
        | RemoteSendError::MailboxFull
        | RemoteSendError::SerializeMessage(_)
        | RemoteSendError::SwarmNotBootstrapped
        | RemoteSendError::DialFailure
        | RemoteSendError::UnsupportedProtocols => RelaySendEffect::NoEffect,
        RemoteSendError::ActorStopped
        | RemoteSendError::ReplyTimeout
        | RemoteSendError::HandlerError(_)
        | RemoteSendError::DeserializeMessage(_)
        | RemoteSendError::SerializeReply(_)
        | RemoteSendError::SerializeHandlerError(_)
        | RemoteSendError::DeserializeHandlerError(_)
        | RemoteSendError::NetworkTimeout
        | RemoteSendError::ConnectionClosed
        | RemoteSendError::Io(_) => RelaySendEffect::MaybeCommitted,
    }
}

/// Wrap a kameo send error as a [`RelayAskError`], preserving the typed
/// classification alongside the rendered diagnostic.
fn send_error<E>(error: RemoteSendError<E>) -> RelayAskError
where
    RemoteSendError<E>: std::fmt::Display,
{
    let failure = classify(&error);
    let effect = classify_effect(&error);
    RelayAskError::Send {
        failure,
        effect,
        message: error.to_string(),
    }
}

impl RelayHandle {
    pub fn new(node_id: NodeId, stop_token: CancellationToken) -> Self {
        let defaults = crate::config::ClusteringMessagingConfig::default();
        Self {
            node_id,
            cached: None,
            mailbox_timeout: defaults.mailbox_timeout,
            reply_timeout: defaults.reply_timeout,
            stop_token,
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
                    });
                }
            }
        }
    }

    /// Ping the relay, refreshing a stale cached ref once: transport-layer
    /// errors that mean "this ActorId is gone" invalidate the cache and
    /// re-resolve via kademlia, then retry the ask exactly once.
    ///
    /// Races the whole ask against this handle's `stop_token`, biased
    /// (cancellation checked first) — see the type-level doc comment.
    pub async fn ping(&mut self) -> Result<RelayPong, RelayAskError> {
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.ping_inner() => result,
        }
    }

    async fn ping_inner(&mut self) -> Result<RelayPong, RelayAskError> {
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
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }

    /// Harness: ask the relay to crash (fault injection). The ask may fail —
    /// the actor can die before the reply leaves — so only the failure
    /// classes plausible for a mid-ask death are tolerated; anything else
    /// (codec mismatch, full mailbox, handler error) means the crash request
    /// never plausibly reached the relay and is propagated so callers don't
    /// pass vacuously. Callers assert recovery separately.
    ///
    /// Races the whole ask against this handle's `stop_token`, biased — see
    /// the type-level doc comment.
    pub async fn crash(&mut self) -> Result<(), RelayAskError> {
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.crash_inner() => result,
        }
    }

    async fn crash_inner(&mut self) -> Result<(), RelayAskError> {
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&RelayCrash)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
            Ok(_) => Ok(()),
            // Dead-before-reply presents as a stale ref, a reply that never
            // arrives, or a torn connection — all expected outcomes here.
            Err(error) => match classify(&error) {
                RelaySendFailure::StaleRef
                | RelaySendFailure::ReplyTimeout
                | RelaySendFailure::Transport => Ok(()),
                RelaySendFailure::MailboxFull
                | RelaySendFailure::Handler
                | RelaySendFailure::Codec => Err(send_error(error)),
            },
        }
    }

    /// Harness: ask the relay to sleep for `millis` inside its handler,
    /// WITHOUT the stale-ref retry (the point is to observe the sender-side
    /// transport timeout, not to recover from it).
    ///
    /// Races the whole ask against this handle's `stop_token`, biased — see
    /// the type-level doc comment.
    pub async fn sleep(&mut self, millis: u64) -> Result<RelayFaultAck, RelayAskError> {
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.sleep_inner(millis) => result,
        }
    }

    async fn sleep_inner(&mut self, millis: u64) -> Result<RelayFaultAck, RelayAskError> {
        let remote_ref = self.resolve().await?;
        remote_ref
            .ask(&RelaySleep { millis })
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
            .map_err(send_error)
    }

    /// Round-trip a stanza through the relay (codec proof), with the same
    /// stale-ref refresh-and-retry-once behaviour as [`Self::ping`].
    ///
    /// Races the whole ask against this handle's `stop_token`, biased — see
    /// the type-level doc comment.
    pub async fn echo_stanza(
        &mut self,
        stanza: RemoteStanza,
    ) -> Result<RelayEchoReply, RelayAskError> {
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.echo_stanza_inner(stanza) => result,
        }
    }

    async fn echo_stanza_inner(
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
            Err(error) if is_no_effect_stale_ref_relookup_error(&error) => {
                self.cached = None;
                let remote_ref = self.resolve().await?;
                remote_ref
                    .ask(&message)
                    .mailbox_timeout(self.mailbox_timeout)
                    .reply_timeout(self.reply_timeout)
                    .await
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }

    /// Send one already-sequenced ordered-relay envelope to the remote relay,
    /// with the same stop-token cancellation and stale-ref refresh/retry-once
    /// behavior as [`Self::echo_stanza`]. Sequencing is deliberately owned
    /// outside `RelayHandle`: callers must share one sender state per ordered
    /// channel rather than allocate from a fresh handle per ask.
    pub async fn deliver_ordered(
        &mut self,
        envelope: RemoteStanzaEnvelope,
    ) -> Result<OrderedRelayReply, RelayAskError> {
        let trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.deliver_ordered_inner(envelope, trace) => result,
        }
    }

    async fn deliver_ordered_inner(
        &mut self,
        envelope: RemoteStanzaEnvelope,
        trace: RelayTraceContext,
    ) -> Result<OrderedRelayReply, RelayAskError> {
        let message = RelayDeliverOrdered { envelope, trace };
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&message)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(error) if is_no_effect_stale_ref_relookup_error(&error) => {
                self.cached = None;
                let remote_ref = self.resolve().await?;
                match remote_ref
                    .ask(&message)
                    .mailbox_timeout(self.mailbox_timeout)
                    .reply_timeout(self.reply_timeout)
                    .await
                {
                    Ok(reply) => Ok(reply),
                    Err(error) => ordered_send_error(&message.envelope, error),
                }
            }
            Err(error) => ordered_send_error(&message.envelope, error),
        }
    }

    pub async fn register_remote_user_resource(
        &mut self,
        mut message: RelayRegisterRemoteUserResource,
    ) -> Result<RelayRemoteResourceRegistrationReply, RelayAskError> {
        message.trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.register_remote_user_resource_inner(message) => result,
        }
    }

    async fn register_remote_user_resource_inner(
        &mut self,
        message: RelayRegisterRemoteUserResource,
    ) -> Result<RelayRemoteResourceRegistrationReply, RelayAskError> {
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&message)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(error) if is_no_effect_stale_ref_relookup_error(&error) => {
                self.cached = None;
                let remote_ref = self.resolve().await?;
                remote_ref
                    .ask(&message)
                    .mailbox_timeout(self.mailbox_timeout)
                    .reply_timeout(self.reply_timeout)
                    .await
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }

    pub async fn unregister_remote_user_resource(
        &mut self,
        mut message: RelayUnregisterRemoteUserResource,
    ) -> Result<RelayRemoteResourceUnregisterReply, RelayAskError> {
        message.trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.unregister_remote_user_resource_inner(message) => result,
        }
    }

    async fn unregister_remote_user_resource_inner(
        &mut self,
        message: RelayUnregisterRemoteUserResource,
    ) -> Result<RelayRemoteResourceUnregisterReply, RelayAskError> {
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&message)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(error) if is_no_effect_stale_ref_relookup_error(&error) => {
                self.cached = None;
                let remote_ref = self.resolve().await?;
                remote_ref
                    .ask(&message)
                    .mailbox_timeout(self.mailbox_timeout)
                    .reply_timeout(self.reply_timeout)
                    .await
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }

    pub async fn update_remote_user_resource(
        &mut self,
        mut message: RelayUpdateRemoteUserResource,
    ) -> Result<RelayRemoteResourceUpdateReply, RelayAskError> {
        message.trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.update_remote_user_resource_inner(message) => result,
        }
    }

    async fn update_remote_user_resource_inner(
        &mut self,
        message: RelayUpdateRemoteUserResource,
    ) -> Result<RelayRemoteResourceUpdateReply, RelayAskError> {
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&message)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(error) if is_no_effect_stale_ref_relookup_error(&error) => {
                self.cached = None;
                let remote_ref = self.resolve().await?;
                remote_ref
                    .ask(&message)
                    .mailbox_timeout(self.mailbox_timeout)
                    .reply_timeout(self.reply_timeout)
                    .await
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }

    pub async fn remote_user_side_effect(
        &mut self,
        mut message: RelayRemoteUserSideEffect,
    ) -> Result<RelayRemoteUserSideEffectReply, RelayAskError> {
        message.trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.remote_user_side_effect_inner(message) => result,
        }
    }

    async fn remote_user_side_effect_inner(
        &mut self,
        message: RelayRemoteUserSideEffect,
    ) -> Result<RelayRemoteUserSideEffectReply, RelayAskError> {
        let remote_ref = self.resolve().await?;
        remote_ref
            .ask(&message)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
            .map_err(send_error)
    }

    pub async fn route_remote_resource_stanza(
        &mut self,
        mut message: RelayRouteRemoteResourceStanza,
    ) -> Result<RelayRouteRemoteResourceStanzaReply, RelayAskError> {
        message.trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.route_remote_resource_stanza_inner(message) => result,
        }
    }

    async fn route_remote_resource_stanza_inner(
        &mut self,
        message: RelayRouteRemoteResourceStanza,
    ) -> Result<RelayRouteRemoteResourceStanzaReply, RelayAskError> {
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&message)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(error) if is_no_effect_stale_ref_relookup_error(&error) => {
                self.cached = None;
                let remote_ref = self.resolve().await?;
                remote_ref
                    .ask(&message)
                    .mailbox_timeout(self.mailbox_timeout)
                    .reply_timeout(self.reply_timeout)
                    .await
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }

    pub async fn deliver_remote_resource_frame(
        &mut self,
        mut message: RelayDeliverRemoteResourceFrame,
    ) -> Result<RelayRemoteResourceFrameReply, RelayAskError> {
        message.trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.deliver_remote_resource_frame_inner(message) => result,
        }
    }

    async fn deliver_remote_resource_frame_inner(
        &mut self,
        message: RelayDeliverRemoteResourceFrame,
    ) -> Result<RelayRemoteResourceFrameReply, RelayAskError> {
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&message)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(error) if is_no_effect_stale_ref_relookup_error(&error) => {
                self.cached = None;
                let remote_ref = self.resolve().await?;
                remote_ref
                    .ask(&message)
                    .mailbox_timeout(self.mailbox_timeout)
                    .reply_timeout(self.reply_timeout)
                    .await
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }

    pub async fn force_detach_remote_user_resource(
        &mut self,
        mut message: RelayForceDetachRemoteUserResource,
    ) -> Result<RelayForceDetachRemoteUserResourceReply, RelayAskError> {
        message.trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.force_detach_remote_user_resource_inner(message) => result,
        }
    }

    async fn force_detach_remote_user_resource_inner(
        &mut self,
        message: RelayForceDetachRemoteUserResource,
    ) -> Result<RelayForceDetachRemoteUserResourceReply, RelayAskError> {
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
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }

    /// Ask this relay's node to force-detach its live SM session
    /// `stream_id` on behalf of `requester_bare_jid` (ADR-0017 Phase 3
    /// Slice 6's cross-node XEP-0198 resume live-steal handshake — this
    /// slice's entire addition to the relay message set, and
    /// `RelayHandle`'s first production, non-harness caller). Same
    /// stale-ref refresh-and-retry-once behaviour as [`Self::ping`].
    ///
    /// Races the whole ask against this handle's `stop_token`, biased — see
    /// the type-level doc comment.
    pub async fn resume_steal(
        &mut self,
        stream_id: waddle_xmpp::pending_delivery::SmSessionId,
        requester_bare_jid: jid::BareJid,
    ) -> Result<RelayResumeStealReply, RelayAskError> {
        let trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.resume_steal_inner(stream_id, requester_bare_jid, trace) => result,
        }
    }

    async fn resume_steal_inner(
        &mut self,
        stream_id: waddle_xmpp::pending_delivery::SmSessionId,
        requester_bare_jid: jid::BareJid,
        trace: RelayTraceContext,
    ) -> Result<RelayResumeStealReply, RelayAskError> {
        let message = RelayResumeSteal {
            stream_id,
            requester_bare_jid,
            trace,
        };
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
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }

    /// Two-part demotion protocol, part (a) (ADR-0017 Phase 3 Slice 7,
    /// element 7): best-effort acked notification to the node `entity`
    /// was just stolen from. Same stale-ref refresh-and-retry-once
    /// behaviour and `stop_token`-raced cancellation-safety as
    /// [`Self::resume_steal`]. Callers must treat any `Err` here as
    /// purely informational (log and move on) — the guaranteed
    /// correctness backstop is the fenced pre-fan-out check, never this
    /// ask's outcome.
    pub async fn demote(
        &mut self,
        entity: Entity,
        new_epoch: ClaimEpoch,
    ) -> Result<DemoteReply, RelayAskError> {
        let trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.demote_inner(entity, new_epoch, trace) => result,
        }
    }

    /// #1594: ask the room's claim owner to re-assert one SFU
    /// participant's media grants from its authoritative occupant
    /// state. Idempotent (re-deriving and re-pushing the same grants
    /// is a no-op), so it shares [`Self::demote`]'s exact stale-ref
    /// refresh-and-retry-once behaviour and `stop_token`-raced
    /// cancellation-safety.
    pub async fn reassert_media_grants(
        &mut self,
        room: jid::BareJid,
        participant: jid::FullJid,
    ) -> Result<RelayReassertMediaGrantsReply, RelayAskError> {
        let trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.reassert_media_grants_inner(room, participant, trace) => result,
        }
    }

    async fn reassert_media_grants_inner(
        &mut self,
        room: jid::BareJid,
        participant: jid::FullJid,
        trace: RelayTraceContext,
    ) -> Result<RelayReassertMediaGrantsReply, RelayAskError> {
        let message = RelayReassertMediaGrants {
            room,
            participant,
            trace,
        };
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
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }

    async fn demote_inner(
        &mut self,
        entity: Entity,
        new_epoch: ClaimEpoch,
        trace: RelayTraceContext,
    ) -> Result<DemoteReply, RelayAskError> {
        let message = Demote {
            entity,
            new_epoch,
            trace,
        };
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
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }
}

/// Transport-layer errors meaning the cached `RemoteActorRef` no longer names
/// a live actor — the explicit re-lookup trigger (ADR element 6). Distinct
/// from handler/timeout errors, which say nothing about registration state.
fn is_stale_ref_error<E>(error: &RemoteSendError<E>) -> bool {
    classify(error) == RelaySendFailure::StaleRef
}

/// Non-idempotent relay messages are not retried after `ActorStopped`: that
/// error may have been enqueued before the actor stopped, so a re-send against
/// a respawned relay could duplicate a committed side effect.
/// `ActorNotRunning`/`UnknownActor`/`BadActorType` are still safe re-lookup
/// cases because they prove the cached ref was unusable.
fn is_no_effect_stale_ref_relookup_error<E>(error: &RemoteSendError<E>) -> bool {
    matches!(
        error,
        RemoteSendError::ActorNotRunning
            | RemoteSendError::UnknownActor { .. }
            | RemoteSendError::BadActorType
    )
}

/// True only for errors that prove the peer never ran the handler
/// because it does not know the (versioned) remote message id.
///
/// Deliberately excludes `DeserializeMessage`: kameo raises that both
/// for the peer failing to decode the request (uncommitted) and for
/// this node failing to decode the peer's reply (handler already
/// committed), so it can never be treated as provably-uncommitted.
/// The versioned `deliver_ordered.vN` id exists precisely so envelope
/// format changes surface as `UnknownMessage` instead (#1597).
fn is_ordered_unsupported_envelope_error<E>(error: &RemoteSendError<E>) -> bool {
    matches!(error, RemoteSendError::UnknownMessage { .. })
}

fn ordered_send_error<E>(
    envelope: &RemoteStanzaEnvelope,
    error: RemoteSendError<E>,
) -> Result<OrderedRelayReply, RelayAskError>
where
    RemoteSendError<E>: std::fmt::Display,
{
    if is_ordered_unsupported_envelope_error(&error) {
        return Ok(OrderedRelayReply::Nack(OrderedRelayNack {
            channel: envelope.channel.clone(),
            sequence: envelope.sequence,
            reason: OrderedRelayNackReason::UnsupportedEnvelope,
        }));
    }
    Err(send_error(error))
}

#[cfg(test)]
mod tests;
