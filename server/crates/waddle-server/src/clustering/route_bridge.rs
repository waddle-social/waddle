//! ADR-0017 Phase 4 Slice 3: ordered-relay bridge for cross-node 1:1
//! full-JID delivery.
//!
//! The relay actor is spawned during clustering bring-up, before the
//! WebSocket routing graph exists. This bridge mirrors
//! [`super::resume_bridge::ResumeStealBridge`]: construct it empty at swarm
//! spawn time, then wire the narrow services it needs once
//! `create_websocket_state` has built the live registries.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use kameo::actor::ActorRef;
use libp2p::identity::{Keypair, PublicKey};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::{
    ClaimSnapshot, ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity,
};
use waddle_xmpp::registry::{BroadcastOutcome, ConnectionRegistry, UserRegistryActor};
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
use waddle_xmpp::Stanza;

use super::claims::NodeLeaseStore;
use super::codec::RemoteStanza;
use super::ordered_relay::{
    OrderedRelayChannel, OrderedRelayClaim, OrderedRelayClaimRole, OrderedRelayDiversion,
    OrderedRelayDiversionReason, OrderedRelayNack, OrderedRelayNackReason, OrderedRelayOrigin,
    OrderedRelayOriginProof, OrderedRelayPayload, OrderedRelayRecipient, OrderedRelayReply,
    OrderedRelaySenderState, OriginInboundSequence, RemoteStanzaEnvelope,
};
use super::relay::{RelayAskError, RelayHandle, RelaySendEffect, RelaySendFailure};
use super::NodeId;
use crate::config::ClusteringMessagingConfig;
use crate::server::routes::interpret::{
    FullJidDeliveryOutcome, OrderedRelayRouteOrigin, OrderedRelayRouteOriginKind,
};
use crate::server::routes::websocket::{interpret_loop::build_interpret_deps, WebSocketState};
const ORDERED_DELIVERY_MAILBOX_TIMEOUT: Duration = Duration::from_secs(2);
const ORDERED_DELIVERY_REPLY_TIMEOUT: Duration = Duration::from_secs(8);
const ORDERED_RECEIVER_DELIVERY_TIMEOUT: Duration = Duration::from_secs(6);
const ORDERED_RECEIVER_EFFECT_TIMEOUT_MARGIN: Duration = Duration::from_millis(250);
const ORDERED_HANDOFF_TASK_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_ORDERED_RELAY_CHANNEL_LOCKS: usize = 4096;

/// Narrow service bundle needed by the relay receiver to validate ownership
/// and hand an inbound full-JID stanza to the local `UserActor` delivery path.
pub struct OrderedRelayDeliveryServices {
    pub claim_store: Arc<dyn ClaimStore>,
    pub node_lease: Arc<dyn NodeLeaseStore>,
    pub node_identity: SharedNodeIdentity,
    pub connection_registry: Arc<ConnectionRegistry>,
    pub user_registry: ActorRef<UserRegistryActor>,
    pub sm_session_registry: Arc<InMemorySmSessionRegistry>,
    pub web_socket_state: Weak<WebSocketState>,
}

struct RelayOriginSigner {
    keypair: Keypair,
    public_key: Vec<u8>,
}

impl RelayOriginSigner {
    fn new(keypair: Keypair) -> Self {
        let public_key = keypair.public().encode_protobuf();
        Self {
            keypair,
            public_key,
        }
    }
}

struct PreparedRemoteDelivery {
    services: Arc<OrderedRelayDeliveryServices>,
    target_entity: Entity,
    previous_owner: NodeIdentity,
    channel: OrderedRelayChannel,
    envelope: RemoteStanzaEnvelope,
    target: jid::Jid,
    stanza: Stanza,
    is_iq: bool,
}

struct RemoteDeliverySeed {
    services: Arc<OrderedRelayDeliveryServices>,
    target_entity: Entity,
    previous_owner: NodeIdentity,
    channel: OrderedRelayChannel,
    asserted_origin_node: NodeId,
    origin_inbound_sequence: OriginInboundSequence,
    origin_claim: OrderedRelayClaim,
    target_claim: OrderedRelayClaim,
    payload: OrderedRelayPayload,
    target: jid::Jid,
    stanza: Stanza,
    is_iq: bool,
}

/// Construction-order bridge plus shared sender sequencing state.
pub struct OrderedRelayDeliveryBridge {
    services: OnceLock<Arc<OrderedRelayDeliveryServices>>,
    origin_signer: OnceLock<RelayOriginSigner>,
    sender_state: Mutex<OrderedRelaySenderState>,
    channel_locks: Mutex<HashMap<OrderedRelayChannel, Arc<Mutex<()>>>>,
    stop_token: CancellationToken,
    mailbox_timeout: Duration,
    reply_timeout: Duration,
}

impl OrderedRelayDeliveryBridge {
    pub fn new(stop_token: CancellationToken, messaging: &ClusteringMessagingConfig) -> Arc<Self> {
        Arc::new(Self {
            services: OnceLock::new(),
            origin_signer: OnceLock::new(),
            sender_state: Mutex::new(OrderedRelaySenderState::default()),
            channel_locks: Mutex::new(HashMap::new()),
            stop_token,
            // WebSocket stanza dispatch has a 15s wedge backstop. Full-JID
            // ordered delivery must resolve before that backstop can cancel
            // the origin future, otherwise the receiver may still commit a
            // side effect after the origin has synthesized a local timeout.
            mailbox_timeout: messaging
                .mailbox_timeout
                .min(ORDERED_DELIVERY_MAILBOX_TIMEOUT),
            reply_timeout: messaging.reply_timeout.min(ORDERED_DELIVERY_REPLY_TIMEOUT),
        })
    }

    pub fn wire(&self, services: Arc<OrderedRelayDeliveryServices>) {
        if self.services.set(services).is_err() {
            tracing::error!(
                "OrderedRelayDeliveryBridge::wire called more than once; \
                 ignoring duplicate service bundle"
            );
        }
    }

    pub fn wire_origin_signer(&self, keypair: Keypair) {
        if self
            .origin_signer
            .set(RelayOriginSigner::new(keypair))
            .is_err()
        {
            tracing::error!(
                "OrderedRelayDeliveryBridge::wire_origin_signer called more than once; \
                 ignoring duplicate signer"
            );
        }
    }

    pub(crate) fn reserved_delivery_effect_timeout(&self) -> Duration {
        self.reply_timeout
            .checked_sub(ORDERED_RECEIVER_EFFECT_TIMEOUT_MARGIN)
            .filter(|timeout| !timeout.is_zero())
            .unwrap_or_else(|| self.reply_timeout / 2)
    }

