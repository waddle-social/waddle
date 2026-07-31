use super::*;

pub(super) mod muc;
pub(super) mod receiver;

pub(super) use muc::muc_proxy_result_to_ordered_outcome;
use muc::*;
pub(crate) use muc::{MucProxyRouteDecision, OrderedRelayMucProxyOutcome};
use receiver::*;

impl OrderedRelayDeliveryBridge {
    pub(crate) async fn remote_resource_origin_if_owner(
        &self,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
    ) -> Option<RemoteResourceOriginSnapshot> {
        let registrations = self.remote_socket_resources.lock().await;
        let registration = registrations
            .get(jid)
            .filter(|registration| Arc::ptr_eq(&registration.owner, owner))?;
        Some(RemoteResourceOriginSnapshot {
            jid: jid.clone(),
            registration_id: registration.registration_id,
            socket_generation: registration.socket_generation,
            user_owner: registration.user_owner.clone(),
        })
    }

    pub(crate) async fn try_deliver_registered_remote_resource(
        &self,
        target: &jid::FullJid,
        stanza: &Stanza,
        kind: DeliveryKind,
    ) -> Option<FullJidDeliveryOutcome> {
        let registration = {
            let registrations = self.remote_owner_resources.lock().await;
            registrations.get(target).cloned()
        }?;
        self.deliver_registered_remote_resource_with_registration(
            target,
            stanza,
            kind,
            &registration,
        )
        .await
    }

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
                        let delivery_outcome = bridge
                            .deliver_seeded_remote(seed, true)
                            .await
                            .map(caller_delivery_outcome);
                        if let Some(outcome) = delivery_outcome {
                            tracing::debug!(
                                jid = %outcome_target,
                                message_id = outcome_message_id
                                    .as_ref()
                                    .map_or("", |id| id.0.as_str()),
                                ?outcome,
                                "ordered-relay deferred full-JID delivery outcome"
                            );
                        }
                        // #1488: this is the point where the deferred
                        // delivery's REAL disposition is known — the
                        // `Delivered` returned below is synthetic. Close
                        // the call-setup ticket here; a `None` outcome
                        // means the relay never handed the stanza to
                        // anyone (and there is no local fallback on the
                        // deferred branch), so the invite is lost.
                        match delivery_outcome {
                            Some(outcome) => {
                                crate::server::routes::interpret::close_call_setup_from_outcome(
                                    call_setup, outcome,
                                );
                            }
                            None => {
                                if let Some(ticket) = call_setup {
                                    ticket.undeliverable();
                                }
                            }
                        }
                        let replies = delivery_outcome
                            .map(|outcome| {
                                replies_for_origin_handoff(
                                    &origin_stanza,
                                    outcome,
                                    sfu_for_bounce.as_deref(),
                                )
                            })
                            .unwrap_or_default();
                        handoff.complete(replies);
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
                        let replies = bridge
                            .deliver_seeded_remote(seed, true)
                            .await
                            .map(|outcome| {
                                replies_for_origin_handoff(
                                    &origin_stanza,
                                    caller_delivery_outcome(outcome),
                                    sfu_for_bounce.as_deref(),
                                )
                            })
                            .unwrap_or_default();
                        handoff.complete(replies);
                    });
                    return Some(FullJidDeliveryOutcome::Delivered);
                }
            }

            Some(caller_delivery_outcome(
                Arc::clone(self).deliver_seeded_remote(seed, true).await?,
            ))
        })
    }

    /// Return `Some` only when this room is currently owned by a fresh
    /// foreign `RoomActor` claim and an ordered-relay MUC proxy send was
    /// attempted. `None` means the caller must keep the existing local room
    /// path.
    /// the deferred-handoff spawn (the immediate `Delivered` is
    /// synthetic), or from the ask outcome inline — whenever this
    /// function returns `Some`. `None` leaves the ticket with the
    /// caller's fallback path.
    pub(super) async fn route_remote_resource_origin(
        self: Arc<Self>,
        remote_origin: RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
        origin_stanza: &Stanza,
        origin: &OrderedRelayRouteOrigin,
        call_setup: Option<waddle_xmpp::telemetry::call::PendingCallSetupRoute>,
    ) -> Option<FullJidDeliveryOutcome> {
        let outcome_log = route_outcome_log(&target);
        if let Some(handoff) = origin.handoff.clone() {
            if handoff.mark_deferred() {
                let bridge = Arc::clone(&self);
                let origin_stanza = origin_stanza.clone();
                tokio::spawn(async move {
                    let outcome = bridge
                        .route_remote_resource_origin_once(remote_origin, target)
                        .await
                        .unwrap_or(FullJidDeliveryOutcome::Dropped);
                    log_remote_resource_route_outcome(&outcome_log, outcome);
                    crate::server::routes::interpret::close_call_setup_from_outcome(
                        call_setup, outcome,
                    );
                    handoff.complete(replies_for_origin_handoff(
                        &origin_stanza,
                        outcome,
                        bridge.sfu_for_bounce().as_deref(),
                    ));
                });
                return Some(FullJidDeliveryOutcome::Delivered);
            }
        }
        let outcome = self
            .route_remote_resource_origin_once(remote_origin, target)
            .await;
        if let Some(outcome) = outcome {
            log_remote_resource_route_outcome(&outcome_log, outcome);
            crate::server::routes::interpret::close_call_setup_from_outcome(call_setup, outcome);
        }
        outcome
    }

    pub(super) async fn route_remote_resource_origin_once(
        self: &Arc<Self>,
        remote_origin: RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Option<FullJidDeliveryOutcome> {
        let target_is_iq = route_target_stanza_is_iq(&target);
        let reply = self
            .ask_remote_resource_origin(&remote_origin, target.clone())
            .await;
        match reply {
            Ok(reply) if reply.outcome == RemoteResourceRouteOutcome::StaleRegistration => {
                match self.refresh_remote_resource_origin(&remote_origin).await {
                    RemoteResourceOriginRefresh::Remote(refreshed) => {
                        match self.ask_remote_resource_origin(&refreshed, target).await {
                            Ok(reply) => Some(reply.outcome.into()),
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "clustered remote-resource origin route retry failed"
                                );
                                outcome_for_ask_error(&error, target_is_iq)
                                    .or(Some(FullJidDeliveryOutcome::Dropped))
                            }
                        }
                    }
                    RemoteResourceOriginRefresh::LocalOwner => {
                        self.route_remote_resource_target_from_local_origin(&remote_origin, target)
                            .await
                    }
                    RemoteResourceOriginRefresh::Failed => {
                        Some(FullJidDeliveryOutcome::Unavailable)
                    }
                }
            }
            Ok(reply) => Some(reply.outcome.into()),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "clustered remote-resource origin route ask failed"
                );
                if ask_error_allows_target_refresh(&error) {
                    match self.refresh_remote_resource_origin(&remote_origin).await {
                        RemoteResourceOriginRefresh::Remote(refreshed) => {
                            return match self.ask_remote_resource_origin(&refreshed, target).await {
                                Ok(reply) => Some(reply.outcome.into()),
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        "clustered remote-resource origin route retry failed"
                                    );
                                    outcome_for_ask_error(&error, target_is_iq)
                                        .or(Some(FullJidDeliveryOutcome::Dropped))
                                }
                            };
                        }
                        RemoteResourceOriginRefresh::LocalOwner => {
                            return self
                                .route_remote_resource_target_from_local_origin(
                                    &remote_origin,
                                    target,
                                )
                                .await;
                        }
                        RemoteResourceOriginRefresh::Failed => {}
                    }
                }
                outcome_for_ask_error(&error, target_is_iq)
                    .or(Some(FullJidDeliveryOutcome::Dropped))
            }
        }
    }

    pub(super) async fn route_remote_resource_origin_muc(
        self: Arc<Self>,
        remote_origin: RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
        _origin_stanza: &Stanza,
        _origin: &OrderedRelayRouteOrigin,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        // MUC joins mutate local membership state after this call returns, so
        // the socket node must observe the owner node's real result instead of
        // deferring through the SM handoff and reporting provisional success.
        self.route_remote_resource_origin_muc_once(remote_origin, target)
            .await
    }

    pub(super) async fn route_remote_resource_origin_muc_once(
        self: &Arc<Self>,
        remote_origin: RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        let reply = self
            .ask_remote_resource_origin(&remote_origin, target.clone())
            .await;
        match reply {
            Ok(reply) if reply.outcome == RemoteResourceRouteOutcome::StaleRegistration => {
                match self.refresh_remote_resource_origin(&remote_origin).await {
                    RemoteResourceOriginRefresh::Remote(refreshed) => {
                        match self
                            .ask_remote_resource_origin(&refreshed, target.clone())
                            .await
                        {
                            Ok(reply) => Some(remote_resource_muc_outcome(reply)),
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "clustered remote-resource MUC origin route retry failed"
                                );
                                Some(remote_resource_muc_ask_error_outcome(&target, &error))
                            }
                        }
                    }
                    RemoteResourceOriginRefresh::LocalOwner => {
                        self.route_remote_resource_muc_target_from_local_origin(
                            &remote_origin,
                            target,
                        )
                        .await
                    }
                    RemoteResourceOriginRefresh::Failed => {
                        Some(OrderedRelayMucProxyOutcome::Unavailable)
                    }
                }
            }
            Ok(reply) => Some(remote_resource_muc_outcome(reply)),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "clustered remote-resource MUC origin route ask failed"
                );
                if ask_error_allows_target_refresh(&error) {
                    match self.refresh_remote_resource_origin(&remote_origin).await {
                        RemoteResourceOriginRefresh::Remote(refreshed) => {
                            return match self
                                .ask_remote_resource_origin(&refreshed, target.clone())
                                .await
                            {
                                Ok(reply) => Some(remote_resource_muc_outcome(reply)),
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        "clustered remote-resource MUC origin route retry failed"
                                    );
                                    Some(remote_resource_muc_ask_error_outcome(&target, &error))
                                }
                            };
                        }
                        RemoteResourceOriginRefresh::LocalOwner => {
                            return self
                                .route_remote_resource_muc_target_from_local_origin(
                                    &remote_origin,
                                    target,
                                )
                                .await;
                        }
                        RemoteResourceOriginRefresh::Failed => {}
                    }
                }
                Some(remote_resource_muc_ask_error_outcome(&target, &error))
            }
        }
    }

    pub(super) async fn ask_remote_resource_origin(
        &self,
        remote_origin: &RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Result<RelayRouteRemoteResourceStanzaReply, RelayAskError> {
        let mut handle =
            RelayHandle::new(remote_origin.user_owner.clone(), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        handle
            .route_remote_resource_stanza(RelayRouteRemoteResourceStanza {
                source_jid: remote_origin.jid.clone(),
                registration_id: remote_origin.registration_id,
                socket_generation: remote_origin.socket_generation,
                target,
                trace: RelayTraceContext::default(),
            })
            .await
    }

    pub(super) async fn route_remote_resource_target_from_local_origin(
        self: &Arc<Self>,
        remote_origin: &RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Option<FullJidDeliveryOutcome> {
        let services = self.services.get().cloned()?;
        let origin = local_origin_for_remote_resource(remote_origin);
        match target {
            RemoteResourceRouteTarget::FullJid { target, stanza } => {
                if let Some(remote) = self
                    .try_deliver_full_jid_remote(&target, &stanza.0, &origin, None)
                    .await
                {
                    Some(remote)
                } else {
                    Some(
                        deliver_local_full_jid_after_target_refresh(&services, &target, &stanza.0)
                            .await,
                    )
                }
            }
            RemoteResourceRouteTarget::BareJid { target, stanza } => {
                match route_local_bare_jid_with_timeout(&services, &target, &stanza.0, Some(origin))
                    .await
                {
                    Ok(replies) if replies.is_empty() => Some(FullJidDeliveryOutcome::Delivered),
                    Ok(_) => Some(FullJidDeliveryOutcome::Unavailable),
                    Err(OrderedRelayNackReason::TargetUnavailable) => {
                        Some(FullJidDeliveryOutcome::Unavailable)
                    }
                    Err(_) => Some(FullJidDeliveryOutcome::Dropped),
                }
            }
            RemoteResourceRouteTarget::MucProxy { .. } => Some(FullJidDeliveryOutcome::Dropped),
        }
    }

    pub(super) async fn route_remote_resource_muc_target_from_local_origin(
        self: &Arc<Self>,
        remote_origin: &RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        let services = self.services.get().cloned()?;
        let RemoteResourceRouteTarget::MucProxy {
            room_jid,
            kind,
            stanza,
        } = target
        else {
            return Some(OrderedRelayMucProxyOutcome::Dropped);
        };
        let origin = local_origin_for_remote_resource(remote_origin);
        if let Some(remote) = self
            .try_proxy_muc_remote_from_local_origin(&room_jid, &stanza.0, kind, &origin)
            .await
        {
            Some(remote)
        } else {
            Some(muc_proxy_result_to_ordered_outcome(
                kind,
                Box::pin(deliver_reserved_muc_proxy(
                    &services, &room_jid, kind, &stanza.0,
                ))
                .await,
            ))
        }
    }

    pub(super) async fn refresh_remote_resource_origin(
        self: &Arc<Self>,
        remote_origin: &RemoteResourceOriginSnapshot,
    ) -> RemoteResourceOriginRefresh {
        let Some(services) = self.services.get().cloned() else {
            return RemoteResourceOriginRefresh::Failed;
        };
        let owner = {
            let registrations = self.remote_socket_resources.lock().await;
            let Some(owner) = registrations
                .get(&remote_origin.jid)
                .filter(|registration| {
                    registration.registration_id == remote_origin.registration_id
                        && registration.socket_generation == remote_origin.socket_generation
                })
                .map(|registration| registration.owner.clone())
            else {
                return RemoteResourceOriginRefresh::Failed;
            };
            owner
        };
        let Some(entry) = services
            .connection_registry
            .entry_if_owner(&remote_origin.jid, &owner)
        else {
            return RemoteResourceOriginRefresh::Failed;
        };
        let target_entity = user_entity(&remote_origin.jid.to_bare());
        let Some(snapshot) = current_claim(&services, &target_entity).await else {
            return RemoteResourceOriginRefresh::Failed;
        };
        let me = services.node_identity.current();
        if snapshot.owner_lease_fresh && snapshot.owner == me {
            match crate::server::dual_registration::mirror_register_outcome(
                &services.user_registry,
                remote_origin.jid.clone(),
                entry,
            )
            .await
            {
                crate::server::dual_registration::MirrorRegisterOutcome::Registered => {}
                crate::server::dual_registration::MirrorRegisterOutcome::ForeignOwner
                | crate::server::dual_registration::MirrorRegisterOutcome::Failed => {
                    return RemoteResourceOriginRefresh::Failed;
                }
            }
            self.remove_remote_socket_registration_if_snapshot(remote_origin, &owner)
                .await;
            return RemoteResourceOriginRefresh::LocalOwner;
        }
        if !snapshot.owner_lease_fresh {
            return RemoteResourceOriginRefresh::Failed;
        }
        match self
            .try_register_remote_user_resource(&remote_origin.jid, entry, owner.clone())
            .await
        {
            RemoteResourceRegisterOutcome::Registered => self
                .remote_resource_origin_if_owner(&remote_origin.jid, &owner)
                .await
                .map(RemoteResourceOriginRefresh::Remote)
                .unwrap_or(RemoteResourceOriginRefresh::Failed),
            RemoteResourceRegisterOutcome::NotRemote => RemoteResourceOriginRefresh::Failed,
            RemoteResourceRegisterOutcome::Failed => RemoteResourceOriginRefresh::Failed,
        }
    }

    pub(crate) async fn route_remote_resource_stanza_on_owner(
        self: &Arc<Self>,
        msg: RelayRouteRemoteResourceStanza,
    ) -> RelayRouteRemoteResourceStanzaReply {
        let Some(services) = self.services.get().cloned() else {
            return remote_resource_route_reply(RemoteResourceRouteOutcome::Dropped);
        };
        let Some(registration) = self
            .remote_owner_resources
            .lock()
            .await
            .get(&msg.source_jid)
            .filter(|registration| {
                registration.registration_id == msg.registration_id
                    && registration.socket_generation == msg.socket_generation
            })
            .cloned()
        else {
            return remote_resource_route_reply(RemoteResourceRouteOutcome::StaleRegistration);
        };
        let actor = match services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: msg.source_jid.to_bare(),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) => {
                return remote_resource_route_reply(RemoteResourceRouteOutcome::StaleRegistration);
            }
            Err(error) => {
                tracing::warn!(
                    jid = %msg.source_jid,
                    %error,
                    "clustered remote-resource origin route could not resolve owner UserActor"
                );
                return remote_resource_route_reply(RemoteResourceRouteOutcome::Dropped);
            }
        };
        if let Err(status) = owner_remote_entry_if_current(
            &actor,
            &services.connection_registry,
            &msg.source_jid,
            &registration.owner,
        )
        .await
        {
            return remote_resource_route_reply(match status {
                RelayRemoteResourceUpdateStatus::Updated => RemoteResourceRouteOutcome::Delivered,
                RelayRemoteResourceUpdateStatus::StaleRegistration => {
                    RemoteResourceRouteOutcome::StaleRegistration
                }
                RelayRemoteResourceUpdateStatus::Unavailable => RemoteResourceRouteOutcome::Dropped,
            });
        }

        let sender_entity = user_entity(&msg.source_jid.to_bare());
        let origin = OrderedRelayRouteOrigin {
            kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
            sender_entity,
            inbound_sequence: 0,
            handoff: None,
        };

        match msg.target {
            RemoteResourceRouteTarget::FullJid { target, stanza } => {
                let outcome = if let Some(remote) = self
                    .try_deliver_full_jid_remote(&target, &stanza.0, &origin, None)
                    .await
                {
                    remote
                } else if let Some(registered) = self
                    .try_deliver_registered_remote_resource(
                        &target,
                        &stanza.0,
                        DeliveryKind::PeerStanza,
                    )
                    .await
                {
                    registered
                } else {
                    deliver_local_full_jid_after_target_refresh(&services, &target, &stanza.0).await
                };
                remote_resource_route_reply(outcome.into())
            }
            RemoteResourceRouteTarget::BareJid { target, stanza } => {
                match route_local_bare_jid_with_timeout(&services, &target, &stanza.0, Some(origin))
                    .await
                {
                    Ok(replies) if replies.is_empty() => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::Delivered)
                    }
                    Ok(_) => remote_resource_route_reply(RemoteResourceRouteOutcome::Unavailable),
                    Err(OrderedRelayNackReason::TargetUnavailable) => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::Unavailable)
                    }
                    Err(_) => remote_resource_route_reply(RemoteResourceRouteOutcome::Dropped),
                }
            }
            RemoteResourceRouteTarget::MucProxy {
                room_jid,
                kind,
                stanza,
            } => {
                // This path can execute the payload locally WITHOUT the
                // envelope validation (`envelope_is_consistent` /
                // `validate_claims`) that the ordered-relay receiver
                // applies, so nothing here has bound the stanza's
                // `from` to the sending session. That is tolerable for
                // the presence/message kinds, but `MujiJingleIq` mints
                // a media credential from whatever `from` says (#1445),
                // so bind it to the registration this call already
                // authenticated (`msg.source_jid`) before dispatch.
                let stanza = if kind == OrderedRelayMucProxyKind::MujiJingleIq {
                    RemoteStanza(rebind_stanza_sender(&stanza.0, &msg.source_jid))
                } else {
                    stanza
                };
                let outcome = if let Some(remote) = self
                    .try_proxy_muc_remote(&room_jid, &stanza.0, kind, &origin)
                    .await
                {
                    remote
                } else {
                    muc_proxy_result_to_ordered_outcome(
                        kind,
                        deliver_reserved_muc_proxy(&services, &room_jid, kind, &stanza.0).await,
                    )
                };
                match outcome {
                    OrderedRelayMucProxyOutcome::Delivered(replies) => {
                        RelayRouteRemoteResourceStanzaReply {
                            outcome: RemoteResourceRouteOutcome::Delivered,
                            replies: replies.into_iter().map(RemoteStanza).collect(),
                        }
                    }
                    OrderedRelayMucProxyOutcome::Unavailable => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::Unavailable)
                    }
                    OrderedRelayMucProxyOutcome::Dropped => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::Dropped)
                    }
                    OrderedRelayMucProxyOutcome::MaybeCommitted => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::MaybeCommitted)
                    }
                    OrderedRelayMucProxyOutcome::JoinMaybeCommitted => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::JoinMaybeCommitted)
                    }
                }
            }
        }
    }

    pub(crate) async fn deliver_remote_resource_frame_on_socket(
        &self,
        msg: RelayDeliverRemoteResourceFrame,
    ) -> RelayRemoteResourceFrameReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Unavailable,
            };
        };
        let registration = {
            let registrations = self.remote_socket_resources.lock().await;
            registrations
                .get(&msg.frame.jid)
                .filter(|registration| registration.registration_id == msg.frame.registration_id)
                .cloned()
        };
        let Some(registration) = registration else {
            return RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Unavailable,
            };
        };
        let outbound = OutboundStanza {
            stanza: msg.frame.stanza.0,
            kind: msg.frame.kind,
            pending_row_id: None,
            pending_row_original_receipt_at: None,
        };
        let outcome = services.connection_registry.try_send_outbound_if_owner(
            &msg.frame.jid,
            &registration.owner,
            outbound,
        );
        let status = match outcome {
            BroadcastOutcome::Delivered => RelayRemoteResourceFrameStatus::Delivered,
            BroadcastOutcome::DroppedFull => RelayRemoteResourceFrameStatus::Backpressure,
            BroadcastOutcome::NotConnected | BroadcastOutcome::DroppedClosed => {
                RelayRemoteResourceFrameStatus::Unavailable
            }
        };
        RelayRemoteResourceFrameReply { status }
    }

    pub(super) async fn deliver_registered_remote_resource_with_registration(
        &self,
        target: &jid::FullJid,
        stanza: &Stanza,
        kind: DeliveryKind,
        registration: &RemoteOwnerRegistration,
    ) -> Option<FullJidDeliveryOutcome> {
        let mut handle =
            RelayHandle::new(registration.socket_node.clone(), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match handle
            .deliver_remote_resource_frame(RelayDeliverRemoteResourceFrame {
                frame: RemoteResourceOutboundFrame {
                    jid: target.clone(),
                    registration_id: registration.registration_id,
                    stanza: RemoteStanza(stanza.clone()),
                    kind,
                },
                trace: RelayTraceContext::default(),
            })
            .await
        {
            Ok(RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Delivered,
            }) => {
                // The direct remote-resource path bypasses the counted
                // owner-node delivery channels AND the deliberately
                // uncounted socket endpoint, so the flagship delivered-
                // message counter must be bumped here — once, on the
                // owner node, upon the socket node's acknowledgment.
                if let Some(message_kind) =
                    waddle_xmpp::telemetry::messages::delivered_message_kind(stanza)
                {
                    waddle_xmpp::telemetry::messages::record_delivered_message(message_kind);
                }
                tracing::debug!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    outcome = ?FullJidDeliveryOutcome::Delivered,
                    "clustered remote-resource delivery outcome"
                );
                Some(FullJidDeliveryOutcome::Delivered)
            }
            Ok(RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Backpressure,
            }) => {
                tracing::debug!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    outcome = ?FullJidDeliveryOutcome::Dropped,
                    "clustered remote-resource delivery outcome"
                );
                Some(FullJidDeliveryOutcome::Dropped)
            }
            Ok(RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Unavailable,
            }) => {
                tracing::debug!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    outcome = ?FullJidDeliveryOutcome::Unavailable,
                    "clustered remote-resource delivery outcome"
                );
                self.cleanup_remote_owner_resource_if_registration(
                    target,
                    registration.registration_id,
                )
                .await;
                None
            }
            Err(error) => {
                tracing::warn!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    %error,
                    "clustered remote-resource acked delivery relay ask failed"
                );
                if ask_error_proves_remote_resource_ref_stale(&error) {
                    self.cleanup_remote_owner_resource_if_registration(
                        target,
                        registration.registration_id,
                    )
                    .await;
                    None
                } else {
                    Some(FullJidDeliveryOutcome::MaybeCommitted)
                }
            }
        }
    }

    pub(super) async fn deliver_seeded_remote(
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

    pub(super) async fn prepare_remote_delivery(
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

    pub(super) async fn lock_for_channel(
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

    pub(super) async fn remove_channel_lock_if_unused(
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

    pub(super) async fn deliver_prepared_remote(
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

    pub(super) async fn send_prepared_to_owner(
        &self,
        owner: &NodeIdentity,
        envelope: RemoteStanzaEnvelope,
    ) -> Result<OrderedRelayReply, RelayAskError> {
        let mut handle =
            RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        handle.deliver_ordered(envelope).await
    }

    pub(super) async fn finish_prepared_delivery_result(
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

    pub(super) async fn retry_after_target_owner_refresh(
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

    /// Receiver-side effect for one already-reserved envelope. The caller
    /// commits the reservation only when this returns `Ok(())`.
    pub async fn deliver_reserved(
        &self,
        envelope: &RemoteStanzaEnvelope,
    ) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
        let Some(services) = self.services.get().cloned() else {
            return Err(OrderedRelayNackReason::Unreachable);
        };
        validate_claims(&services, envelope).await?;
        match relay_payload_target(envelope)? {
            RelayPayloadTarget::Full(target, stanza) => self
                .deliver_reserved_full_jid(&services, target, stanza)
                .await
                .map(|()| Vec::new()),
            RelayPayloadTarget::Bare(target, stanza) => {
                deliver_reserved_bare_jid(&services, &target, stanza)
                    .await
                    .map(|()| Vec::new())
            }
            RelayPayloadTarget::Muc(room, kind, stanza) => {
                deliver_reserved_muc_proxy(&services, room, kind, stanza).await
            }
        }
    }

    pub(super) fn sign_envelope(&self, envelope: &mut RemoteStanzaEnvelope) -> Result<(), ()> {
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

    pub(super) async fn divert_channel(
        &self,
        channel: OrderedRelayChannel,
        reason: OrderedRelayDiversionReason,
    ) {
        self.sender_state
            .lock()
            .await
            .divert(OrderedRelayDiversion { channel, reason });
    }

    pub(super) async fn forget_channel(&self, channel: &OrderedRelayChannel) {
        self.sender_state.lock().await.forget_channel(channel);
    }

    pub(super) async fn apply_nack_channel_action(
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
pub(super) fn no_client_reply_outcome(delivery: FullJidDeliveryOutcome) -> RemoteDeliveryOutcome {
    no_client_reply_outcome_with_commit_state(delivery, false)
}

pub(super) fn remote_resource_route_reply(
    outcome: RemoteResourceRouteOutcome,
) -> RelayRouteRemoteResourceStanzaReply {
    RelayRouteRemoteResourceStanzaReply {
        outcome,
        replies: Vec::new(),
    }
}

pub(super) fn remote_resource_muc_outcome(
    reply: RelayRouteRemoteResourceStanzaReply,
) -> OrderedRelayMucProxyOutcome {
    match reply.outcome {
        RemoteResourceRouteOutcome::Delivered | RemoteResourceRouteOutcome::QueuedDetached => {
            OrderedRelayMucProxyOutcome::Delivered(
                reply.replies.into_iter().map(|reply| reply.0).collect(),
            )
        }
        RemoteResourceRouteOutcome::Unavailable | RemoteResourceRouteOutcome::StaleRegistration => {
            OrderedRelayMucProxyOutcome::Unavailable
        }
        RemoteResourceRouteOutcome::Dropped => OrderedRelayMucProxyOutcome::Dropped,
        RemoteResourceRouteOutcome::MaybeCommitted => OrderedRelayMucProxyOutcome::MaybeCommitted,
        RemoteResourceRouteOutcome::JoinMaybeCommitted => {
            OrderedRelayMucProxyOutcome::JoinMaybeCommitted
        }
    }
}

pub(super) fn remote_resource_muc_ask_error_outcome(
    target: &RemoteResourceRouteTarget,
    error: &RelayAskError,
) -> OrderedRelayMucProxyOutcome {
    if !ask_error_maybe_committed(error) {
        return OrderedRelayMucProxyOutcome::Dropped;
    }
    match target {
        RemoteResourceRouteTarget::MucProxy {
            kind: OrderedRelayMucProxyKind::JoinPresence,
            ..
        } => OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
        RemoteResourceRouteTarget::MucProxy { .. } => OrderedRelayMucProxyOutcome::MaybeCommitted,
        RemoteResourceRouteTarget::FullJid { .. } | RemoteResourceRouteTarget::BareJid { .. } => {
            OrderedRelayMucProxyOutcome::Dropped
        }
    }
}

pub(super) fn no_client_reply_outcome_with_commit_state(
    delivery: FullJidDeliveryOutcome,
    maybe_committed: bool,
) -> RemoteDeliveryOutcome {
    no_client_reply_outcome_with_commit_state_and_join_repair(
        delivery,
        maybe_committed,
        maybe_committed,
    )
}

pub(super) fn no_client_reply_outcome_with_commit_state_and_join_repair(
    delivery: FullJidDeliveryOutcome,
    maybe_committed: bool,
    join_repair_allowed: bool,
) -> RemoteDeliveryOutcome {
    RemoteDeliveryOutcome {
        delivery,
        client_replies: Vec::new(),
        maybe_committed,
        join_repair_allowed,
    }
}

pub(super) fn route_origin_claim(
    kind: &OrderedRelayRouteOriginKind,
) -> (Entity, OrderedRelayOrigin) {
    match kind {
        OrderedRelayRouteOriginKind::SmSession(stream_id) => (
            Entity::new(EntityType::SmSession, stream_id.to_string()),
            OrderedRelayOrigin::SmSession(stream_id.clone()),
        ),
        OrderedRelayRouteOriginKind::Entity(entity) => {
            (entity.clone(), OrderedRelayOrigin::Entity(entity.clone()))
        }
        OrderedRelayRouteOriginKind::RemoteResource(remote) => {
            let entity = user_entity(&remote.jid.to_bare());
            (entity.clone(), OrderedRelayOrigin::Entity(entity))
        }
    }
}

pub(super) fn remote_resource_origin(
    origin: &OrderedRelayRouteOrigin,
) -> Option<RemoteResourceOriginSnapshot> {
    match &origin.kind {
        OrderedRelayRouteOriginKind::RemoteResource(remote) => Some(remote.clone()),
        OrderedRelayRouteOriginKind::SmSession(_) | OrderedRelayRouteOriginKind::Entity(_) => None,
    }
}

pub(super) fn local_origin_for_remote_resource(
    remote_origin: &RemoteResourceOriginSnapshot,
) -> OrderedRelayRouteOrigin {
    let sender_entity = user_entity(&remote_origin.jid.to_bare());
    OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
        sender_entity,
        inbound_sequence: 0,
        handoff: None,
    }
}

pub(super) async fn current_fresh_local_relay_claim(
    services: &OrderedRelayDeliveryServices,
    entity: &Entity,
    me: &NodeIdentity,
    role: &'static str,
) -> Option<OrderedRelayClaim> {
    let snapshot = current_claim(services, entity).await?;
    if !snapshot.owner_lease_fresh || snapshot.owner != *me {
        tracing::debug!(
            entity = %entity,
            role,
            "ordered relay: entity is not currently owned locally; keeping local fallback path"
        );
        return None;
    }
    Some(OrderedRelayClaim {
        entity: entity.clone(),
        epoch: snapshot.claim_epoch,
    })
}

pub(super) fn payload_for_recipient(
    recipient: jid::Jid,
    stanza: &Stanza,
) -> Option<OrderedRelayPayload> {
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

pub(super) fn remote_replies_from_frames(
    frames: Vec<String>,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    frames
        .into_iter()
        .map(|frame| super::super::codec::decode_stanza(frame.as_str()).map(RemoteStanza))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            tracing::warn!(
                %error,
                "ordered relay: MUC proxy reply frame was not a stanza"
            );
            OrderedRelayNackReason::ParseFailure
        })
}

pub(super) fn synthetic_session_for_full_jid(sender_jid: &jid::FullJid) -> crate::auth::Session {
    let sender_bare = sender_jid.to_bare();
    let localpart = sender_bare
        .node()
        .map(|node| node.to_string())
        .unwrap_or_else(|| sender_bare.to_string());
    let sender_bare_string = sender_bare.to_string();
    crate::auth::Session::new(
        sender_bare_string.as_str(),
        localpart.as_str(),
        localpart.as_str(),
    )
}

pub(super) async fn deliver_local_after_target_refresh_outcome(
    services: &OrderedRelayDeliveryServices,
    target: &jid::Jid,
    stanza: &Stanza,
    payload: &OrderedRelayPayload,
) -> RemoteDeliveryOutcome {
    match payload {
        OrderedRelayPayload::MucProxy {
            room_jid,
            kind,
            stanza,
        } => muc_proxy_result_to_outcome(
            Box::pin(deliver_reserved_muc_proxy(
                services, room_jid, *kind, &stanza.0,
            ))
            .await,
        ),
        OrderedRelayPayload::Message { .. }
        | OrderedRelayPayload::Iq { .. }
        | OrderedRelayPayload::Presence { .. } => no_client_reply_outcome(
            deliver_local_after_target_refresh(services, target, stanza).await,
        ),
    }
}
pub(super) async fn deliver_local_after_target_refresh(
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

pub(super) async fn deliver_local_full_jid_after_target_refresh(
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
