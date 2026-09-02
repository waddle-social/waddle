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
    let kind = outbound.kind;
    let frame = RemoteResourceOutboundFrame {
        jid: jid.clone(),
        registration_id,
        stanza: RemoteStanza(outbound.stanza),
        kind,
    };
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
    let mut handle = RelayHandle::new(socket_node.clone(), bridge.stop_token.clone())
        .with_ask_timeouts(bridge.mailbox_timeout, bridge.reply_timeout);
    let outcome = match handle
        .force_detach_remote_user_resource(RelayForceDetachRemoteUserResource {
            jid: jid.clone(),
            registration_id,
            origin: request.origin,
            requester_bare_jid: request.requester_bare_jid,
            trace: RelayTraceContext::default(),
        })
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