    /// Return `Some` only when this exact full-JID target is currently owned
    /// by a fresh foreign `UserActor` claim and an ordered-relay send was
    /// attempted. `None` means the caller must keep the existing local path.
    pub(crate) async fn try_deliver_full_jid_remote(
        self: &Arc<Self>,
        target: &jid::FullJid,
        stanza: &Stanza,
        origin: &OrderedRelayRouteOrigin,
    ) -> Option<FullJidDeliveryOutcome> {
        let services = self.services.get()?.clone();
        let target_entity = user_entity(&target.to_bare());
        let target_snapshot = current_claim(&services, &target_entity).await?;
        if !target_snapshot.owner_lease_fresh {
            return None;
        }
        let me = services.node_identity.current();
        if target_snapshot.owner == me {
            return None;
        }

        let (origin_entity, channel_origin) = route_origin_claim(&origin.kind);
        let origin_snapshot = current_claim(&services, &origin_entity).await?;
        if !origin_snapshot.owner_lease_fresh || origin_snapshot.owner != me {
            tracing::debug!(
                target = %target,
                origin_entity = %origin_entity,
                "ordered relay: origin entity is not currently owned locally; \
                 keeping local fallback path"
            );
            return None;
        }

        let payload = payload_for_recipient(jid::Jid::from(target.clone()), stanza)?;
        let is_iq = matches!(stanza, Stanza::Iq(_));
        let channel = OrderedRelayChannel {
            origin: channel_origin,
            recipient: OrderedRelayRecipient::FullJid(target.clone()),
            target_epoch: target_snapshot.claim_epoch,
        };
        let origin_claim = OrderedRelayClaim {
            entity: origin_entity,
            epoch: origin_snapshot.claim_epoch,
        };
        let target_claim = OrderedRelayClaim {
            entity: target_entity.clone(),
            epoch: target_snapshot.claim_epoch,
        };
        let seed = RemoteDeliverySeed {
            services,
            target_entity,
            previous_owner: target_snapshot.owner,
            channel,
            asserted_origin_node: NodeId::new(me.node_id.clone()),
            origin_inbound_sequence: OriginInboundSequence(origin.inbound_sequence),
            origin_claim,
            target_claim,
            payload,
            target: jid::Jid::from(target.clone()),
            stanza: stanza.clone(),
            is_iq,
        };

        if let Some(handoff) = origin.handoff.clone() {
            if handoff.mark_deferred() {
                let bridge = Arc::clone(self);
                let origin_stanza = stanza.clone();
                tokio::spawn(async move {
                    let outcome = match tokio::time::timeout(
                        ORDERED_HANDOFF_TASK_TIMEOUT,
                        bridge.deliver_seeded_remote(seed, true),
                    )
                    .await
                    {
                        Ok(outcome) => outcome,
                        Err(_) => {
                            tracing::warn!(
                                timeout_ms = ORDERED_HANDOFF_TASK_TIMEOUT.as_millis(),
                                "ordered relay: deferred handoff timed out"
                            );
                            FullJidDeliveryOutcome::Dropped
                        }
                    };
                    handoff.complete(replies_for_origin_handoff(&origin_stanza, outcome));
                });
                return Some(FullJidDeliveryOutcome::Delivered);
            }
        }

        Some(Arc::clone(self).deliver_seeded_remote(seed, true).await)
    }

    /// Return `Some` only when this bare-JID target is currently owned by a
    /// fresh foreign `UserActor` claim and an ordered-relay send was attempted.
    /// `None` means the caller must keep the existing local path.
    pub(crate) async fn try_deliver_bare_jid_remote(
        self: &Arc<Self>,
        target: &jid::BareJid,
        stanza: &Stanza,
        origin: &OrderedRelayRouteOrigin,
    ) -> Option<FullJidDeliveryOutcome> {
        let services = self.services.get()?.clone();
        let target_entity = user_entity(target);
        let target_snapshot = current_claim(&services, &target_entity).await?;
        if !target_snapshot.owner_lease_fresh {
            return None;
        }
        let me = services.node_identity.current();
        if target_snapshot.owner == me {
            return None;
        }

        let (origin_entity, channel_origin) = route_origin_claim(&origin.kind);
        let origin_snapshot = current_claim(&services, &origin_entity).await?;
        if !origin_snapshot.owner_lease_fresh || origin_snapshot.owner != me {
            tracing::debug!(
                target = %target,
                origin_entity = %origin_entity,
                "ordered relay: origin entity is not currently owned locally; \
                 keeping local fallback path"
            );
            return None;
        }

        let payload = payload_for_recipient(jid::Jid::from(target.clone()), stanza)?;
        let is_iq = matches!(stanza, Stanza::Iq(_));
        let channel = OrderedRelayChannel {
            origin: channel_origin,
            recipient: OrderedRelayRecipient::BareJid(target.clone()),
            target_epoch: target_snapshot.claim_epoch,
        };
        let origin_claim = OrderedRelayClaim {
            entity: origin_entity,
            epoch: origin_snapshot.claim_epoch,
        };
        let target_claim = OrderedRelayClaim {
            entity: target_entity.clone(),
            epoch: target_snapshot.claim_epoch,
        };
        let seed = RemoteDeliverySeed {
            services,
            target_entity,
            previous_owner: target_snapshot.owner,
            channel,
            asserted_origin_node: NodeId::new(me.node_id.clone()),
            origin_inbound_sequence: OriginInboundSequence(origin.inbound_sequence),
            origin_claim,
            target_claim,
            payload,
            target: jid::Jid::from(target.clone()),
            stanza: stanza.clone(),
            is_iq,
        };

        if let Some(handoff) = origin.handoff.clone() {
            if handoff.mark_deferred() {
                let bridge = Arc::clone(self);
                let origin_stanza = stanza.clone();
                tokio::spawn(async move {
                    let outcome = match tokio::time::timeout(
                        ORDERED_HANDOFF_TASK_TIMEOUT,
                        bridge.deliver_seeded_remote(seed, true),
                    )
                    .await
                    {
                        Ok(outcome) => outcome,
                        Err(_) => {
                            tracing::warn!(
                                timeout_ms = ORDERED_HANDOFF_TASK_TIMEOUT.as_millis(),
                                "ordered relay: deferred handoff timed out"
                            );
                            FullJidDeliveryOutcome::Dropped
                        }
                    };
                    handoff.complete(replies_for_origin_handoff(&origin_stanza, outcome));
                });
                return Some(FullJidDeliveryOutcome::Delivered);
            }
        }

        Some(Arc::clone(self).deliver_seeded_remote(seed, true).await)
    }

    async fn deliver_seeded_remote(
        self: Arc<Self>,
        seed: RemoteDeliverySeed,
        allow_target_refresh_retry: bool,
    ) -> FullJidDeliveryOutcome {
        let channel = seed.channel.clone();
        let Some(lock) = self.lock_for_channel(&channel).await else {
            self.divert_channel(channel, OrderedRelayDiversionReason::Backpressure)
                .await;
            return definite_no_effect_outcome(seed.is_iq);
        };
        let outcome = {
            let _guard = lock.lock().await;
            match self.prepare_remote_delivery(seed).await {
                Ok(prepared) => {
                    Arc::clone(&self)
                        .deliver_prepared_remote(prepared, allow_target_refresh_retry)
                        .await
                }
                Err(outcome) => outcome,
            }
        };
        self.remove_channel_lock_if_unused(&channel, &lock).await;
        outcome
    }

    async fn prepare_remote_delivery(
        &self,
        seed: RemoteDeliverySeed,
    ) -> Result<PreparedRemoteDelivery, FullJidDeliveryOutcome> {
        let mut envelope = {
            let mut sender = self.sender_state.lock().await;
            match sender.next_envelope(
                seed.asserted_origin_node,
                seed.channel.clone(),
                seed.origin_inbound_sequence,
                seed.origin_claim,
                seed.target_claim,
                seed.payload,
            ) {
                Ok(envelope) => envelope,
                Err(diversion) => {
                    tracing::warn!(
                        target = %seed.target,
                        reason = ?diversion.reason,
                        "ordered relay: sender channel diverted; dropping to avoid \
                         reordering"
                    );
                    return Err(definite_no_effect_outcome(seed.is_iq));
                }
            }
        };
        let channel = envelope.channel.clone();
        if self.sign_envelope(&mut envelope).is_err() {
            self.divert_channel(channel, OrderedRelayDiversionReason::Unreachable)
                .await;
            return Err(definite_no_effect_outcome(seed.is_iq));
        }
        Ok(PreparedRemoteDelivery {
            services: seed.services,
            target_entity: seed.target_entity,
            previous_owner: seed.previous_owner,
            channel,
            envelope,
            target: seed.target,
            stanza: seed.stanza,
            is_iq: seed.is_iq,
        })
    }

