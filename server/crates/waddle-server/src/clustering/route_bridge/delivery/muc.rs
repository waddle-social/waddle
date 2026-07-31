use super::*;

#[derive(Debug, Clone)]
pub(crate) enum OrderedRelayMucProxyOutcome {
    Delivered(Vec<Stanza>),
    Unavailable,
    Dropped,
    MaybeCommitted,
    JoinMaybeCommitted,
}

/// Typed routing decision for a MUC proxy attempt (#1249). The old
/// `Option<OrderedRelayMucProxyOutcome>` API collapsed six distinct
/// "no relay attempted" conditions into one `None`, forcing the
/// disconnect-cleanup caller to treat the benign "room claim is locally
/// owned" case (the local room loop handles the leave moments later)
/// exactly like the harmful "origin `UserActor` claim held by another
/// node" case (which recurs whenever the disconnecting user has a
/// second device on another node and previously ghosted the occupant
/// forever). Callers that only need the legacy semantics keep using
/// [`OrderedRelayDeliveryBridge::try_proxy_muc_remote`]; the cleanup
/// path consumes this decision directly.
#[derive(Debug, Clone)]
pub(crate) enum MucProxyRouteDecision {
    /// An ordered-relay send was attempted; the payload is its result.
    Attempted(OrderedRelayMucProxyOutcome),
    /// The room claim is owned by THIS node — the local room path is
    /// authoritative and handles the stanza. Benign for cleanup: the
    /// local `LeaveByRealJid` loop converges the occupancy.
    LocalRoom,
    /// Definitive: no claim row exists for the room, so no node holds a
    /// live `RoomActor` (occupancy is in-memory on the claim owner).
    /// There is no remote occupancy left to clean up.
    RoomUnclaimed,
    /// The room claim could not be used right now: the claim lookup
    /// errored, the owner's lease is stale (owner crash / renewal lag),
    /// or the bridge services are not wired. Retryable.
    RoomClaimUnavailable,
    /// The origin/sender claim needed to sequence the relay is not
    /// usable from this node (typically: the origin `UserActor` claim
    /// is held by the node hosting the user's other device). Retryable;
    /// disconnect cleanup avoids this case up-front by preferring the
    /// remote-resource origin when the socket was registered against a
    /// foreign `UserActor` owner.
    OriginUnavailable,
}

