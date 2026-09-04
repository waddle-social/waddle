use super::types::{MucProxyRouteAttempt, MucProxyRouteDecision, OrderedRelayMucProxyOutcome};
use super::*;
use waddle_xmpp::ingress::RelayTargetIdentity;

fn relay_target_identity_from_owner(
    owner: &waddle_xmpp::ownership::NodeIdentity,
) -> RelayTargetIdentity {
    RelayTargetIdentity::owner_node(owner.node_id.clone(), owner.node_epoch.clone())
}

fn room_fence_from_remote_delivery(
    outcome: &RemoteDeliveryOutcome,
) -> Option<waddle_xmpp::muc::RoomClaimFenceContext> {
    let owner = outcome.relay_target.as_ref()?;
    let target_claim = outcome.target_claim.as_ref()?;
    Some(waddle_xmpp::muc::RoomClaimFenceContext::new(
        target_claim.entity.clone(),
        owner.clone(),
        target_claim.epoch,
    ))
}

impl OrderedRelayDeliveryBridge {
    /// Return `Some` only when this room is currently owned by a fresh
    /// foreign `RoomActor` claim and an ordered-relay MUC proxy send was
    /// attempted. `None` means the caller must keep the existing local room
    /// path.
    pub(crate) async fn try_proxy_muc_remote(
        self: &Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        kind: OrderedRelayMucProxyKind,
        muc_origin: MucProxyOrigin,
        origin: &OrderedRelayRouteOrigin,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        self.try_proxy_muc_remote_decision(room_jid, stanza, kind, muc_origin, origin)
            .await
            .into_attempted()
    }

    /// Typed variant of [`Self::try_proxy_muc_remote`] (#1249): reports
    /// WHY no relay was attempted instead of a flat `None`, so the
    /// disconnect-cleanup path can converge (forget vs retry) per case.
    pub(crate) async fn try_proxy_muc_remote_decision(
        self: &Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        kind: OrderedRelayMucProxyKind,
        muc_origin: MucProxyOrigin,
        origin: &OrderedRelayRouteOrigin,
    ) -> MucProxyRouteDecision {
        if let Some(remote_origin) = remote_resource_origin(origin) {
            return match Arc::clone(self)
                .route_remote_resource_origin_muc(
                    remote_origin,
                    RemoteResourceRouteTarget::MucProxy {
                        room_jid: room_jid.clone(),
                        kind,
                        origin: muc_origin,
                        stanza: RemoteStanza(stanza.clone()),
                    },
                    stanza,
                    origin,
                )
                .await
            {
                Some(attempt) => MucProxyRouteDecision::Attempted(attempt),
                // Only `services.get()` misses produce `None` on the
                // remote-resource path — the bridge is not wired yet.
                None => MucProxyRouteDecision::RoomClaimUnavailable,
            };
        }
        self.try_proxy_muc_remote_from_local_origin_decision(
            room_jid, stanza, kind, muc_origin, origin,
        )
        .await
    }