    async fn lock_for_channel(&self, channel: &OrderedRelayChannel) -> Option<Arc<Mutex<()>>> {
        let mut locks = self.channel_locks.lock().await;
        if !locks.contains_key(channel) && locks.len() >= MAX_ORDERED_RELAY_CHANNEL_LOCKS {
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        }
        if !locks.contains_key(channel) && locks.len() >= MAX_ORDERED_RELAY_CHANNEL_LOCKS {
            tracing::warn!(
                limit = MAX_ORDERED_RELAY_CHANNEL_LOCKS,
                "ordered relay: channel lock map is full"
            );
            return None;
        }
        Some(
            locks
                .entry(channel.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone(),
        )
    }

    async fn remove_channel_lock_if_unused(
        &self,
        channel: &OrderedRelayChannel,
        lock: &Arc<Mutex<()>>,
    ) {
        let mut locks = self.channel_locks.lock().await;
        if locks
            .get(channel)
            .is_some_and(|existing| Arc::ptr_eq(existing, lock) && Arc::strong_count(lock) == 2)
        {
            locks.remove(channel);
        }
    }

    async fn deliver_prepared_remote(
        self: Arc<Self>,
        prepared: PreparedRemoteDelivery,
        allow_target_refresh_retry: bool,
    ) -> FullJidDeliveryOutcome {
        let result = self
            .send_prepared_to_owner(&prepared.previous_owner, prepared.envelope.clone())
            .await;
        if allow_target_refresh_retry
            && matches!(
                &result,
                Ok(OrderedRelayReply::Nack(OrderedRelayNack {
                    reason: OrderedRelayNackReason::NotOwner {
                        role: OrderedRelayClaimRole::Target
                    },
                    ..
                }))
            )
        {
            if let Some(outcome) = Arc::clone(&self)
                .retry_after_target_owner_refresh(&prepared)
                .await
            {
                return outcome;
            }
        }
        if allow_target_refresh_retry
            && matches!(&result, Err(error) if ask_error_allows_target_refresh(error))
        {
            if let Some(outcome) = Arc::clone(&self)
                .retry_after_target_owner_refresh(&prepared)
                .await
            {
                return outcome;
            }
        }

        self.finish_prepared_delivery_result(prepared, result).await
    }

    async fn send_prepared_to_owner(
        &self,
        owner: &NodeIdentity,
        envelope: RemoteStanzaEnvelope,
    ) -> Result<OrderedRelayReply, RelayAskError> {
        let mut handle =
            RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        handle.deliver_ordered(envelope).await
    }

    async fn finish_prepared_delivery_result(
        self: Arc<Self>,
        prepared: PreparedRemoteDelivery,
        result: Result<OrderedRelayReply, RelayAskError>,
    ) -> FullJidDeliveryOutcome {
        match result {
            Ok(OrderedRelayReply::Ack(_)) => FullJidDeliveryOutcome::Delivered,
            Ok(OrderedRelayReply::Nack(nack)) => {
                let (outcome, channel_action) = outcome_for_nack(
                    &prepared.services,
                    &prepared.target_entity,
                    &prepared.previous_owner,
                    &nack,
                    prepared.is_iq,
                )
                .await;
                self.apply_nack_channel_action(prepared.channel, channel_action)
                    .await;
                match outcome {
                    Some(outcome) => outcome,
                    None => {
                        deliver_local_after_target_refresh(
                            &prepared.services,
                            &prepared.target,
                            &prepared.stanza,
                        )
                        .await
                    }
                }
            }
            Err(error) => {
                self.divert_channel(prepared.channel, diversion_reason_for_ask_error(&error))
                    .await;
                outcome_for_ask_error(&error, prepared.is_iq)
            }
        }
    }

    async fn retry_after_target_owner_refresh(
        self: Arc<Self>,
        prepared: &PreparedRemoteDelivery,
    ) -> Option<FullJidDeliveryOutcome> {
        let snapshot = current_claim(&prepared.services, &prepared.target_entity).await?;
        if !snapshot.owner_lease_fresh {
            return None;
        }

        self.forget_channel(&prepared.envelope.channel).await;
        let me = prepared.services.node_identity.current();
        if snapshot.owner == me {
            return Some(
                deliver_local_after_target_refresh(
                    &prepared.services,
                    &prepared.target,
                    &prepared.stanza,
                )
                .await,
            );
        }

        let new_channel = OrderedRelayChannel {
            origin: prepared.envelope.channel.origin.clone(),
            recipient: prepared.envelope.channel.recipient.clone(),
            target_epoch: snapshot.claim_epoch,
        };
        if new_channel == prepared.envelope.channel {
            return None;
        }

        if snapshot.owner == prepared.previous_owner
            && snapshot.claim_epoch == prepared.envelope.target_claim.epoch
        {
            return None;
        }

        tracing::debug!(
            entity_id = %prepared.target_entity.id,
            previous_owner = %prepared.previous_owner.node_id,
            refreshed_owner = %snapshot.owner.node_id,
            previous_epoch = prepared.envelope.target_claim.epoch.0,
            refreshed_epoch = snapshot.claim_epoch.0,
            "ordered relay: retrying target-owner NACK on refreshed ordered channel"
        );

        let seed = RemoteDeliverySeed {
            services: prepared.services.clone(),
            target_entity: prepared.target_entity.clone(),
            previous_owner: snapshot.owner,
            channel: new_channel.clone(),
            asserted_origin_node: prepared.envelope.asserted_origin_node.clone(),
            origin_inbound_sequence: prepared.envelope.origin_inbound_sequence,
            origin_claim: prepared.envelope.origin_claim.clone(),
            target_claim: OrderedRelayClaim {
                entity: prepared.target_entity.clone(),
                epoch: snapshot.claim_epoch,
            },
            payload: prepared.envelope.payload.clone(),
            target: prepared.target.clone(),
            stanza: prepared.stanza.clone(),
            is_iq: prepared.is_iq,
        };
        let Some(lock) = self.lock_for_channel(&new_channel).await else {
            self.divert_channel(new_channel, OrderedRelayDiversionReason::Backpressure)
                .await;
            return Some(definite_no_effect_outcome(prepared.is_iq));
        };
        let outcome = {
            let _guard = lock.lock().await;
            match self.prepare_remote_delivery(seed).await {
                Ok(retry) => {
                    let result = self
                        .send_prepared_to_owner(&retry.previous_owner, retry.envelope.clone())
                        .await;
                    Arc::clone(&self)
                        .finish_prepared_delivery_result(retry, result)
                        .await
                }
                Err(outcome) => outcome,
            }
        };
        self.remove_channel_lock_if_unused(&new_channel, &lock)
            .await;
        Some(outcome)
    }

    /// Receiver-side effect for one already-reserved envelope. The caller
    /// commits the reservation only when this returns `Ok(())`.
    pub async fn deliver_reserved(
        &self,
        envelope: &RemoteStanzaEnvelope,
    ) -> Result<(), OrderedRelayNackReason> {
        let Some(services) = self.services.get().cloned() else {
            return Err(OrderedRelayNackReason::Unreachable);
        };
        validate_claims(&services, envelope).await?;
        match relay_payload_target(envelope)? {
            RelayPayloadTarget::Full(target, stanza) => {
                deliver_reserved_full_jid(&services, target, stanza).await
            }
            RelayPayloadTarget::Bare(target, stanza) => {
                deliver_reserved_bare_jid(&services, &target, stanza).await
            }
        }
    }

    fn sign_envelope(&self, envelope: &mut RemoteStanzaEnvelope) -> Result<(), ()> {
        let Some(signer) = self.origin_signer.get() else {
            tracing::warn!("ordered relay: origin signer is not wired; dropping envelope");
            return Err(());
        };
        let signing_bytes = envelope.signing_bytes().map_err(|error| {
            tracing::warn!(
                %error,
                "ordered relay: failed to serialize envelope signing bytes"
            );
        })?;
        let signature = signer.keypair.sign(&signing_bytes).map_err(|error| {
            tracing::warn!(
                %error,
                "ordered relay: failed to sign envelope"
            );
        })?;
        envelope.origin_proof = Some(OrderedRelayOriginProof {
            public_key: signer.public_key.clone(),
            signature,
        });
        Ok(())
    }

    async fn divert_channel(
        &self,
        channel: OrderedRelayChannel,
        reason: OrderedRelayDiversionReason,
    ) {
        self.sender_state
            .lock()
            .await
            .divert(OrderedRelayDiversion { channel, reason });
    }

    async fn forget_channel(&self, channel: &OrderedRelayChannel) {
        self.sender_state.lock().await.forget_channel(channel);
    }

    async fn apply_nack_channel_action(
        &self,
        channel: OrderedRelayChannel,
        action: NackChannelAction,
    ) {
        match action {
            NackChannelAction::Divert(reason) => self.divert_channel(channel, reason).await,
            NackChannelAction::Forget => self.forget_channel(&channel).await,
            NackChannelAction::Keep => {}
        }
    }
}

async fn deliver_reserved_full_jid(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> Result<(), OrderedRelayNackReason> {
    if matches!(stanza, Stanza::Iq(_)) {
        return deliver_reserved_full_jid_peer_live_only(services, target, stanza).await;
    }
    match crate::server::routes::interpret::deliver_peer_to_full(
        Some(&services.user_registry),
        Some(&services.sm_session_registry),
        target,
        stanza,
    )
    .await
    {
        FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => Ok(()),
        FullJidDeliveryOutcome::Dropped => Err(OrderedRelayNackReason::Backpressure),
        FullJidDeliveryOutcome::Unavailable => Err(OrderedRelayNackReason::TargetUnavailable),
    }
}

async fn deliver_reserved_full_jid_peer_live_only(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> Result<(), OrderedRelayNackReason> {
    let user_actor = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
        .map_err(|error| {
            tracing::warn!(
                target = %target,
                %error,
                "ordered relay: failed to resolve target UserActor for full-JID IQ"
            );
            OrderedRelayNackReason::Unreachable
        })?;
    let Some(user_actor) = user_actor else {
        return Err(OrderedRelayNackReason::TargetUnavailable);
    };

    match user_actor
        .ask(waddle_xmpp::registry::TrySendPeer {
            jid: target.clone(),
            stanza: stanza.clone(),
        })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
    {
        Ok(BroadcastOutcome::Delivered) => Ok(()),
        Ok(BroadcastOutcome::NotConnected | BroadcastOutcome::DroppedClosed) => {
            Err(OrderedRelayNackReason::TargetUnavailable)
        }
        Ok(BroadcastOutcome::DroppedFull) => Err(OrderedRelayNackReason::Backpressure),
        Err(error) => {
            tracing::warn!(
                target = %target,
                %error,
                "ordered relay: live-only full-JID IQ peer delivery failed"
            );
            Err(OrderedRelayNackReason::InFlight)
        }
    }
}

async fn deliver_reserved_bare_jid(
    services: &OrderedRelayDeliveryServices,
    target: &jid::BareJid,
    stanza: &Stanza,
) -> Result<(), OrderedRelayNackReason> {
    let origin = OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(user_entity(target)),
        inbound_sequence: 0,
        handoff: None,
    };
    let replies = route_local_bare_jid_with_timeout(services, target, stanza, Some(origin)).await?;
    if !replies.is_empty() {
        tracing::warn!(
            bare_jid = %target,
            reply_count = replies.len(),
            "ordered relay: receiver-side bare-JID delivery produced local fallback replies"
        );
        return Err(OrderedRelayNackReason::TargetUnavailable);
    }
    Ok(())
}

async fn route_local_bare_jid_with_timeout(
    services: &OrderedRelayDeliveryServices,
    target: &jid::BareJid,
    stanza: &Stanza,
    origin: Option<OrderedRelayRouteOrigin>,
) -> Result<Vec<Stanza>, OrderedRelayNackReason> {
    let Some(state) = services.web_socket_state.upgrade() else {
        tracing::warn!(
            bare_jid = %target,
            "ordered relay: WebSocket state is gone; cannot deliver bare-JID relay payload"
        );
        return Err(OrderedRelayNackReason::Unreachable);
    };
    let deps = build_interpret_deps(state.as_ref(), None).with_ordered_relay_origin(origin);
    match tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::interpret::route_to_connection(
            &deps,
            jid::Jid::from(target.clone()),
            Box::new(stanza.clone()),
            0,
        ),
    )
    .await
    {
        Ok(replies) => Ok(replies),
        Err(_) => {
            tracing::warn!(
                bare_jid = %target,
                timeout_ms = ORDERED_RECEIVER_DELIVERY_TIMEOUT.as_millis(),
                "ordered relay: local bare-JID delivery timed out"
            );
            Err(OrderedRelayNackReason::MaybeCommitted)
        }
    }
}

async fn current_claim(
    services: &OrderedRelayDeliveryServices,
    entity: &Entity,
) -> Option<ClaimSnapshot> {
    match services.claim_store.current_claim(entity).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(
                entity = %entity,
                %error,
                "ordered relay: claim lookup failed"
            );
            None
        }
    }
}

