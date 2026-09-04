use super::muc::MucProxyRouteAttempt;
use super::*;
use waddle_xmpp::ingress::RelayTargetIdentity;

impl OrderedRelayDeliveryBridge {
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
        self.route_remote_resource_origin_with_capture(
            remote_origin,
            target,
            origin_stanza,
            origin,
            call_setup,
            None,
        )
        .await
        .map(|outcome| outcome.outcome)
    }

    pub(in super::super) async fn route_remote_resource_origin_with_capture(
        self: Arc<Self>,
        remote_origin: RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
        origin_stanza: &Stanza,
        origin: &OrderedRelayRouteOrigin,
        call_setup: Option<waddle_xmpp::telemetry::call::PendingCallSetupRoute>,
        deferred_capture: Option<crate::ingress_shadow::IngressEffectCapture>,
    ) -> Option<CapturedRemoteDeliveryOutcome> {
        let outcome_log = route_outcome_log(&target);
        if let Some(handoff) = origin.handoff.clone() {
            if handoff.mark_deferred() {
                let bridge = Arc::clone(&self);
                let origin_stanza = origin_stanza.clone();
                tokio::spawn(async move {
                    let outcome = bridge
                        .route_remote_resource_origin_once_with_capture(remote_origin, target)
                        .await
                        .unwrap_or_else(|| {
                            CapturedRemoteDeliveryOutcome::from_outcome(
                                FullJidDeliveryOutcome::Dropped,
                            )
                        });
                    log_remote_resource_route_outcome(&outcome_log, outcome.outcome);
                    crate::server::routes::interpret::close_call_setup_from_outcome(
                        call_setup,
                        outcome.outcome,
                    );
                    if let Some(capture) = deferred_capture {
                        for stream in outcome.recipient_sm_append_streams {
                            capture.record_recipient_sm_append(stream);
                        }
                    }
                    handoff.complete(replies_for_origin_handoff(
                        &origin_stanza,
                        outcome.outcome,
                        bridge.sfu_for_bounce().as_deref(),
                    ));
                });
                return Some(CapturedRemoteDeliveryOutcome::from_outcome(
                    FullJidDeliveryOutcome::Delivered,
                ));
            }
        }
        let outcome = self
            .route_remote_resource_origin_once_with_capture(remote_origin, target)
            .await;
        if let Some(ref outcome) = outcome {
            log_remote_resource_route_outcome(&outcome_log, outcome.outcome);
            crate::server::routes::interpret::close_call_setup_from_outcome(
                call_setup,
                outcome.outcome,
            );
        }
        outcome
    }

    pub(in super::super) async fn route_remote_resource_origin_once_with_capture(
        self: &Arc<Self>,
        remote_origin: RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Option<CapturedRemoteDeliveryOutcome> {
        let target_is_iq = route_target_stanza_is_iq(&target);
        let reply = self
            .ask_remote_resource_origin(&remote_origin, target.clone())
            .await;
        match reply {
            Ok(reply) if reply.outcome == RemoteResourceRouteOutcome::StaleRegistration => {
                match self.refresh_remote_resource_origin(&remote_origin).await {
                    RemoteResourceOriginRefresh::Remote(refreshed) => {
                        match self.ask_remote_resource_origin(&refreshed, target).await {
                            Ok(reply) => Some(CapturedRemoteDeliveryOutcome {
                                outcome: reply.outcome.into(),
                                recipient_sm_append_streams: reply.recipient_sm_append_streams,
                            }),
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "clustered remote-resource origin route retry failed"
                                );
                                outcome_for_ask_error(&error, target_is_iq)
                                    .map(CapturedRemoteDeliveryOutcome::from_outcome)
                                    .or(Some(CapturedRemoteDeliveryOutcome::from_outcome(
                                        FullJidDeliveryOutcome::Dropped,
                                    )))
                            }
                        }
                    }
                    RemoteResourceOriginRefresh::LocalOwner => {
                        self.route_remote_resource_target_from_local_origin_with_capture(
                            &remote_origin,
                            target,
                        )
                        .await
                    }
                    RemoteResourceOriginRefresh::Failed => {
                        Some(CapturedRemoteDeliveryOutcome::from_outcome(
                            FullJidDeliveryOutcome::Unavailable,
                        ))
                    }
                }
            }
            Ok(reply) => Some(CapturedRemoteDeliveryOutcome {
                outcome: reply.outcome.into(),
                recipient_sm_append_streams: reply.recipient_sm_append_streams,
            }),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "clustered remote-resource origin route ask failed"
                );
                if ask_error_allows_target_refresh(&error) {
                    match self.refresh_remote_resource_origin(&remote_origin).await {
                        RemoteResourceOriginRefresh::Remote(refreshed) => {
                            return match self.ask_remote_resource_origin(&refreshed, target).await {
                                Ok(reply) => Some(CapturedRemoteDeliveryOutcome {
                                    outcome: reply.outcome.into(),
                                    recipient_sm_append_streams: reply.recipient_sm_append_streams,
                                }),
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        "clustered remote-resource origin route retry failed"
                                    );
                                    outcome_for_ask_error(&error, target_is_iq)
                                        .map(CapturedRemoteDeliveryOutcome::from_outcome)
                                        .or(Some(CapturedRemoteDeliveryOutcome::from_outcome(
                                            FullJidDeliveryOutcome::Dropped,
                                        )))
                                }
                            };
                        }
                        RemoteResourceOriginRefresh::LocalOwner => {
                            return self
                                .route_remote_resource_target_from_local_origin_with_capture(
                                    &remote_origin,
                                    target,
                                )
                                .await;
                        }
                        RemoteResourceOriginRefresh::Failed => {}
                    }
                }
                outcome_for_ask_error(&error, target_is_iq)
                    .map(CapturedRemoteDeliveryOutcome::from_outcome)
                    .or(Some(CapturedRemoteDeliveryOutcome::from_outcome(
                        FullJidDeliveryOutcome::Dropped,
                    )))
            }
        }
    }

    pub(in super::super) async fn route_remote_resource_origin_muc(
        self: Arc<Self>,
        remote_origin: RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
        _origin_stanza: &Stanza,
        _origin: &OrderedRelayRouteOrigin,
    ) -> Option<MucProxyRouteAttempt> {
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
    ) -> Option<MucProxyRouteAttempt> {
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
                            Ok(reply) => Some(MucProxyRouteAttempt {
                                relay_target: Some(RelayTargetIdentity::relay_node(
                                    refreshed.user_owner.as_str(),
                                )),
                                room_fence: None,
                                outcome: remote_resource_muc_outcome(reply),
                            }),
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "clustered remote-resource MUC origin route retry failed"
                                );
                                Some(MucProxyRouteAttempt {
                                    relay_target: Some(RelayTargetIdentity::relay_node(
                                        refreshed.user_owner.as_str(),
                                    )),
                                    room_fence: None,
                                    outcome: remote_resource_muc_ask_error_outcome(&target, &error),
                                })
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
                    RemoteResourceOriginRefresh::Failed => Some(MucProxyRouteAttempt {
                        relay_target: Some(RelayTargetIdentity::relay_node(
                            remote_origin.user_owner.as_str(),
                        )),
                        room_fence: None,
                        outcome: OrderedRelayMucProxyOutcome::Unavailable,
                    }),
                }
            }
            Ok(reply) => Some(MucProxyRouteAttempt {
                relay_target: Some(RelayTargetIdentity::relay_node(
                    remote_origin.user_owner.as_str(),
                )),
                room_fence: None,
                outcome: remote_resource_muc_outcome(reply),
            }),
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
                                Ok(reply) => Some(MucProxyRouteAttempt {
                                    relay_target: Some(RelayTargetIdentity::relay_node(
                                        refreshed.user_owner.as_str(),
                                    )),
                                    room_fence: None,
                                    outcome: remote_resource_muc_outcome(reply),
                                }),
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        "clustered remote-resource MUC origin route retry failed"
                                    );
                                    Some(MucProxyRouteAttempt {
                                        relay_target: Some(RelayTargetIdentity::relay_node(
                                            refreshed.user_owner.as_str(),
                                        )),
                                        room_fence: None,
                                        outcome: remote_resource_muc_ask_error_outcome(
                                            &target, &error,
                                        ),
                                    })
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
                Some(MucProxyRouteAttempt {
                    relay_target: Some(RelayTargetIdentity::relay_node(
                        remote_origin.user_owner.as_str(),
                    )),
                    room_fence: None,
                    outcome: remote_resource_muc_ask_error_outcome(&target, &error),
                })
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

    pub(in super::super) async fn route_remote_resource_target_from_local_origin_with_capture(
        self: &Arc<Self>,
        remote_origin: &RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Option<CapturedRemoteDeliveryOutcome> {
        let services = self.services.get().cloned()?;
        let origin = local_origin_for_remote_resource(remote_origin);
        match target {
            RemoteResourceRouteTarget::FullJid { target, stanza } => {
                if let Some(remote) = self
                    .try_deliver_full_jid_remote_with_capture(
                        &target, &stanza.0, &origin, None, None,
                    )
                    .await
                {
                    Some(remote)
                } else {
                    let outcome = deliver_local_full_jid_after_target_refresh_with_capture(
                        &services, &target, &stanza.0,
                    )
                    .await;
                    Some(CapturedRemoteDeliveryOutcome {
                        outcome: outcome.outcome,
                        recipient_sm_append_streams: outcome
                            .recipient_sm_append_stream
                            .into_iter()
                            .collect(),
                    })
                }
            }
            RemoteResourceRouteTarget::BareJid { target, stanza } => {
                match route_local_bare_jid_with_timeout(&services, &target, &stanza.0, Some(origin))
                    .await
                {
                    Ok(replies) if replies.is_empty() => {
                        Some(CapturedRemoteDeliveryOutcome::from_outcome(
                            FullJidDeliveryOutcome::Delivered,
                        ))
                    }
                    Ok(_) => Some(CapturedRemoteDeliveryOutcome::from_outcome(
                        FullJidDeliveryOutcome::Unavailable,
                    )),
                    Err(OrderedRelayNackReason::TargetUnavailable) => {
                        Some(CapturedRemoteDeliveryOutcome::from_outcome(
                            FullJidDeliveryOutcome::Unavailable,
                        ))
                    }
                    Err(_) => Some(CapturedRemoteDeliveryOutcome::from_outcome(
                        FullJidDeliveryOutcome::Dropped,
                    )),
                }
            }
            RemoteResourceRouteTarget::MucProxy { .. } => Some(
                CapturedRemoteDeliveryOutcome::from_outcome(FullJidDeliveryOutcome::Dropped),
            ),
        }
    }

    pub(in super::super) async fn route_remote_resource_muc_target_from_local_origin(
        self: &Arc<Self>,
        remote_origin: &RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Option<MucProxyRouteAttempt> {
        let services = self.services.get().cloned()?;
        let RemoteResourceRouteTarget::MucProxy {
            room_jid,
            kind,
            origin: muc_origin,
            stanza,
        } = target
        else {
            return Some(MucProxyRouteAttempt {
                relay_target: None,
                room_fence: None,
                outcome: OrderedRelayMucProxyOutcome::Dropped,
            });
        };
        let origin = local_origin_for_remote_resource(remote_origin);
        match self
            .try_proxy_muc_remote_from_local_origin_decision(
                &room_jid, &stanza.0, kind, muc_origin, &origin,
            )
            .await
        {
            MucProxyRouteDecision::Attempted(attempt) => Some(attempt),
            MucProxyRouteDecision::LocalRoom
            | MucProxyRouteDecision::RoomUnclaimed
            | MucProxyRouteDecision::RoomClaimUnavailable
            | MucProxyRouteDecision::OriginUnavailable => Some(MucProxyRouteAttempt {
                relay_target: None,
                room_fence: None,
                outcome: muc_proxy_result_to_ordered_outcome(
                    kind,
                    Box::pin(deliver_reserved_muc_proxy(
                        &services, &room_jid, kind, muc_origin, &stanza.0,
                    ))
                    .await,
                ),
            }),
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
                | crate::server::dual_registration::MirrorRegisterOutcome::Busy
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
