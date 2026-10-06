use super::super::*;

impl OrderedRelayDeliveryBridge {
    pub(super) fn spawn_remote_resource_forwarder(
        self: &Arc<Self>,
        jid: jid::FullJid,
        registration_id: RemoteResourceRegistrationId,
        socket_generation: RemoteResourceSocketGeneration,
        socket_node: NodeId,
        mut rx: mpsc::Receiver<OutboundStanza>,
        force_detach_rx: Option<mpsc::Receiver<ForceDetachRequest>>,
    ) {
        let outbound_bridge = Arc::clone(self);
        let outbound_jid = jid.clone();
        let outbound_socket_node = socket_node.clone();
        tokio::spawn(async move {
            while let Some(outbound) = rx.recv().await {
                forward_remote_resource_outbound(
                    &outbound_bridge,
                    &outbound_jid,
                    registration_id,
                    socket_generation,
                    &outbound_socket_node,
                    outbound,
                )
                .await;
            }
        });
        if let Some(mut force_detach_rx) = force_detach_rx {
            let control_bridge = Arc::clone(self);
            tokio::spawn(async move {
                while let Some(request) = force_detach_rx.recv().await {
                    forward_remote_resource_force_detach(
                        &control_bridge,
                        &jid,
                        registration_id,
                        &socket_node,
                        request,
                    )
                    .await;
                }
            });
        }
    }
}

/// Relays outbound stanzas for a remotely owned resource to the socket node.
async fn forward_remote_resource_outbound(
    bridge: &Arc<OrderedRelayDeliveryBridge>,
    jid: &jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    socket_generation: RemoteResourceSocketGeneration,
    socket_node: &NodeId,
    mut outbound: OutboundStanza,
) {
    if outbound.pending_row_id.is_some() {
        tracing::warn!(
            jid = %jid,
            "clustered remote-resource forwarder received pending-delivery \
             flush frame; dropping to avoid breaking SM row ack accounting"
        );
        return;
    }
    // A frame carrying a write acceptance is a durable effect that reached
    // this owner-mirror node's PROXY entry via the local queue path: the
    // legacy enqueue-only ask would drop the acceptance (never acknowledged,
    // origin redelivers forever). Carry it end to end on the write-accepted
    // ask and acknowledge only on the destination writer's acceptance.
    if let Some(acceptance) = outbound.write_acceptance.take() {
        let mut handle = RelayHandle::new(socket_node.clone(), bridge.stop_token.clone())
            .with_ask_timeouts(
                bridge.mailbox_timeout,
                bridge.remote_resource_write_accepted_reply_timeout(),
            );
        match handle
            .deliver_remote_resource_write_accepted_frame(
                RelayDeliverRemoteResourceWriteAcceptedFrame {
                    frame: RemoteResourceWriteAcceptedOutboundFrame {
                        jid: jid.clone(),
                        registration_id,
                        socket_generation,
                        stanza: RemoteStanza(outbound.stanza),
                    },
                    trace: RelayTraceContext::default(),
                },
            )
            .await
        {
            Ok(RelayRemoteResourceWriteAcceptedReply {
                status: RelayRemoteResourceWriteAcceptedStatus::WriteAccepted,
            }) => {
                acceptance.acknowledge();
            }
            Ok(RelayRemoteResourceWriteAcceptedReply {
                status: RelayRemoteResourceWriteAcceptedStatus::Unavailable,
            }) => {
                tracing::debug!(
                    jid = %jid,
                    "write-accepted forward found the socket registration gone; cleaning owner mirror"
                );
                bridge
                    .cleanup_remote_owner_resource_if_registration(jid, registration_id)
                    .await;
                drop(acceptance);
            }
            Ok(_) | Err(_) => {
                // AcceptanceClosed/Pending/StaleRegistration or an ask
                // failure: drop the acceptance unacknowledged — the closed
                // oneshot tells the origin to retry (at-least-once).
                drop(acceptance);
            }
        }
        return;
    }
    let frame = remote_resource_outbound_frame(jid, registration_id, &outbound);
    let mut handle = RelayHandle::new(socket_node.clone(), bridge.stop_token.clone())
        .with_ask_timeouts(bridge.mailbox_timeout, bridge.reply_timeout);
    match handle
        .deliver_remote_resource_frame(RelayDeliverRemoteResourceFrame {
            frame,
            trace: RelayTraceContext::default(),
        })
        .await
    {
        Ok(RelayRemoteResourceFrameReply {
            status: RelayRemoteResourceFrameStatus::Delivered,
        }) => {}
        Ok(RelayRemoteResourceFrameReply {
            status: RelayRemoteResourceFrameStatus::Unavailable,
        }) => {
            tracing::debug!(
                jid = %jid,
                "clustered remote-resource socket registration unavailable; cleaning owner mirror"
            );
            bridge
                .cleanup_remote_owner_resource_if_registration(jid, registration_id)
                .await;
        }
        Ok(reply) => {
            tracing::debug!(
                jid = %jid,
                status = ?reply.status,
                "clustered remote-resource forwarder did not deliver frame"
            );
        }
        Err(error) => {
            tracing::warn!(
                jid = %jid,
                %error,
                "clustered remote-resource forwarder relay ask failed"
            );
            if ask_error_proves_remote_resource_ref_stale(&error) {
                bridge
                    .cleanup_remote_owner_resource_if_registration(jid, registration_id)
                    .await;
            }
        }
    }
}