fn user_entity(bare: &jid::BareJid) -> Entity {
    Entity::new(EntityType::UserActor, bare.to_string())
}

fn route_origin_claim(kind: &OrderedRelayRouteOriginKind) -> (Entity, OrderedRelayOrigin) {
    match kind {
        OrderedRelayRouteOriginKind::SmSession(stream_id) => (
            Entity::new(EntityType::SmSession, stream_id.to_string()),
            OrderedRelayOrigin::SmSession(stream_id.clone()),
        ),
        OrderedRelayRouteOriginKind::Entity(entity) => {
            (entity.clone(), OrderedRelayOrigin::Entity(entity.clone()))
        }
    }
}

fn payload_for_recipient(recipient: jid::Jid, stanza: &Stanza) -> Option<OrderedRelayPayload> {
    match stanza {
        Stanza::Message(message)
            if message.type_ == xmpp_parsers::message::MessageType::Groupchat =>
        {
            None
        }
        Stanza::Message(_) => Some(OrderedRelayPayload::Message {
            recipient,
            stanza: RemoteStanza(stanza.clone()),
        }),
        Stanza::Iq(_) => Some(OrderedRelayPayload::Iq {
            recipient,
            stanza: RemoteStanza(stanza.clone()),
        }),
        Stanza::Presence(_) => Some(OrderedRelayPayload::Presence {
            recipient,
            stanza: RemoteStanza(stanza.clone()),
        }),
    }
}

enum RelayPayloadTarget<'a> {
    Full(&'a jid::FullJid, &'a Stanza),
    Bare(jid::BareJid, &'a Stanza),
}