impl MucProxyRouteDecision {
    /// Legacy adapter: `Some(outcome)` iff a relay send was attempted;
    /// `None` means "keep the existing local path" (all non-attempt
    /// variants), exactly matching the pre-#1249 `Option` contract.
    pub(super) fn into_attempted(self) -> Option<OrderedRelayMucProxyOutcome> {
        match self {
            MucProxyRouteDecision::Attempted(outcome) => Some(outcome),
            MucProxyRouteDecision::LocalRoom
            | MucProxyRouteDecision::RoomUnclaimed
            | MucProxyRouteDecision::RoomClaimUnavailable
            | MucProxyRouteDecision::OriginUnavailable => None,
        }
    }
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
        origin: &OrderedRelayRouteOrigin,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        self.try_proxy_muc_remote_decision(room_jid, stanza, kind, origin)
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
        origin: &OrderedRelayRouteOrigin,
    ) -> MucProxyRouteDecision {
        if let Some(remote_origin) = remote_resource_origin(origin) {
            return match Arc::clone(self)
                .route_remote_resource_origin_muc(
                    remote_origin,
                    RemoteResourceRouteTarget::MucProxy {
                        room_jid: room_jid.clone(),
                        kind,
                        stanza: RemoteStanza(stanza.clone()),
                    },
                    stanza,
                    origin,
                )
                .await
            {
                Some(outcome) => MucProxyRouteDecision::Attempted(outcome),
                // Only `services.get()` misses produce `None` on the
                // remote-resource path — the bridge is not wired yet.
                None => MucProxyRouteDecision::RoomClaimUnavailable,
            };
        }
        self.try_proxy_muc_remote_from_local_origin_decision(room_jid, stanza, kind, origin)
            .await
    }

    pub(super) async fn try_proxy_muc_remote_from_local_origin(
        self: &Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        kind: OrderedRelayMucProxyKind,
        origin: &OrderedRelayRouteOrigin,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        self.try_proxy_muc_remote_from_local_origin_decision(room_jid, stanza, kind, origin)
            .await
            .into_attempted()
    }

    pub(super) async fn try_proxy_muc_remote_from_local_origin_decision(
        self: &Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        kind: OrderedRelayMucProxyKind,
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
                            return MucProxyRouteDecision::Attempted(
                                OrderedRelayMucProxyOutcome::Delivered(retry.client_replies),
                            );
                        }
                        FullJidDeliveryOutcome::Unavailable
                        | FullJidDeliveryOutcome::Dropped
                        | FullJidDeliveryOutcome::MaybeCommitted => {}
                    }
                }
                if let Some(repair) = Arc::clone(self)
                    .try_proxy_muc_join_repair(room_jid, stanza, origin)
                    .await
                {
                    if !repair.maybe_committed {
                        match repair.delivery {
                            FullJidDeliveryOutcome::Delivered
                            | FullJidDeliveryOutcome::QueuedDetached => {
                                return MucProxyRouteDecision::Attempted(
                                    OrderedRelayMucProxyOutcome::Delivered(repair.client_replies),
                                );
                            }
                            FullJidDeliveryOutcome::Unavailable
                            | FullJidDeliveryOutcome::Dropped
                            | FullJidDeliveryOutcome::MaybeCommitted => {}
                        }
                    }
                }
                return MucProxyRouteDecision::Attempted(
                    OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
                );
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
            return MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::MaybeCommitted);
        }

        MucProxyRouteDecision::Attempted(match outcome.delivery {
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
        })
    }

    pub(super) async fn try_proxy_muc_join_repair(
        self: Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
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

pub(in super::super) async fn deliver_reserved_muc_proxy(
    services: &OrderedRelayDeliveryServices,
    room_jid: &jid::BareJid,
    kind: OrderedRelayMucProxyKind,
    stanza: &Stanza,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(state) = services.web_socket_state.upgrade() else {
        tracing::warn!(
            room = %room_jid,
            "ordered relay: WebSocket state is gone; cannot deliver MUC relay payload"
        );
        return Err(OrderedRelayNackReason::Unreachable);
    };
    match (kind, stanza) {
        (OrderedRelayMucProxyKind::JoinPresence, Stanza::Presence(presence)) => {
            deliver_reserved_muc_join(state.as_ref(), room_jid, presence).await
        }
        (OrderedRelayMucProxyKind::GroupchatMessage, Stanza::Message(message)) => {
            deliver_reserved_muc_groupchat(state.as_ref(), room_jid, message).await
        }
        (OrderedRelayMucProxyKind::OccupantPresence, Stanza::Presence(presence)) => {
            deliver_reserved_muc_occupant_presence(state.as_ref(), room_jid, presence).await
        }
        (OrderedRelayMucProxyKind::MujiJingleIq, Stanza::Iq(iq)) => {
            deliver_reserved_muji_iq(state.as_ref(), room_jid, iq).await
        }
        _ => Err(OrderedRelayNackReason::ParseFailure),
    }
}

/// #1445: execute a relayed Muji `session-initiate` on this node — the
/// room-claim owner — and carry the reply frames (IQ ack +
/// server-initiated `session-accept` with the LiveKit token) back to
/// the origin node as client replies. Envelope validation has already
/// bound the payload's `<muji room>` to `room_jid`, so the room
/// binding needs no re-check here; the executor re-runs the membership
/// gate against the local room actor and is terminal (a denial comes
/// back as a delivered IQ-error frame, never a NACK, so no relay
/// loop can form).
pub(in super::super) async fn deliver_reserved_muji_iq(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    iq: &xmpp_parsers::iq::Iq,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let frames = tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::websocket::handlers::iq::jingle_muji_relay::handle_relayed_muji_initiate(
            state, iq,
        ),
    )
    .await
    .map_err(|_| {
        tracing::warn!(
            room = %room_jid,
            timeout_ms = ORDERED_RECEIVER_DELIVERY_TIMEOUT.as_millis(),
            "ordered relay: Muji Jingle execution timed out"
        );
        OrderedRelayNackReason::MaybeCommitted
    })?
    .ok_or(OrderedRelayNackReason::ParseFailure)?;
    remote_replies_from_frames(frames)
}

pub(in super::super) async fn deliver_reserved_muc_occupant_presence(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    if presence.type_ == xmpp_parsers::presence::Type::Unavailable {
        return deliver_reserved_muc_leave(state, room_jid, presence).await;
    }
    deliver_reserved_muc_update(state, room_jid, presence).await
}

pub(in super::super) async fn deliver_reserved_muc_join(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(sender_jid) = presence
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(to) = presence.to.as_ref() else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(nick) = to.resource().map(|resource| resource.as_str()) else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    if to.to_bare() != *room_jid {
        return Err(OrderedRelayNackReason::ParseFailure);
    }
    let presence_show = presence
        .show
        .clone()
        .map(crate::notification_activity::NotificationPresenceShow::from_xep0045);
    let synthetic_session = synthetic_session_for_full_jid(&sender_jid);
    let frames = crate::server::routes::websocket::handlers::presence::handle_muc_join(
        state,
        state.deps.auth_state.xmpp_domain.as_str(),
        room_jid,
        &sender_jid,
        nick,
        presence_show,
        &Some(synthetic_session),
    )
    .await;
    remote_replies_from_frames(frames)
}

