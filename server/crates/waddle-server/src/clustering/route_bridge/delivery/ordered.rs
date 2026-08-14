use super::*;

impl OrderedRelayDeliveryBridge {
    /// Return `Some` only when this exact full-JID target is currently owned
    /// by a fresh foreign `UserActor` claim and an ordered-relay send was
    /// attempted. `None` means the caller must keep the existing local path.
    /// `call_setup` (#1488): a routed 1:1 call-setup ticket. This
    /// function owns closing it whenever it returns `Some` — in
    /// particular the deferred-handoff branch, whose immediate
    /// `MaybeCommitted` is synthetic (it only suppresses local fallback)
    /// and whose real disposition resolves in the spawned completion
    /// task. Returning `None` leaves the ticket with the caller.
    pub(crate) fn try_deliver_full_jid_remote<'a>(
        self: &'a Arc<Self>,
        target: &'a jid::FullJid,
        stanza: &'a Stanza,
        origin: &'a OrderedRelayRouteOrigin,
        call_setup: Option<waddle_xmpp::telemetry::call::PendingCallSetupRoute>,
    ) -> RemoteDeliveryFuture<'a> {
        Box::pin(async move {
            self.try_deliver_full_jid_remote_with_capture(target, stanza, origin, call_setup, None)
                .await
                .map(|outcome| outcome.outcome)
        })
    }

    pub(crate) fn try_deliver_full_jid_remote_with_capture<'a>(
        self: &'a Arc<Self>,
        target: &'a jid::FullJid,
        stanza: &'a Stanza,
        origin: &'a OrderedRelayRouteOrigin,
        call_setup: Option<waddle_xmpp::telemetry::call::PendingCallSetupRoute>,
        deferred_capture: Option<crate::ingress_shadow::IngressEffectCapture>,
    ) -> CapturedRemoteDeliveryFuture<'a> {
        Box::pin(async move {
            if let Some(remote_origin) = remote_resource_origin(origin) {
                // Ticket ownership passes down: `route_remote_resource_origin`
                // has its own deferred-handoff branch and closes the
                // ticket from the REAL outcome (#1488).
                return Arc::clone(self)
                    .route_remote_resource_origin_with_capture(
                        remote_origin,
                        RemoteResourceRouteTarget::FullJid {
                            target: target.clone(),
                            stanza: RemoteStanza(stanza.clone()),
                        },
                        stanza,
                        origin,
                        call_setup,
                        deferred_capture,
                    )
                    .await;
            }
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
            let sender_claim =
                current_fresh_local_relay_claim(&services, &origin.sender_entity, &me, "sender")
                    .await?;

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
                services: services.clone(),
                target_entity: target_entity.clone(),
                previous_owner: target_snapshot.owner.clone(),
                channel,
                asserted_origin_node: NodeId::new(me.node_id.clone()),
                origin_inbound_sequence: OriginInboundSequence(origin.inbound_sequence),
                origin_claim,
                sender_claim,
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
                    let outcome_target = target.clone();
                    let outcome_message_id = match stanza {
                        Stanza::Message(message) => message.id.clone(),
                        Stanza::Iq(_) | Stanza::Presence(_) => None,
                    };
                    tokio::spawn(async move {
                        let sfu_for_bounce = bridge.sfu_for_bounce();
                        let fallback_services = seed.services.clone();
                        let fallback_target = seed.target.clone();
                        let fallback_payload = seed.payload.clone();
                        // `None` from `deliver_seeded_remote` means "relay
                        // declined; local fallback decides the disposition"
                        // (e.g. `RelayAskError::NotFound`) — the non-deferred
                        // path propagates it via `?` so the caller keeps the
                        // local path. The deferred branch already returned a
                        // synthetic `MaybeCommitted`, so it must run that fallback
                        // itself instead of treating `None` as a lost invite
                        // (#1611 review round 4).
                        let outcome = match bridge.deliver_seeded_remote(seed, true).await {
                            Some(remote) => caller_delivery_outcome(remote),
                            None => caller_delivery_outcome(
                                deliver_local_after_target_refresh_outcome(
                                    &fallback_services,
                                    &fallback_target,
                                    &origin_stanza,
                                    &fallback_payload,
                                )
                                .await,
                            ),
                        };
                        tracing::debug!(
                            jid = %outcome_target,
                            message_id = outcome_message_id
                                .as_ref()
                                .map_or("", |id| id.0.as_str()),
                            ?outcome,
                            "ordered-relay deferred full-JID delivery outcome"
                        );
                        // #1488: this is the point where the deferred
                        // delivery's REAL disposition is known — the
                        // `MaybeCommitted` returned below is synthetic. Close
                        // the call-setup ticket here.
                        crate::server::routes::interpret::close_call_setup_from_outcome(
                            call_setup, outcome,
                        );
                        handoff.complete(replies_for_origin_handoff(
                            &origin_stanza,
                            outcome,
                            sfu_for_bounce.as_deref(),
                        ));
                    });
                    return Some(CapturedRemoteDeliveryOutcome::from_outcome(
                        FullJidDeliveryOutcome::MaybeCommitted,
                    ));
                }
            }

            let outcome =
                caller_delivery_outcome(Arc::clone(self).deliver_seeded_remote(seed, true).await?);
            tracing::debug!(
                jid = %target,
                message_id = stanza_message_id(stanza),
                ?outcome,
                "ordered-relay full-JID delivery outcome"
            );
            crate::server::routes::interpret::close_call_setup_from_outcome(call_setup, outcome);
            Some(CapturedRemoteDeliveryOutcome::from_outcome(outcome))
        })
    }

    /// Return `Some` only when this bare-JID target is currently owned by a
    /// fresh foreign `UserActor` claim and an ordered-relay send was attempted.
    /// `None` means the caller must keep the existing local path.
    pub(crate) fn try_deliver_bare_jid_remote<'a>(
        self: &'a Arc<Self>,
        target: &'a jid::BareJid,
        stanza: &'a Stanza,
        origin: &'a OrderedRelayRouteOrigin,
    ) -> RemoteDeliveryFuture<'a> {
        Box::pin(async move {
            if let Some(remote_origin) = remote_resource_origin(origin) {
                return Arc::clone(self)
                    .route_remote_resource_origin(
                        remote_origin,
                        RemoteResourceRouteTarget::BareJid {
                            target: target.clone(),
                            stanza: RemoteStanza(stanza.clone()),
                        },
                        stanza,
                        origin,
                        None,
                    )
                    .await;
            }
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
            let sender_claim =
                current_fresh_local_relay_claim(&services, &origin.sender_entity, &me, "sender")
                    .await?;

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
                sender_claim,
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
                        let sfu_for_bounce = bridge.sfu_for_bounce();
                        let fallback_services = seed.services.clone();
                        let fallback_target = seed.target.clone();
                        let fallback_payload = seed.payload.clone();
                        // Same `None` semantics as the full-JID deferred
                        // branch above: relay declined, so run the local
                        // fallback rather than dropping the stanza with no
                        // replies (#1611 review round 4).
                        let outcome = match bridge.deliver_seeded_remote(seed, true).await {
                            Some(remote) => caller_delivery_outcome(remote),
                            None => caller_delivery_outcome(
                                deliver_local_after_target_refresh_outcome(
                                    &fallback_services,
                                    &fallback_target,
                                    &origin_stanza,
                                    &fallback_payload,
                                )
                                .await,
                            ),
                        };
                        handoff.complete(replies_for_origin_handoff(
                            &origin_stanza,
                            outcome,
                            sfu_for_bounce.as_deref(),
                        ));
                    });
                    return Some(FullJidDeliveryOutcome::MaybeCommitted);
                }
            }

            Some(caller_delivery_outcome(
                Arc::clone(self).deliver_seeded_remote(seed, true).await?,
            ))
        })
    }
}