fn relay_payload_target(
    envelope: &RemoteStanzaEnvelope,
) -> Result<RelayPayloadTarget<'_>, OrderedRelayNackReason> {
    let (recipient, stanza) = match &envelope.payload {
        OrderedRelayPayload::Message { recipient, stanza }
        | OrderedRelayPayload::Iq { recipient, stanza }
        | OrderedRelayPayload::Presence { recipient, stanza } => Ok((recipient, &stanza.0)),
        OrderedRelayPayload::MucProxy { .. } => Err(OrderedRelayNackReason::Unreachable),
    }?;
    match &envelope.channel.recipient {
        OrderedRelayRecipient::FullJid(full) if recipient == &jid::Jid::from(full.clone()) => {
            Ok(RelayPayloadTarget::Full(full, stanza))
        }
        OrderedRelayRecipient::BareJid(bare) if recipient == &jid::Jid::from(bare.clone()) => {
            Ok(RelayPayloadTarget::Bare(bare.clone(), stanza))
        }
        OrderedRelayRecipient::FullJid(_) | OrderedRelayRecipient::BareJid(_) => {
            Err(OrderedRelayNackReason::ParseFailure)
        }
        OrderedRelayRecipient::Room(_) => Err(OrderedRelayNackReason::Unreachable),
    }
}

async fn deliver_local_after_target_refresh(
    services: &OrderedRelayDeliveryServices,
    target: &jid::Jid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    match target.clone().try_into_full() {
        Ok(full) => deliver_local_full_jid_after_target_refresh(services, &full, stanza).await,
        Err(bare) => match route_local_bare_jid_with_timeout(services, &bare, stanza, None).await {
            Ok(replies) if !replies.is_empty() => FullJidDeliveryOutcome::Unavailable,
            Ok(_) => FullJidDeliveryOutcome::Delivered,
            Err(error) => {
                tracing::warn!(
                    bare_jid = %bare,
                    ?error,
                    "ordered relay: target-owner refresh resolved to local bare-JID \
                     owner but local delivery did not complete"
                );
                FullJidDeliveryOutcome::Dropped
            }
        },
    }
}

async fn deliver_local_full_jid_after_target_refresh(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    if matches!(stanza, Stanza::Iq(_)) {
        return match deliver_reserved_full_jid_peer_live_only(services, target, stanza).await {
            Ok(()) => FullJidDeliveryOutcome::Delivered,
            Err(OrderedRelayNackReason::TargetUnavailable) => FullJidDeliveryOutcome::Unavailable,
            Err(_) => FullJidDeliveryOutcome::Dropped,
        };
    }
    crate::server::routes::interpret::deliver_peer_to_full(
        Some(&services.user_registry),
        Some(&services.sm_session_registry),
        target,
        stanza,
    )
    .await
}

async fn validate_claims(
    services: &OrderedRelayDeliveryServices,
    envelope: &RemoteStanzaEnvelope,
) -> Result<(), OrderedRelayNackReason> {
    let origin = services
        .claim_store
        .current_claim(&envelope.origin_claim.entity)
        .await
        .map_err(|error| {
            tracing::warn!(
                entity = %envelope.origin_claim.entity,
                %error,
                "ordered relay: origin claim lookup failed"
            );
            OrderedRelayNackReason::Unreachable
        })?
        .ok_or(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        })?;
    if !origin.owner_lease_fresh
        || origin.claim_epoch != envelope.origin_claim.epoch
        || origin.owner.node_id != envelope.asserted_origin_node.as_str()
    {
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    }
    validate_origin_proof(services, envelope, &origin.owner).await?;

    let target = services
        .claim_store
        .current_claim(&envelope.target_claim.entity)
        .await
        .map_err(|error| {
            tracing::warn!(
                entity = %envelope.target_claim.entity,
                %error,
                "ordered relay: target claim lookup failed"
            );
            OrderedRelayNackReason::Unreachable
        })?
        .ok_or(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        })?;
    let me = services.node_identity.current();
    if !target.owner_lease_fresh
        || target.claim_epoch != envelope.target_claim.epoch
        || target.owner != me
    {
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        });
    }
    Ok(())
}

async fn validate_origin_proof(
    services: &OrderedRelayDeliveryServices,
    envelope: &RemoteStanzaEnvelope,
    origin_owner: &waddle_xmpp::ownership::NodeIdentity,
) -> Result<(), OrderedRelayNackReason> {
    let Some(proof) = &envelope.origin_proof else {
        tracing::warn!(
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: unsigned origin envelope rejected"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    };
    let public_key = PublicKey::try_decode_protobuf(&proof.public_key).map_err(|error| {
        tracing::warn!(
            %error,
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: origin proof public key did not decode"
        );
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        }
    })?;
    let signing_bytes = envelope.signing_bytes().map_err(|error| {
        tracing::warn!(
            %error,
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: failed to serialize origin verification bytes"
        );
        OrderedRelayNackReason::ParseFailure
    })?;
    if !public_key.verify(&signing_bytes, &proof.signature) {
        tracing::warn!(
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: origin proof signature verification failed"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    }
    let signed_peer_id = public_key.to_peer_id().to_string();
    let registered_peer_id = services
        .node_lease
        .peer_id_for_node(origin_owner)
        .await
        .map_err(|error| {
            tracing::warn!(
                %error,
                node_id = %origin_owner.node_id,
                "ordered relay: failed to load origin node PeerId binding"
            );
            OrderedRelayNackReason::Unreachable
        })?
        .ok_or_else(|| {
            tracing::warn!(
                node_id = %origin_owner.node_id,
                "ordered relay: origin node has no PeerId binding"
            );
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Origin,
            }
        })?;
    if registered_peer_id != signed_peer_id {
        tracing::warn!(
            node_id = %origin_owner.node_id,
            registered_peer_id = %registered_peer_id,
            signed_peer_id = %signed_peer_id,
            "ordered relay: origin proof PeerId does not match node lease binding"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    }
    Ok(())
}

async fn outcome_for_nack(
    services: &OrderedRelayDeliveryServices,
    target_entity: &Entity,
    previous_owner: &waddle_xmpp::ownership::NodeIdentity,
    nack: &OrderedRelayNack,
    is_iq: bool,
) -> (Option<FullJidDeliveryOutcome>, NackChannelAction) {
    match nack.reason {
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        } => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner),
        ),
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        } => {
            let Some(snapshot) = current_claim(services, target_entity).await else {
                return (
                    Some(FullJidDeliveryOutcome::Unavailable),
                    NackChannelAction::Divert(OrderedRelayDiversionReason::Unreachable),
                );
            };
            if !snapshot.owner_lease_fresh {
                return (
                    Some(FullJidDeliveryOutcome::Unavailable),
                    NackChannelAction::Divert(OrderedRelayDiversionReason::Unreachable),
                );
            }
            let me = services.node_identity.current();
            if snapshot.owner == me {
                return (None, NackChannelAction::Forget);
            }
            if snapshot.owner != *previous_owner {
                tracing::debug!(
                    entity_id = %target_entity.id,
                    previous_owner = %previous_owner.node_id,
                    refreshed_owner = %snapshot.owner.node_id,
                    "ordered relay: target-owner changed after retry window; suppressing client fallback"
                );
                return (
                    Some(definite_no_effect_outcome(is_iq)),
                    NackChannelAction::Forget,
                );
            }
            (
                Some(definite_no_effect_outcome(is_iq)),
                NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner),
            )
        }
        OrderedRelayNackReason::TargetUnavailable => (
            Some(FullJidDeliveryOutcome::Unavailable),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
        ),
        OrderedRelayNackReason::InFlight => (
            Some(FullJidDeliveryOutcome::Dropped),
            NackChannelAction::Keep,
        ),
        OrderedRelayNackReason::MaybeCommitted => (
            Some(FullJidDeliveryOutcome::Dropped),
            NackChannelAction::Divert(OrderedRelayDiversionReason::MaybeCommitted),
        ),
        OrderedRelayNackReason::Diverted(ref diversion)
            if diversion.reason == OrderedRelayDiversionReason::MaybeCommitted =>
        {
            (
                Some(FullJidDeliveryOutcome::Dropped),
                NackChannelAction::Divert(OrderedRelayDiversionReason::MaybeCommitted),
            )
        }
        OrderedRelayNackReason::Diverted(_) => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
        ),
        OrderedRelayNackReason::Unreachable => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
        ),
        OrderedRelayNackReason::Gap { .. }
        | OrderedRelayNackReason::ParseFailure
        | OrderedRelayNackReason::Backpressure => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NackChannelAction {
    Divert(OrderedRelayDiversionReason),
    Forget,
    Keep,
}

