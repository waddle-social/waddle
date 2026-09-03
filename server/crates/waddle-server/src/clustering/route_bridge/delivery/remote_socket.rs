use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegisteredRemoteWriteAcceptedDelivery {
    Delivered,
    Retryable,
    Absent,
    RefreshNeeded,
}

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
                    .try_deliver_full_jid_remote_with_capture(
                        &target, &stanza.0, &origin, None, None,
                    )
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
                    CapturedRemoteDeliveryOutcome::from_outcome(registered)
                } else {
                    let local = deliver_local_full_jid_after_target_refresh_with_capture(
                        &services, &target, &stanza.0,
                    )
                    .await;
                    CapturedRemoteDeliveryOutcome {
                        outcome: local.outcome,
                        recipient_sm_append_streams: local
                            .recipient_sm_append_stream
                            .into_iter()
                            .collect(),
                    }
                };
                RelayRouteRemoteResourceStanzaReply {
                    outcome: outcome.outcome.into(),
                    replies: Vec::new(),
                    recipient_sm_append_streams: outcome.recipient_sm_append_streams,
                }
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
                            recipient_sm_append_streams: Vec::new(),
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

    pub(crate) async fn try_deliver_registered_remote_resource_write_accepted(
        self: &Arc<Self>,
        target: &jid::FullJid,
        stanza: &Stanza,
    ) -> RegisteredRemoteWriteAcceptedDelivery {
        let Some(registration) = ({
            let registrations = self.remote_owner_resources.lock().await;
            registrations.get(target).cloned()
        }) else {
            return RegisteredRemoteWriteAcceptedDelivery::Absent;
        };
        match self
            .deliver_registered_remote_resource_write_accepted_with_registration(
                target,
                stanza,
                &registration,
            )
            .await
        {
            RegisteredRemoteWriteAcceptedDelivery::Delivered => {
                RegisteredRemoteWriteAcceptedDelivery::Delivered
            }
            RegisteredRemoteWriteAcceptedDelivery::Retryable => {
                RegisteredRemoteWriteAcceptedDelivery::Retryable
            }
            RegisteredRemoteWriteAcceptedDelivery::Absent => {
                RegisteredRemoteWriteAcceptedDelivery::Absent
            }
            RegisteredRemoteWriteAcceptedDelivery::RefreshNeeded => {
                // A stale registration means the destination's socket
                // lifecycle moved; an owner-side "refresh" is structurally
                // impossible (the owner mirror is rebuilt only by the socket
                // node's own re-registration ask). Keep the mirror intact and
                // retry: the row stays leased/released for the janitor and
                // the next pass observes the converged registration.
                RegisteredRemoteWriteAcceptedDelivery::Retryable
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
            write_acceptance: None,
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

    pub(crate) async fn deliver_remote_resource_write_accepted_frame_on_socket(
        &self,
        msg: RelayDeliverRemoteResourceWriteAcceptedFrame,
    ) -> RelayRemoteResourceWriteAcceptedReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceWriteAcceptedReply {
                status: RelayRemoteResourceWriteAcceptedStatus::Unavailable,
            };
        };
        let registration = {
            let registrations = self.remote_socket_resources.lock().await;
            match registrations.get(&msg.frame.jid) {
                None => {
                    return RelayRemoteResourceWriteAcceptedReply {
                        status: RelayRemoteResourceWriteAcceptedStatus::Unavailable,
                    };
                }
                Some(registration)
                    if registration.registration_id != msg.frame.registration_id
                        || registration.socket_generation != msg.frame.socket_generation =>
                {
                    return RelayRemoteResourceWriteAcceptedReply {
                        status: RelayRemoteResourceWriteAcceptedStatus::StaleRegistration,
                    };
                }
                Some(registration) => registration.clone(),
            }
        };
        let (acceptance, receiver) = OutboundWriteAcceptance::new();
        let outbound = OutboundStanza::with_write_acceptance(msg.frame.stanza.0, acceptance);
        let outcome = services.connection_registry.try_send_outbound_if_owner(
            &msg.frame.jid,
            &registration.owner,
            outbound,
        );
        let status = match outcome {
            BroadcastOutcome::Delivered => match tokio::time::timeout(
                self.remote_resource_write_accepted_acceptance_timeout(),
                receiver,
            )
            .await
            {
                Ok(Ok(())) => RelayRemoteResourceWriteAcceptedStatus::WriteAccepted,
                Ok(Err(_)) => RelayRemoteResourceWriteAcceptedStatus::AcceptanceClosed,
                Err(_) => RelayRemoteResourceWriteAcceptedStatus::AcceptancePending,
            },
            BroadcastOutcome::DroppedFull => {
                RelayRemoteResourceWriteAcceptedStatus::AcceptancePending
            }
            BroadcastOutcome::NotConnected | BroadcastOutcome::DroppedClosed => {
                RelayRemoteResourceWriteAcceptedStatus::AcceptanceClosed
            }
        };
        RelayRemoteResourceWriteAcceptedReply { status }
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
                super::telemetry::record_remote_resource_delivered(stanza);
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

    async fn deliver_registered_remote_resource_write_accepted_with_registration(
        &self,
        target: &jid::FullJid,
        stanza: &Stanza,
        registration: &RemoteOwnerRegistration,
    ) -> RegisteredRemoteWriteAcceptedDelivery {
        let mut handle =
            RelayHandle::new(registration.socket_node.clone(), self.stop_token.clone())
                .with_ask_timeouts(
                    self.mailbox_timeout,
                    self.remote_resource_write_accepted_reply_timeout(),
                );
        match handle
            .deliver_remote_resource_write_accepted_frame(
                RelayDeliverRemoteResourceWriteAcceptedFrame {
                    frame: RemoteResourceWriteAcceptedOutboundFrame {
                        jid: target.clone(),
                        registration_id: registration.registration_id,
                        socket_generation: registration.socket_generation,
                        stanza: RemoteStanza(stanza.clone()),
                    },
                    trace: RelayTraceContext::default(),
                },
            )
            .await
        {
            Ok(RelayRemoteResourceWriteAcceptedReply {
                status: RelayRemoteResourceWriteAcceptedStatus::WriteAccepted,
            }) => {
                super::telemetry::record_remote_resource_delivered(stanza);
                tracing::debug!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    outcome = ?FullJidDeliveryOutcome::Delivered,
                    "clustered remote-resource write-accepted delivery outcome"
                );
                RegisteredRemoteWriteAcceptedDelivery::Delivered
            }
            Ok(RelayRemoteResourceWriteAcceptedReply {
                status:
                    RelayRemoteResourceWriteAcceptedStatus::AcceptanceClosed
                    | RelayRemoteResourceWriteAcceptedStatus::AcceptancePending,
            }) => {
                tracing::debug!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    "clustered remote-resource write-accepted delivery requires retry"
                );
                RegisteredRemoteWriteAcceptedDelivery::Retryable
            }
            Ok(RelayRemoteResourceWriteAcceptedReply {
                status: RelayRemoteResourceWriteAcceptedStatus::StaleRegistration,
            }) => RegisteredRemoteWriteAcceptedDelivery::RefreshNeeded,
            Ok(RelayRemoteResourceWriteAcceptedReply {
                status: RelayRemoteResourceWriteAcceptedStatus::Unavailable,
            }) => {
                tracing::debug!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    outcome = ?FullJidDeliveryOutcome::Unavailable,
                    "clustered remote-resource write-accepted delivery outcome"
                );
                self.cleanup_remote_owner_resource_if_registration(
                    target,
                    registration.registration_id,
                )
                .await;
                RegisteredRemoteWriteAcceptedDelivery::Absent
            }
            Err(error) => {
                tracing::warn!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    %error,
                    "clustered remote-resource write-accepted relay ask failed"
                );
                RegisteredRemoteWriteAcceptedDelivery::Retryable
            }
        }
    }
}