pub(in super::super) async fn deliver_reserved_muc_update(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(sender_jid) = presence
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(to) = presence.to.as_ref() else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(nick) = to.resource().map(|resource| resource.as_str()) else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    if to.to_bare() != *room_jid {
        return Err(OrderedRelayNackReason::ParseFailure);
    }

    match crate::server::routes::websocket::handlers::presence::try_handle_muc_presence_update(
        state,
        room_jid,
        &sender_jid,
        nick,
        presence,
    )
    .await
    {
        Some(frames) => remote_replies_from_frames(frames),
        None => Err(OrderedRelayNackReason::TargetUnavailable),
    }
}

pub(in super::super) async fn deliver_reserved_muc_leave(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(sender_jid) = presence
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(to) = presence.to.as_ref() else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(nick) = to.resource().map(|resource| resource.as_str()) else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    if to.to_bare() != *room_jid {
        return Err(OrderedRelayNackReason::ParseFailure);
    }

    let frames = crate::server::routes::websocket::handlers::presence::handle_muc_leave(
        state,
        room_jid,
        &sender_jid,
        nick,
        None,
    )
    .await;
    remote_replies_from_frames(frames)
}

pub(in super::super) async fn deliver_reserved_muc_groupchat(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    message: &xmpp_parsers::message::Message,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let synthetic_session = message
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
        .map(|sender| synthetic_session_for_full_jid(&sender));
    let sender_entity = room_entity(room_jid);
    let deps = build_interpret_deps(state, synthetic_session.as_ref()).with_ordered_relay_origin(
        Some(OrderedRelayRouteOrigin {
            kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
            sender_entity,
            inbound_sequence: 0,
            handoff: None,
        }),
    );
    let outcome = tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::interpret::dispatch_muc_to_room_for_relay(
            &deps,
            room_jid.clone(),
            message.clone(),
        ),
    )
    .await
    .map_err(|_| OrderedRelayNackReason::MaybeCommitted)?;
    Ok(outcome
        .frames
        .into_iter()
        .filter_map(
            |frame| match super::super::super::codec::decode_stanza(frame.as_str()) {
                Ok(stanza) => Some(RemoteStanza(stanza)),
                Err(error) => {
                    tracing::warn!(
                        room = %room_jid,
                        %error,
                        "ordered relay: MUC groupchat reply frame was not a stanza"
                    );
                    None
                }
            },
        )
        .collect())
}

pub(in super::super) fn muc_proxy_result_to_outcome(
    result: Result<Vec<RemoteStanza>, OrderedRelayNackReason>,
) -> RemoteDeliveryOutcome {
    match result {
        Ok(replies) => RemoteDeliveryOutcome {
            delivery: FullJidDeliveryOutcome::Delivered,
            client_replies: replies.into_iter().map(|reply| reply.0).collect(),
            maybe_committed: false,
            join_repair_allowed: false,
        },
        Err(OrderedRelayNackReason::MaybeCommitted) => {
            no_client_reply_outcome_with_commit_state(FullJidDeliveryOutcome::Dropped, true)
        }
        Err(OrderedRelayNackReason::TargetUnavailable) => {
            no_client_reply_outcome(FullJidDeliveryOutcome::Unavailable)
        }
        Err(_) => no_client_reply_outcome(FullJidDeliveryOutcome::Dropped),
    }
}

pub(in super::super) fn muc_proxy_result_to_ordered_outcome(
    kind: OrderedRelayMucProxyKind,
    result: Result<Vec<RemoteStanza>, OrderedRelayNackReason>,
) -> OrderedRelayMucProxyOutcome {
    match result {
        Ok(replies) => OrderedRelayMucProxyOutcome::Delivered(
            replies.into_iter().map(|reply| reply.0).collect(),
        ),
        Err(OrderedRelayNackReason::TargetUnavailable) => OrderedRelayMucProxyOutcome::Unavailable,
        Err(OrderedRelayNackReason::MaybeCommitted)
            if kind == OrderedRelayMucProxyKind::JoinPresence =>
        {
            OrderedRelayMucProxyOutcome::JoinMaybeCommitted
        }
        Err(OrderedRelayNackReason::MaybeCommitted) => OrderedRelayMucProxyOutcome::MaybeCommitted,
        Err(_) => OrderedRelayMucProxyOutcome::Dropped,
    }
}