fn definite_no_effect_outcome(is_iq: bool) -> FullJidDeliveryOutcome {
    if is_iq {
        FullJidDeliveryOutcome::Unavailable
    } else {
        FullJidDeliveryOutcome::Dropped
    }
}

fn replies_for_origin_handoff(stanza: &Stanza, outcome: FullJidDeliveryOutcome) -> Vec<Stanza> {
    match outcome {
        FullJidDeliveryOutcome::Unavailable => {
            crate::server::routes::interpret::fallback_reply_for_undeliverable_iq(stanza)
                .into_iter()
                .collect()
        }
        FullJidDeliveryOutcome::Delivered
        | FullJidDeliveryOutcome::QueuedDetached
        | FullJidDeliveryOutcome::Dropped => Vec::new(),
    }
}

fn diversion_reason_for_nack(nack: &OrderedRelayNack) -> OrderedRelayDiversionReason {
    match &nack.reason {
        OrderedRelayNackReason::Gap { .. }
        | OrderedRelayNackReason::ParseFailure
        | OrderedRelayNackReason::Diverted(_) => OrderedRelayDiversionReason::OrderingGap,
        OrderedRelayNackReason::NotOwner { .. } => OrderedRelayDiversionReason::NotOwner,
        OrderedRelayNackReason::Unreachable | OrderedRelayNackReason::TargetUnavailable => {
            OrderedRelayDiversionReason::Unreachable
        }
        OrderedRelayNackReason::InFlight | OrderedRelayNackReason::Backpressure => {
            OrderedRelayDiversionReason::Backpressure
        }
        OrderedRelayNackReason::MaybeCommitted => OrderedRelayDiversionReason::MaybeCommitted,
    }
}

fn diversion_reason_for_ask_error(error: &RelayAskError) -> OrderedRelayDiversionReason {
    match error {
        RelayAskError::Send {
            failure: RelaySendFailure::MailboxFull,
            ..
        } => OrderedRelayDiversionReason::Backpressure,
        RelayAskError::Send { .. } | RelayAskError::NotFound { .. } | RelayAskError::Cancelled => {
            OrderedRelayDiversionReason::Unreachable
        }
    }
}

fn ask_error_allows_target_refresh(error: &RelayAskError) -> bool {
    match error {
        RelayAskError::NotFound { .. } => true,
        RelayAskError::Send {
            effect: RelaySendEffect::NoEffect,
            ..
        } => true,
        RelayAskError::Cancelled
        | RelayAskError::Send {
            effect: RelaySendEffect::MaybeCommitted,
            ..
        } => false,
    }
}

