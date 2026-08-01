use super::*;

impl OrderedRelayDeliveryBridge {
    /// Return `Some` only when this exact full-JID target is currently owned
    /// by a fresh foreign `UserActor` claim and an ordered-relay send was
    /// attempted. `None` means the caller must keep the existing local path.
    /// `call_setup` (#1488): a routed 1:1 call-setup ticket. This
    /// function owns closing it whenever it returns `Some` — in
    /// particular the deferred-handoff branch, whose immediate
    /// `Delivered` is synthetic (it only suppresses local fallback)
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
            if let Some(remote_origin) = remote_resource_origin(origin) {
                // Ticket ownership passes down: `route_remote_resource_origin`
                // has its own deferred-handoff branch and closes the
                // ticket from the REAL outcome (#1488).
                return Arc::clone(self)
                    .route_remote_resource_origin(
                        remote_origin,
                        RemoteResourceRouteTarget::FullJid {
                            target: target.clone(),
                            stanza: RemoteStanza(stanza.clone()),
                        },
                        stanza,
                        origin,
                        call_setup,
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
                        // synthetic `Delivered`, so it must run that fallback
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
                        // `Delivered` returned below is synthetic. Close
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
                    return Some(FullJidDeliveryOutcome::Delivered);
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
            Some(outcome)
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
                    return Some(FullJidDeliveryOutcome::Delivered);
                }
            }