    pub(in super::super) async fn try_proxy_muc_remote_from_local_origin_decision(
        self: &Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        kind: OrderedRelayMucProxyKind,
        muc_origin: MucProxyOrigin,
        origin: &OrderedRelayRouteOrigin,
    ) -> MucProxyRouteDecision {
        let Some(services) = self.services.get().cloned() else {
            return MucProxyRouteDecision::RoomClaimUnavailable;
        };
        let target_entity = room_entity(room_jid);
        // Distinguish "no claim row exists" (definitive: no live
        // RoomActor anywhere, nothing to relay to) from "claim lookup
        // errored" (retryable) — `current_claim` conflates them (#1249).
        let target_snapshot = match services.claim_store.current_claim(&target_entity).await {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return MucProxyRouteDecision::RoomUnclaimed,
            Err(error) => {
                tracing::warn!(
                    entity = %target_entity,
                    %error,
                    "ordered relay: claim lookup failed"
                );
                return MucProxyRouteDecision::RoomClaimUnavailable;
            }
        };
        if !target_snapshot.owner_lease_fresh {
            return MucProxyRouteDecision::RoomClaimUnavailable;
        }
        let me = services.node_identity.current();
        if target_snapshot.owner == me {
            return MucProxyRouteDecision::LocalRoom;
        }
        let relay_target = relay_target_identity_from_owner(&target_snapshot.owner);
        let initial_room_fence = waddle_xmpp::muc::RoomClaimFenceContext::new(
            target_entity.clone(),
            target_snapshot.owner.clone(),
            target_snapshot.claim_epoch,
        );

        let (origin_entity, channel_origin) = route_origin_claim(&origin.kind);
        let Some(origin_snapshot) = current_claim(&services, &origin_entity).await else {
            return MucProxyRouteDecision::OriginUnavailable;
        };
        if !origin_snapshot.owner_lease_fresh || origin_snapshot.owner != me {
            tracing::debug!(
                room = %room_jid,
                origin_entity = %origin_entity,
                "ordered relay: MUC origin entity is not currently owned locally; \
                 keeping local fallback path"
            );
            return MucProxyRouteDecision::OriginUnavailable;
        }
        let Some(sender_claim) =
            current_fresh_local_relay_claim(&services, &origin.sender_entity, &me, "sender").await
        else {
            return MucProxyRouteDecision::OriginUnavailable;
        };

        let payload = OrderedRelayPayload::MucProxy {
            room_jid: room_jid.clone(),
            kind,
            origin: muc_origin,
            stanza: RemoteStanza(stanza.clone()),
        };
        let channel = OrderedRelayChannel {
            origin: channel_origin,
            recipient: OrderedRelayRecipient::Room {
                room: room_jid.clone(),
                lane: kind.room_lane(),
            },
            target_epoch: target_snapshot.claim_epoch,
        };
        let retry_channel = channel.clone();
        let previous_owner = target_snapshot.owner.clone();
        let origin_claim = OrderedRelayClaim {
            entity: origin_entity,
            epoch: origin_snapshot.claim_epoch,
        };
        let target_claim = OrderedRelayClaim {
            entity: room_entity(room_jid),
            epoch: target_snapshot.claim_epoch,
        };
        let seed = RemoteDeliverySeed {
            services: services.clone(),
            target_entity: target_entity.clone(),
            previous_owner: previous_owner.clone(),
            channel,
            asserted_origin_node: NodeId::new(me.node_id.clone()),
            origin_inbound_sequence: OriginInboundSequence(origin.inbound_sequence),
            origin_claim: origin_claim.clone(),
            sender_claim: sender_claim.clone(),
            target_claim: target_claim.clone(),
            payload: payload.clone(),
            target: jid::Jid::from(room_jid.clone()),
            stanza: stanza.clone(),
            is_iq: matches!(stanza, Stanza::Iq(_)),
        };

        let Some(outcome) = Arc::clone(self).deliver_seeded_remote(seed, true).await else {
            // `deliver_seeded_remote` yields `None` for more than the
            // benign "target claim refreshed to local ownership" case —
            // notably a `RelayAskError::NotFound` after a transient
            // owner-lookup miss while the room claim lease is still
            // fresh (the envelope was rolled back, provably never
            // delivered). Classify RETRYABLE (race review P2 on PR
            // #1277): the cleanup janitor re-drives it, and if the room
            // truly became local the next attempt classifies
            // `LocalRoom` and the local sweep converges the occupancy.
            return MucProxyRouteDecision::RoomClaimUnavailable;
        };
        if outcome.maybe_committed {
            if kind == OrderedRelayMucProxyKind::JoinPresence && outcome.join_repair_allowed {
                self.forget_channel(&retry_channel).await;
                let retry = Arc::clone(self)
                    .deliver_seeded_remote(
                        RemoteDeliverySeed {
                            services: services.clone(),
                            target_entity: target_entity.clone(),
                            previous_owner: previous_owner.clone(),
                            channel: retry_channel,
                            asserted_origin_node: NodeId::new(me.node_id.clone()),
                            origin_inbound_sequence: OriginInboundSequence(origin.inbound_sequence),
                            origin_claim: origin_claim.clone(),
                            sender_claim: sender_claim.clone(),
                            target_claim: target_claim.clone(),
                            payload: payload.clone(),
                            target: jid::Jid::from(room_jid.clone()),
                            stanza: stanza.clone(),
                            is_iq: false,
                        },
                        false,
                    )
                    .await;
                if let Some(retry) = retry.filter(|retry| !retry.maybe_committed) {
                    match retry.delivery {
                        FullJidDeliveryOutcome::Delivered
                        | FullJidDeliveryOutcome::QueuedDetached => {
                            return MucProxyRouteDecision::Attempted(MucProxyRouteAttempt {
                                relay_target: retry
                                    .relay_target
                                    .as_ref()
                                    .map(relay_target_identity_from_owner),
                                room_fence: room_fence_from_remote_delivery(&retry),
                                outcome: OrderedRelayMucProxyOutcome::Delivered(
                                    retry.client_replies,
                                ),
                            });
                        }
                        FullJidDeliveryOutcome::Unavailable
                        | FullJidDeliveryOutcome::Dropped
                        | FullJidDeliveryOutcome::MaybeCommitted => {}
                    }
                }
                if let Some(repair) = Arc::clone(self)
                    .try_proxy_muc_join_repair(room_jid, stanza, muc_origin, origin)
                    .await
                {
                    if !repair.maybe_committed {
                        match repair.delivery {
                            FullJidDeliveryOutcome::Delivered
                            | FullJidDeliveryOutcome::QueuedDetached => {
                                return MucProxyRouteDecision::Attempted(MucProxyRouteAttempt {
                                    relay_target: repair
                                        .relay_target
                                        .as_ref()
                                        .map(relay_target_identity_from_owner),
                                    room_fence: room_fence_from_remote_delivery(&repair),
                                    outcome: OrderedRelayMucProxyOutcome::Delivered(
                                        repair.client_replies,
                                    ),
                                });
                            }
                            FullJidDeliveryOutcome::Unavailable
                            | FullJidDeliveryOutcome::Dropped
                            | FullJidDeliveryOutcome::MaybeCommitted => {}
                        }
                    }
                }
                return MucProxyRouteDecision::Attempted(MucProxyRouteAttempt {
                    relay_target: Some(relay_target.clone()),
                    room_fence: Some(initial_room_fence.clone()),
                    outcome: OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
                });
            }
            // A `MujiJingleIq` maybe-committed deliberately gets the
            // same treatment as every other kind: the channel keeps
            // its diversion. An earlier attempt to `forget_channel`
            // here was WRONG and is recorded so it is not retried —
            // `OrderedRelaySenderState::forget_channel` drops
            // `next_by_channel`, resetting the SENDER's sequence to
            // FIRST while the receiver's `next_expected` is untouched.
            // The next envelope on the channel (the user's next
            // groupchat message or leave presence) would then arrive
            // with a stale sequence and NACK as an ordering gap,
            // converting a bounded failure into a diversion attached
            // to unrelated MUC traffic. Only the `JoinPresence` arm
            // above can forget safely, because it immediately re-sends
            // and has `try_proxy_muc_join_repair` as a backstop.
            return MucProxyRouteDecision::Attempted(MucProxyRouteAttempt {
                relay_target: Some(relay_target.clone()),
                room_fence: Some(initial_room_fence),
                outcome: OrderedRelayMucProxyOutcome::MaybeCommitted,
            });
        }

        let final_relay_target = outcome
            .relay_target
            .as_ref()
            .map(relay_target_identity_from_owner);
        let room_fence = room_fence_from_remote_delivery(&outcome);
        MucProxyRouteDecision::Attempted(MucProxyRouteAttempt {
            relay_target: final_relay_target,
            room_fence,
            outcome: match outcome.delivery {
                FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                    OrderedRelayMucProxyOutcome::Delivered(outcome.client_replies)
                }
                FullJidDeliveryOutcome::Unavailable => OrderedRelayMucProxyOutcome::Unavailable,
                FullJidDeliveryOutcome::Dropped => OrderedRelayMucProxyOutcome::Dropped,
                FullJidDeliveryOutcome::MaybeCommitted => {
                    if kind == OrderedRelayMucProxyKind::JoinPresence {
                        OrderedRelayMucProxyOutcome::JoinMaybeCommitted
                    } else {
                        OrderedRelayMucProxyOutcome::MaybeCommitted
                    }
                }
            },
        })
    }

    pub(super) async fn try_proxy_muc_join_repair(
        self: Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        muc_origin: MucProxyOrigin,
        original_origin: &OrderedRelayRouteOrigin,
    ) -> Option<RemoteDeliveryOutcome> {
        let Stanza::Presence(presence) = stanza else {
            return None;
        };
        let sender_jid = presence
            .from
            .as_ref()
            .and_then(|jid| jid.clone().try_into_full().ok())?;
        let services = self.services.get()?.clone();
        let target_entity = room_entity(room_jid);
        let target_snapshot = current_claim(&services, &target_entity).await?;
        if !target_snapshot.owner_lease_fresh {
            return None;
        }
        let me = services.node_identity.current();
        if target_snapshot.owner == me {
            return None;
        }

        let repair_origin_entity = user_entity(&sender_jid.to_bare());
        let (original_origin_entity, _) = route_origin_claim(&original_origin.kind);
        if repair_origin_entity == original_origin_entity {
            return None;
        }
        let origin_snapshot = current_claim(&services, &repair_origin_entity).await?;
        if !origin_snapshot.owner_lease_fresh || origin_snapshot.owner != me {
            tracing::warn!(
                room = %room_jid,
                sender = %sender_jid,
                origin_entity = %repair_origin_entity,
                "ordered relay: cannot repair maybe-committed MUC join because \
                 UserActor origin is not owned locally"
            );
            return None;
        }
        let sender_claim = OrderedRelayClaim {
            entity: repair_origin_entity.clone(),
            epoch: origin_snapshot.claim_epoch,
        };

        tracing::warn!(
            room = %room_jid,
            sender = %sender_jid,
            "ordered relay: retrying maybe-committed MUC join on UserActor repair channel"
        );
        let payload = OrderedRelayPayload::MucProxy {
            room_jid: room_jid.clone(),
            kind: OrderedRelayMucProxyKind::JoinPresence,
            origin: muc_origin,
            stanza: RemoteStanza(stanza.clone()),
        };
        let channel = OrderedRelayChannel {
            origin: OrderedRelayOrigin::Entity(repair_origin_entity.clone()),
            recipient: OrderedRelayRecipient::Room {
                room: room_jid.clone(),
                lane: OrderedRelayMucProxyKind::JoinPresence.room_lane(),
            },
            target_epoch: target_snapshot.claim_epoch,
        };
        let seed = RemoteDeliverySeed {
            services,
            target_entity,
            previous_owner: target_snapshot.owner,
            channel,
            asserted_origin_node: NodeId::new(me.node_id.clone()),
            origin_inbound_sequence: OriginInboundSequence(original_origin.inbound_sequence),
            origin_claim: OrderedRelayClaim {
                entity: repair_origin_entity,
                epoch: origin_snapshot.claim_epoch,
            },
            sender_claim,
            target_claim: OrderedRelayClaim {
                entity: room_entity(room_jid),
                epoch: target_snapshot.claim_epoch,
            },
            payload,
            target: jid::Jid::from(room_jid.clone()),
            stanza: stanza.clone(),
            is_iq: false,
        };
        self.deliver_seeded_remote(seed, true).await
    }
}