/// Relays a force-detach request to the socket node and acknowledges outcome.
async fn forward_remote_resource_force_detach(
    bridge: &Arc<OrderedRelayDeliveryBridge>,
    jid: &jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    socket_node: &NodeId,
    request: ForceDetachRequest,
) {
    // Socket cleanup unregisters on this owner before acknowledging detach.
    // Release the registration map before waiting for that acknowledgement.
    let occupancy_session = {
        let registrations = bridge.remote_owner_resources.lock().await;
        registrations
            .get(jid)
            .filter(|registration| registration.registration_id == registration_id)
            .map(|registration| registration.occupancy_session)
    };
    let mut handle = RelayHandle::new(socket_node.clone(), bridge.stop_token.clone())
        .with_ask_timeouts(bridge.mailbox_timeout, bridge.reply_timeout);
    let outcome = match send_force_detach(
        &mut handle,
        RelayForceDetachRemoteUserResource {
            jid: jid.clone(),
            registration_id,
            occupancy_session,
            origin: request.origin,
            requester_bare_jid: request.requester_bare_jid,
            trace: RelayTraceContext::default(),
        },
    )
    .await
    {
        Ok(reply) => reply.outcome,
        Err(error) => {
            tracing::warn!(
                jid = %jid,
                %error,
                "clustered remote-resource force-detach relay ask failed"
            );
            ForceDetachOutcome::NotPersisted
        }
    };
    let _ = request.ack.send(outcome);
}

async fn send_force_detach(
    handle: &mut RelayHandle,
    message: RelayForceDetachRemoteUserResource,
) -> Result<RelayForceDetachRemoteUserResourceReply, RelayAskError> {
    #[cfg(test)]
    if let Ok(sender) = tests::FORCE_DETACH_RELAY.try_with(Clone::clone) {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        sender
            .send((message, reply))
            .await
            .expect("test relay receives force-detach");
        return Ok(receiver.await.expect("test relay replies to force-detach"));
    }
    handle.force_detach_remote_user_resource(message).await
}

/// Build the owner-to-socket wire metadata without consuming delivery ownership.
pub(in super::super) fn remote_resource_outbound_frame(
    jid: &jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    outbound: &OutboundStanza,
) -> RemoteResourceOutboundFrame {
    let kind = outbound.kind;
    let ingress_append = outbound
        .ingress_append
        .clone()
        .map(crate::ingress::identity::IngressAppendObligationRef::from_relayed);
    // Keyed authority already carries the authoritative receipt time. Keep the
    // timestamp-only live wire separate so keyed frames retain their baseline.
    let received_at = if ingress_append.is_none() {
        outbound.original_receipt_at
    } else {
        None
    };
    RemoteResourceOutboundFrame {
        jid: jid.clone(),
        registration_id,
        stanza: RemoteStanza(outbound.stanza.clone()),
        kind,
        ingress_append,
        received_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;
    use tokio::time::{timeout, Duration};
    use tokio_util::sync::CancellationToken;
    use waddle_xmpp::registry::ForceDetachOrigin;
    use waddle_xmpp_core::OccupancySessionGeneration;

    tokio::task_local! {
        pub(super) static FORCE_DETACH_RELAY: mpsc::Sender<(
            RelayForceDetachRemoteUserResource,
            oneshot::Sender<RelayForceDetachRemoteUserResourceReply>,
        )>;
    }

    #[tokio::test]
    async fn force_detach_forwarder_allows_owner_unregister_before_socket_ack() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let jid: jid::FullJid = "alice@example.test/phone".parse().expect("full jid");
        let socket_node = NodeId::new("socket".to_owned());
        let registration_id = RemoteResourceRegistrationId::fresh();
        let generation = OccupancySessionGeneration::mint();
        bridge.remote_owner_resources.lock().await.insert(
            jid.clone(),
            RemoteOwnerRegistration {
                occupancy_session: generation,
                socket_identity: NodeIdentity::new("socket", "epoch"),
                unregister_pending: false,
                registration_id,
                socket_node: socket_node.clone(),
                socket_generation: RemoteResourceSocketGeneration::next(None),
                owner: Arc::new(AtomicBool::new(false)),
            },
        );
        let (relay_tx, mut relay_rx) = mpsc::channel(1);
        let (ack, ack_rx) = oneshot::channel();
        let forwarder = tokio::spawn(FORCE_DETACH_RELAY.scope(relay_tx, {
            let bridge = Arc::clone(&bridge);
            let jid = jid.clone();
            async move {
                forward_remote_resource_force_detach(
                    &bridge,
                    &jid,
                    registration_id,
                    &socket_node,
                    ForceDetachRequest {
                        origin: ForceDetachOrigin::OwnerManagedRetirement,
                        requester_bare_jid: jid.to_bare(),
                        ack,
                    },
                )
                .await;
            }
        }));
        let (request, reply) = timeout(Duration::from_secs(1), relay_rx.recv())
            .await
            .expect("forwarder reaches the relay ask")
            .expect("force-detach request");
        assert_eq!(request.occupancy_session, Some(generation));
        assert_eq!(request.registration_id, registration_id);

        // The socket must unregister on this owner before it can acknowledge
        // detach. Keep the relay reply pending while completing that step.
        let retired = timeout(Duration::from_secs(1), async {
            bridge.remote_owner_resources.lock().await.remove(&jid)
        })
        .await
        .expect("owner registration map must be available before the socket ACK")
        .expect("incumbent registration remains until unregister");
        assert_eq!(retired.occupancy_session, generation);
        reply
            .send(RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::Detached,
                status: RelayRemoteResourceForceDetachStatus::Detached,
            })
            .expect("forwarder still awaits the socket ACK");
        assert_eq!(
            ack_rx.await.expect("forwarded ACK"),
            ForceDetachOutcome::Detached
        );
        forwarder.await.expect("forwarder completes");
    }
}