            Some(caller_delivery_outcome(
                Arc::clone(self).deliver_seeded_remote(seed, true).await?,
            ))
        })
    }

    pub(in super::super) async fn deliver_seeded_remote(
        self: Arc<Self>,
        seed: RemoteDeliverySeed,
        allow_target_refresh_retry: bool,
    ) -> Option<RemoteDeliveryOutcome> {
        let channel = seed.channel.clone();
        let Some(lock) = self.lock_for_channel(&channel).await else {
            self.divert_channel(channel, OrderedRelayDiversionReason::Backpressure)
                .await;
            return Some(no_client_reply_outcome(definite_no_effect_outcome(
                seed.is_iq,
            )));
        };
        let outcome = {
            let _guard = lock.lock().await;
            match self.prepare_remote_delivery(seed).await {
                Ok(prepared) => {
                    Arc::clone(&self)
                        .deliver_prepared_remote(prepared, allow_target_refresh_retry)
                        .await
                }
                Err(outcome) => Some(no_client_reply_outcome(outcome)),
            }
        };
        self.remove_channel_lock_if_unused(&channel, &lock).await;
        outcome
    }

    pub(in super::super) async fn prepare_remote_delivery(
        &self,
        seed: RemoteDeliverySeed,
    ) -> Result<PreparedRemoteDelivery, FullJidDeliveryOutcome> {
        let mut envelope = {
            let mut sender = self.sender_state.lock().await;
            match sender.next_envelope(
                seed.asserted_origin_node,
                seed.channel.clone(),
                seed.origin_inbound_sequence,
                OrderedRelayEnvelopeClaims::new(
                    seed.origin_claim,
                    seed.sender_claim,
                    seed.target_claim,
                ),
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

    pub(in super::super) async fn lock_for_channel(
        &self,
        channel: &OrderedRelayChannel,
    ) -> Option<Arc<Mutex<()>>> {
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

    pub(in super::super) async fn remove_channel_lock_if_unused(
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

    pub(in super::super) async fn deliver_prepared_remote(
        self: Arc<Self>,
        prepared: PreparedRemoteDelivery,
        allow_target_refresh_retry: bool,
    ) -> Option<RemoteDeliveryOutcome> {
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
                return Some(outcome);
            }
        }
        if allow_target_refresh_retry
            && matches!(&result, Err(error) if ask_error_allows_target_refresh(error))
        {
            if let Some(outcome) = Arc::clone(&self)
                .retry_after_target_owner_refresh(&prepared)
                .await
            {
                return Some(outcome);
            }
        }

        self.finish_prepared_delivery_result(prepared, result).await
    }

    pub(in super::super) async fn send_prepared_to_owner(
        &self,
        owner: &NodeIdentity,
        envelope: RemoteStanzaEnvelope,
    ) -> Result<OrderedRelayReply, RelayAskError> {
        let mut handle =
            RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        handle.deliver_ordered(envelope).await
    }

    pub(in super::super) async fn finish_prepared_delivery_result(
        self: Arc<Self>,
        prepared: PreparedRemoteDelivery,
        result: Result<OrderedRelayReply, RelayAskError>,
    ) -> Option<RemoteDeliveryOutcome> {
        match result {
            Ok(OrderedRelayReply::Ack(ack)) => Some(RemoteDeliveryOutcome {
                delivery: FullJidDeliveryOutcome::Delivered,
                client_replies: ack
                    .client_replies
                    .into_iter()
                    .map(|remote| remote.0)
                    .collect(),
                maybe_committed: false,
                join_repair_allowed: false,
            }),
            Ok(OrderedRelayReply::Nack(nack)) => {
                let (outcome, channel_action, maybe_committed) = outcome_for_nack(
                    &prepared.services,
                    &prepared.target_entity,
                    &prepared.previous_owner,
                    &nack,
                    prepared.is_iq,
                )
                .await;
                self.apply_nack_channel_action(&prepared.envelope, channel_action)
                    .await;
                let join_repair_allowed =
                    maybe_committed && !matches!(nack.reason, OrderedRelayNackReason::InFlight);
                match outcome {
                    Some(outcome) => {
                        Some(no_client_reply_outcome_with_commit_state_and_join_repair(
                            outcome,
                            maybe_committed,
                            join_repair_allowed,
                        ))
                    }
                    None => Some(
                        deliver_local_after_target_refresh_outcome(
                            &prepared.services,
                            &prepared.target,
                            &prepared.stanza,
                            &prepared.envelope.payload,
                        )
                        .await,
                    ),
                }
            }
            Err(error) => {
                if matches!(error, RelayAskError::NotFound { .. }) {
                    self.sender_state
                        .lock()
                        .await
                        .rollback_unseen_envelope(&prepared.envelope);
                }
                if let Some(reason) = channel_diversion_for_ask_error(&error) {
                    self.divert_channel(prepared.channel, reason).await;
                }
                outcome_for_ask_error(&error, prepared.is_iq).map(|outcome| {
                    no_client_reply_outcome_with_commit_state(
                        outcome,
                        ask_error_maybe_committed(&error),
                    )
                })
            }
        }
    }

    pub(in super::super) async fn retry_after_target_owner_refresh(
        self: Arc<Self>,
        prepared: &PreparedRemoteDelivery,
    ) -> Option<RemoteDeliveryOutcome> {
        let snapshot = current_claim(&prepared.services, &prepared.target_entity).await?;
        if !snapshot.owner_lease_fresh {
            return None;
        }

        let me = prepared.services.node_identity.current();
        if snapshot.owner == me {
            self.forget_channel(&prepared.envelope.channel).await;
            return Some(
                deliver_local_after_target_refresh_outcome(
                    &prepared.services,
                    &prepared.target,
                    &prepared.stanza,
                    &prepared.envelope.payload,
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

        self.forget_channel(&prepared.envelope.channel).await;

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
            sender_claim: prepared.envelope.sender_claim.clone(),
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
            return Some(no_client_reply_outcome(definite_no_effect_outcome(
                prepared.is_iq,
            )));
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
                Err(outcome) => Some(no_client_reply_outcome(outcome)),
            }
        };
        self.remove_channel_lock_if_unused(&new_channel, &lock)
            .await;
        outcome
    }

    pub(in super::super) fn sign_envelope(
        &self,
        envelope: &mut RemoteStanzaEnvelope,
    ) -> Result<(), ()> {
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

    pub(in super::super) async fn divert_channel(
        &self,
        channel: OrderedRelayChannel,
        reason: OrderedRelayDiversionReason,
    ) {
        self.sender_state
            .lock()
            .await
            .divert(OrderedRelayDiversion { channel, reason });
    }

    pub(in super::super) async fn forget_channel(&self, channel: &OrderedRelayChannel) {
        self.sender_state.lock().await.forget_channel(channel);
    }

    pub(in super::super) async fn apply_nack_channel_action(
        &self,
        envelope: &RemoteStanzaEnvelope,
        action: NackChannelAction,
    ) {
        match action {
            NackChannelAction::Divert(reason) => {
                self.divert_channel(envelope.channel.clone(), reason).await;
            }
            NackChannelAction::Forget => self.forget_channel(&envelope.channel).await,
            NackChannelAction::Keep => {}
            NackChannelAction::Rollback => {
                self.sender_state
                    .lock()
                    .await
                    .rollback_unseen_envelope(envelope);
            }
        }
    }
}
