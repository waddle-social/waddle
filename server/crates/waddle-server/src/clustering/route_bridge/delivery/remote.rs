use super::*;

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

    /// Return `Some` only when this room is currently owned by a fresh
    /// foreign `RoomActor` claim and an ordered-relay MUC proxy send was
    /// attempted. `None` means the caller must keep the existing local room
    /// path.
    /// the deferred-handoff spawn (the immediate `Delivered` is
    /// synthetic), or from the ask outcome inline — whenever this
    /// function returns `Some`. `None` leaves the ticket with the
    /// caller's fallback path.
    pub(in super::super) async fn route_remote_resource_origin(
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

    pub(in super::super) async fn route_remote_resource_origin_once(
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

    pub(in super::super) async fn route_remote_resource_origin_muc(
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

    pub(in super::super) async fn route_remote_resource_origin_muc_once(
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

    pub(in super::super) async fn ask_remote_resource_origin(
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

    pub(in super::super) async fn route_remote_resource_target_from_local_origin(
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

    pub(in super::super) async fn route_remote_resource_muc_target_from_local_origin(
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

    pub(in super::super) async fn refresh_remote_resource_origin(
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

    pub(in super::super) async fn deliver_registered_remote_resource_with_registration(
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
}
pub(in super::super) fn route_origin_claim(
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

pub(in super::super) fn remote_resource_origin(
    origin: &OrderedRelayRouteOrigin,
) -> Option<RemoteResourceOriginSnapshot> {
    match &origin.kind {
        OrderedRelayRouteOriginKind::RemoteResource(remote) => Some(remote.clone()),
        OrderedRelayRouteOriginKind::SmSession(_) | OrderedRelayRouteOriginKind::Entity(_) => None,
    }
}

pub(in super::super) fn local_origin_for_remote_resource(
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

pub(in super::super) async fn current_fresh_local_relay_claim(
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

pub(in super::super) fn payload_for_recipient(
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