fn outcome_for_ask_error(error: &RelayAskError, is_iq: bool) -> FullJidDeliveryOutcome {
    tracing::warn!(
        ?error,
        "ordered relay: remote ask failed; classifying for client fallback"
    );
    match error {
        RelayAskError::NotFound { .. } => definite_no_effect_outcome(is_iq),
        RelayAskError::Cancelled => FullJidDeliveryOutcome::Dropped,
        RelayAskError::Send { effect, .. } => match effect {
            RelaySendEffect::NoEffect => definite_no_effect_outcome(is_iq),
            RelaySendEffect::MaybeCommitted => FullJidDeliveryOutcome::Dropped,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::ordered_relay::OrderedRelaySequence;
    use kameo::actor::Spawn;
    use waddle_xmpp::ownership::{ClaimEpoch, ClaimError, InProcessClaimStore, NodeIdentity};
    use xmpp_parsers::message::{Lang, Message};

    struct StaticNodeLease {
        origin: NodeIdentity,
        peer_id: String,
    }

    #[async_trait::async_trait]
    impl NodeLeaseStore for StaticNodeLease {
        async fn register(
            &self,
            _me: &NodeIdentity,
            _pod_template_hash: Option<String>,
        ) -> Result<(), ClaimError> {
            Ok(())
        }

        async fn peer_id_for_node(
            &self,
            node: &NodeIdentity,
        ) -> Result<Option<String>, ClaimError> {
            if node == &self.origin {
                Ok(Some(self.peer_id.clone()))
            } else {
                Ok(None)
            }
        }

        async fn heartbeat(
            &self,
            _me: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<bool, ClaimError> {
            Ok(true)
        }

        async fn expire(
            &self,
            _owner: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<bool, ClaimError> {
            Ok(true)
        }

        async fn mark_draining(&self, _me: &NodeIdentity) -> Result<(), ClaimError> {
            Ok(())
        }

        async fn count_other_live_nodes(
            &self,
            _me: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<usize, ClaimError> {
            Ok(0)
        }

        async fn reconcile(
            &self,
            _me: &NodeIdentity,
            _locally_owned: &[Entity],
        ) -> Result<Vec<Entity>, ClaimError> {
            Ok(Vec::new())
        }

        async fn report_steal_intent(
            &self,
            _entity: &Entity,
            _reporter: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            Ok(())
        }

        async fn owner_steal_intents(
            &self,
            _me: &NodeIdentity,
        ) -> Result<Vec<(Entity, ClaimEpoch)>, ClaimError> {
            Ok(Vec::new())
        }

        async fn clear_steal_intent(
            &self,
            _entity: &Entity,
            _me: &NodeIdentity,
            _mine: ClaimEpoch,
        ) -> Result<u64, ClaimError> {
            Ok(0)
        }

        async fn list_orphaned_sm_session_claims(
            &self,
        ) -> Result<Vec<crate::clustering::claims::OrphanedSmSessionClaim>, ClaimError> {
            Ok(Vec::new())
        }

        async fn current_generation(&self) -> Result<Option<String>, ClaimError> {
            Ok(None)
        }
    }

    fn origin_identity() -> NodeIdentity {
        NodeIdentity::new("origin-node", "origin-epoch")
    }

    fn receiver_identity() -> NodeIdentity {
        NodeIdentity::new("receiver-node", "receiver-epoch")
    }

    fn other_identity() -> NodeIdentity {
        NodeIdentity::new("other-node", "other-epoch")
    }

    fn origin_entity() -> Entity {
        Entity::new(EntityType::SmSession, "stream-1")
    }

    fn target_bare() -> jid::BareJid {
        "juliet@example.test".parse().expect("bare jid")
    }

    fn target_full() -> jid::FullJid {
        "juliet@example.test/phone".parse().expect("full jid")
    }

    fn target_entity() -> Entity {
        user_entity(&target_bare())
    }

    fn message_payload() -> OrderedRelayPayload {
        let full = target_full();
        let mut message = Message::new(Some(jid::Jid::from(full.clone())));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message
            .bodies
            .insert(Lang::new(), "hello from remote".to_string());
        OrderedRelayPayload::Message {
            recipient: jid::Jid::from(full),
            stanza: RemoteStanza(Stanza::Message(message)),
        }
    }

    fn iq_payload() -> OrderedRelayPayload {
        let full = target_full();
        let mut iq = xmpp_parsers::iq::Iq::from_get("iq-1", xmpp_parsers::ping::Ping);
        *iq.to_mut() = Some(jid::Jid::from(full.clone()));
        OrderedRelayPayload::Iq {
            recipient: jid::Jid::from(full),
            stanza: RemoteStanza(Stanza::Iq(Box::new(iq))),
        }
    }

    #[test]
    fn full_jid_bridge_rejects_groupchat_payloads() {
        let target = target_full();
        let mut message = Message::new(Some(jid::Jid::from(target.clone())));
        message.type_ = xmpp_parsers::message::MessageType::Groupchat;

        assert!(payload_for_recipient(jid::Jid::from(target), &Stanza::Message(message)).is_none());
    }

    fn envelope() -> RemoteStanzaEnvelope {
        RemoteStanzaEnvelope {
            asserted_origin_node: NodeId::new(origin_identity().node_id),
            channel: OrderedRelayChannel {
                origin: OrderedRelayOrigin::SmSession(
                    waddle_xmpp::pending_delivery::SmSessionId::new("stream-1"),
                ),
                recipient: OrderedRelayRecipient::FullJid(target_full()),
                target_epoch: waddle_xmpp::ownership::ClaimEpoch(0),
            },
            sequence: OrderedRelaySequence::FIRST,
            origin_inbound_sequence: OriginInboundSequence(1),
            origin_claim: OrderedRelayClaim {
                entity: origin_entity(),
                epoch: waddle_xmpp::ownership::ClaimEpoch(0),
            },
            target_claim: OrderedRelayClaim {
                entity: target_entity(),
                epoch: waddle_xmpp::ownership::ClaimEpoch(0),
            },
            payload: message_payload(),
            origin_proof: None,
        }
    }

    fn sign_envelope(
        mut envelope: RemoteStanzaEnvelope,
        keypair: &Keypair,
    ) -> RemoteStanzaEnvelope {
        let signing_bytes = envelope.signing_bytes().expect("signing bytes");
        envelope.origin_proof = Some(OrderedRelayOriginProof {
            public_key: keypair.public().encode_protobuf(),
            signature: keypair.sign(&signing_bytes).expect("sign envelope"),
        });
        envelope
    }

    fn signed_envelope(keypair: &Keypair) -> RemoteStanzaEnvelope {
        sign_envelope(envelope(), keypair)
    }

    async fn services_with_claims(
        origin_owner: NodeIdentity,
        target_owner: NodeIdentity,
        receiver: NodeIdentity,
        origin_peer_id: String,
    ) -> OrderedRelayDeliveryServices {
        let store = Arc::new(InProcessClaimStore::new());
        store
            .acquire(&origin_entity(), &origin_owner)
            .await
            .expect("origin claim");
        store
            .acquire(&target_entity(), &target_owner)
            .await
            .expect("target claim");
        OrderedRelayDeliveryServices {
            claim_store: store,
            node_lease: Arc::new(StaticNodeLease {
                origin: origin_owner,
                peer_id: origin_peer_id,
            }),
            node_identity: SharedNodeIdentity::new(receiver),
            connection_registry: Arc::new(ConnectionRegistry::new()),
            user_registry: UserRegistryActor::spawn(UserRegistryActor::new()),
            sm_session_registry: Arc::new(InMemorySmSessionRegistry::new()),
            web_socket_state: Weak::new(),
        }
    }

    #[tokio::test]
    async fn unwired_bridge_reports_unreachable_without_advancing_effects() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );

        let err = bridge
            .deliver_reserved(&envelope())
            .await
            .expect_err("unwired bridge cannot deliver");

        assert_eq!(err, OrderedRelayNackReason::Unreachable);
    }

    #[tokio::test]
    async fn receiver_rejects_target_claim_not_owned_by_this_node() {
        let keypair = Keypair::generate_ed25519();
        let services = services_with_claims(
            origin_identity(),
            other_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;

        let err = validate_claims(&services, &signed_envelope(&keypair))
            .await
            .expect_err("foreign target owner is not this relay");

        assert_eq!(
            err,
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Target
            }
        );
    }

    #[tokio::test]
    async fn receiver_accepts_fresh_origin_and_local_target_claims() {
        let keypair = Keypair::generate_ed25519();
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;

        validate_claims(&services, &signed_envelope(&keypair))
            .await
            .expect("claims match origin and receiver");
    }

    #[tokio::test]
    async fn receiver_nacks_iq_without_live_resource_instead_of_detached_queue() {
        let keypair = Keypair::generate_ed25519();
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::new(services));
        let mut envelope = envelope();
        envelope.payload = iq_payload();
        let envelope = sign_envelope(envelope, &keypair);

        let err = bridge
            .deliver_reserved(&envelope)
            .await
            .expect_err("remote full-JID IQ requires a live local resource");

        assert_eq!(err, OrderedRelayNackReason::TargetUnavailable);
    }

    #[tokio::test]
    async fn receiver_delivers_full_jid_iq_as_peer_stanza() {
        let keypair = Keypair::generate_ed25519();
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        services
            .user_registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: target_full(),
                entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
            })
            .await
            .expect("register resource");
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::new(services));
        let mut envelope = envelope();
        envelope.payload = iq_payload();
        let envelope = sign_envelope(envelope, &keypair);

        bridge
            .deliver_reserved(&envelope)
            .await
            .expect("remote full-JID IQ delivers to live resource");

        let outbound = rx.try_recv().expect("queued outbound stanza");
        assert_eq!(
            outbound.kind,
            waddle_xmpp::registry::DeliveryKind::PeerStanza
        );
    }

    #[tokio::test]
    async fn receiver_nacks_dropped_peer_delivery_instead_of_acknowledging_loss() {
        let keypair = Keypair::generate_ed25519();
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(waddle_xmpp::registry::OutboundStanza::peer_stanza(
            Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
        ))
        .expect("seed outbound channel to capacity");
        services
            .user_registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: target_full(),
                entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
            })
            .await
            .expect("register resource");
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::new(services));
        let envelope = sign_envelope(envelope(), &keypair);

        let err = bridge
            .deliver_reserved(&envelope)
            .await
            .expect_err("full outbound channel must not be ACKed as delivered");

        assert_eq!(err, OrderedRelayNackReason::Backpressure);
    }

    #[tokio::test]
    async fn origin_not_owner_nack_is_terminal_provenance_failure() {
        let services = services_with_claims(
            origin_identity(),
            other_identity(),
            origin_identity(),
            "origin-peer".to_string(),
        )
        .await;
        let nack = OrderedRelayNack {
            channel: envelope().channel,
            sequence: OrderedRelaySequence::FIRST,
            reason: OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Origin,
            },
        };

        let (iq_outcome, iq_action) = outcome_for_nack(
            &services,
            &target_entity(),
            &receiver_identity(),
            &nack,
            true,
        )
        .await;
        let (message_outcome, message_action) = outcome_for_nack(
            &services,
            &target_entity(),
            &receiver_identity(),
            &nack,
            false,
        )
        .await;

        assert_eq!(iq_outcome, Some(FullJidDeliveryOutcome::Unavailable));
        assert_eq!(message_outcome, Some(FullJidDeliveryOutcome::Dropped));
        assert_eq!(
            iq_action,
            NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner)
        );
        assert_eq!(
            message_action,
            NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner)
        );
    }

    #[tokio::test]
    async fn maybe_committed_diversion_suppresses_iq_fallback_on_replay() {
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            "origin-peer".to_string(),
        )
        .await;
        let channel = envelope().channel;
        let nack = OrderedRelayNack {
            channel: channel.clone(),
            sequence: OrderedRelaySequence::FIRST,
            reason: OrderedRelayNackReason::Diverted(OrderedRelayDiversion {
                channel,
                reason: OrderedRelayDiversionReason::MaybeCommitted,
            }),
        };

        let (outcome, action) = outcome_for_nack(
            &services,
            &target_entity(),
            &receiver_identity(),
            &nack,
            true,
        )
        .await;

        assert_eq!(outcome, Some(FullJidDeliveryOutcome::Dropped));
        assert_eq!(
            action,
            NackChannelAction::Divert(OrderedRelayDiversionReason::MaybeCommitted)
        );
    }

    #[tokio::test]
    async fn failed_ordered_delivery_sticky_diverts_channel_instead_of_rewinding_sequence() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let channel = envelope().channel;
        {
            let mut sender = bridge.sender_state.lock().await;
            let first = sender
                .next_envelope(
                    NodeId::new(origin_identity().node_id),
                    channel.clone(),
                    OriginInboundSequence(1),
                    OrderedRelayClaim {
                        entity: origin_entity(),
                        epoch: ClaimEpoch(0),
                    },
                    OrderedRelayClaim {
                        entity: target_entity(),
                        epoch: ClaimEpoch(0),
                    },
                    message_payload(),
                )
                .expect("first envelope allocates");
            assert_eq!(first.sequence, OrderedRelaySequence::FIRST);
        }

        let nack = OrderedRelayNack {
            channel: channel.clone(),
            sequence: OrderedRelaySequence::FIRST,
            reason: OrderedRelayNackReason::TargetUnavailable,
        };
        bridge
            .divert_channel(channel.clone(), diversion_reason_for_nack(&nack))
            .await;

        let diverted = bridge
            .sender_state
            .lock()
            .await
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                channel,
                OriginInboundSequence(2),
                OrderedRelayClaim {
                    entity: origin_entity(),
                    epoch: ClaimEpoch(0),
                },
                OrderedRelayClaim {
                    entity: target_entity(),
                    epoch: ClaimEpoch(0),
                },
                message_payload(),
            )
            .expect_err("later sends must not restart at sequence one");
        assert_eq!(diverted.reason, OrderedRelayDiversionReason::Unreachable);
    }

    #[tokio::test]
    async fn not_owner_nack_clears_sender_channel_for_refreshed_owner_retry() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let channel = envelope().channel;
        {
            let mut sender = bridge.sender_state.lock().await;
            let first = sender
                .next_envelope(
                    NodeId::new(origin_identity().node_id),
                    channel.clone(),
                    OriginInboundSequence(1),
                    OrderedRelayClaim {
                        entity: origin_entity(),
                        epoch: ClaimEpoch(0),
                    },
                    OrderedRelayClaim {
                        entity: target_entity(),
                        epoch: ClaimEpoch(0),
                    },
                    message_payload(),
                )
                .expect("first envelope allocates");
            assert_eq!(first.sequence, OrderedRelaySequence::FIRST);
        }

        bridge
            .apply_nack_channel_action(channel.clone(), NackChannelAction::Forget)
            .await;

        let refreshed_channel = OrderedRelayChannel {
            target_epoch: ClaimEpoch(1),
            ..channel
        };
        let retried = bridge
            .sender_state
            .lock()
            .await
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                refreshed_channel,
                OriginInboundSequence(2),
                OrderedRelayClaim {
                    entity: origin_entity(),
                    epoch: ClaimEpoch(0),
                },
                OrderedRelayClaim {
                    entity: target_entity(),
                    epoch: ClaimEpoch(1),
                },
                message_payload(),
            )
            .expect("not-owner no-effect path must allow refreshed-owner retry");
        assert_eq!(retried.sequence, OrderedRelaySequence::FIRST);
    }

    #[tokio::test]
    async fn same_owner_target_not_owner_nack_diverts_rejected_channel() {
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            origin_identity(),
            "origin-peer".to_string(),
        )
        .await;
        let nack = OrderedRelayNack {
            channel: envelope().channel,
            sequence: OrderedRelaySequence(5),
            reason: OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Target,
            },
        };
        let (outcome, action) = outcome_for_nack(
            &services,
            &target_entity(),
            &receiver_identity(),
            &nack,
            true,
        )
        .await;
        assert_eq!(outcome, Some(FullJidDeliveryOutcome::Unavailable));
        assert_eq!(
            action,
            NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner)
        );

        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let channel = envelope().channel;
        bridge
            .apply_nack_channel_action(channel.clone(), action)
            .await;

        let diverted = bridge
            .sender_state
            .lock()
            .await
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                channel,
                OriginInboundSequence(6),
                OrderedRelayClaim {
                    entity: origin_entity(),
                    epoch: ClaimEpoch(0),
                },
                OrderedRelayClaim {
                    entity: target_entity(),
                    epoch: ClaimEpoch(1),
                },
                message_payload(),
            )
            .expect_err("same-owner claim churn must divert the rejected channel");
        assert_eq!(diverted.reason, OrderedRelayDiversionReason::NotOwner);
    }

    #[test]
    fn handoff_completion_synthesizes_iq_fallback_only_for_unavailable() {
        let mut iq = xmpp_parsers::iq::Iq::from_get("iq-1", xmpp_parsers::ping::Ping);
        *iq.to_mut() = Some(jid::Jid::from(target_full()));
        let stanza = Stanza::Iq(Box::new(iq));

        assert_eq!(
            replies_for_origin_handoff(&stanza, FullJidDeliveryOutcome::Delivered).len(),
            0
        );
        assert_eq!(
            replies_for_origin_handoff(&stanza, FullJidDeliveryOutcome::Dropped).len(),
            0
        );
        assert_eq!(
            replies_for_origin_handoff(&stanza, FullJidDeliveryOutcome::Unavailable).len(),
            1
        );
    }

    #[test]
    fn iq_ask_error_classifier_falls_back_only_for_definite_no_effect_failures() {
        let not_found = RelayAskError::NotFound {
            node_id: NodeId::new("missing-node".to_string()),
        };
        assert!(ask_error_allows_target_refresh(&not_found));
        assert_eq!(
            outcome_for_ask_error(&not_found, true),
            FullJidDeliveryOutcome::Unavailable
        );
        let mailbox_full = RelayAskError::Send {
            failure: RelaySendFailure::MailboxFull,
            effect: RelaySendEffect::NoEffect,
            message: "mailbox full".to_string(),
        };
        assert!(ask_error_allows_target_refresh(&mailbox_full));
        assert_eq!(
            outcome_for_ask_error(&mailbox_full, true),
            FullJidDeliveryOutcome::Unavailable
        );
        assert_eq!(
            diversion_reason_for_ask_error(&mailbox_full),
            OrderedRelayDiversionReason::Backpressure
        );
        let stale_ref = RelayAskError::Send {
            failure: RelaySendFailure::StaleRef,
            effect: RelaySendEffect::NoEffect,
            message: "actor not running before enqueue".to_string(),
        };
        assert!(ask_error_allows_target_refresh(&stale_ref));
        assert_eq!(
            outcome_for_ask_error(&stale_ref, true),
            FullJidDeliveryOutcome::Unavailable
        );
        let reply_timeout = RelayAskError::Send {
            failure: RelaySendFailure::ReplyTimeout,
            effect: RelaySendEffect::MaybeCommitted,
            message: "reply timeout".to_string(),
        };
        assert!(!ask_error_allows_target_refresh(&reply_timeout));
        assert_eq!(
            outcome_for_ask_error(&reply_timeout, true),
            FullJidDeliveryOutcome::Dropped
        );
        let codec_after_handler = RelayAskError::Send {
            failure: RelaySendFailure::Codec,
            effect: RelaySendEffect::MaybeCommitted,
            message: "reply codec failed after handler".to_string(),
        };
        assert!(!ask_error_allows_target_refresh(&codec_after_handler));
        assert_eq!(
            outcome_for_ask_error(&codec_after_handler, true),
            FullJidDeliveryOutcome::Dropped
        );
        assert!(!ask_error_allows_target_refresh(&RelayAskError::Cancelled));
        assert_eq!(
            diversion_reason_for_ask_error(&RelayAskError::Cancelled),
            OrderedRelayDiversionReason::Unreachable
        );
    }
}
