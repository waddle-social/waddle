use super::*;

impl OrderedRelayDeliveryBridge {
    /// Receiver-side effect for one already-reserved envelope. The caller
    /// commits the reservation only when this returns `Ok(())`.
    pub(crate) async fn deliver_reserved(
        &self,
        envelope: &RemoteStanzaEnvelope,
        completion: &mut Option<RelayFrameCompletion>,
    ) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
        let Some(services) = self.services.get().cloned() else {
            return Err(OrderedRelayNackReason::Unreachable);
        };
        validate_claims(&services, envelope).await?;
        match relay_payload_target(envelope)? {
            RelayPayloadTarget::Full(target, stanza) => {
                let obligation = match &envelope.payload {
                    OrderedRelayPayload::Message { ingress_append, .. }
                    | OrderedRelayPayload::ProcessedDirectMessage { ingress_append, .. } => {
                        ingress_append.as_ref()
                    }
                    _ => None,
                };
                // Authorize unconditionally. Deciding from a detached-session
                // probe here is unsound: teardown inserts the detached session
                // during cleanup, so a resource can be live at the probe and
                // detached by the time `deliver_peer_to_full` falls back to the
                // queue, and that append would then be unkeyed (#1790).
                let ingress_append_context = super::ingress_append::authorize_ingress_append(
                    &services,
                    &envelope.sender_claim.entity,
                    stanza,
                    obligation,
                )
                .await;
                if matches!(
                    envelope.payload,
                    OrderedRelayPayload::ProcessedDirectMessage { .. }
                ) {
                    if obligation.is_some() && ingress_append_context.is_none() {
                        return Err(OrderedRelayNackReason::TargetUnavailable);
                    }
                    return self
                        .deliver_processed_resource(
                            &services,
                            target,
                            stanza,
                            ingress_append_context.as_ref(),
                        )
                        .await
                        .map(|()| Vec::new());
                }
                self.deliver_reserved_full_jid(
                    &services,
                    target,
                    stanza,
                    ingress_append_context.as_ref(),
                )
                .await
                .map(|()| Vec::new())
            }
            RelayPayloadTarget::Bare(target, stanza) => {
                deliver_reserved_bare_jid(&services, &target, stanza)
                    .await
                    .map(|()| Vec::new())
            }
            RelayPayloadTarget::Muc(room, kind, origin, stanza, admission) => {
                deliver_reserved_muc_proxy(
                    &services,
                    room,
                    kind,
                    origin,
                    stanza,
                    admission.as_ref(),
                    completion,
                )
                .await
            }
        }
    }
}

pub(in super::super) async fn deliver_local_after_target_refresh_outcome(
    services: &OrderedRelayDeliveryServices,
    target: &jid::Jid,
    stanza: &Stanza,
    payload: &OrderedRelayPayload,
    ingress_append_context: Option<&crate::server::routes::interpret::SmIngressAppendContext>,
) -> RemoteDeliveryOutcome {
    match payload {
        OrderedRelayPayload::MucProxy {
            canonical,
            principal,
            stanza_lang,
            room_jid,
            kind,
            origin,
            stanza,
        } => {
            let mut completion = None;
            let mut outcome = muc_proxy_result_to_outcome(
                Box::pin(deliver_reserved_muc_proxy(
                    services,
                    room_jid,
                    *kind,
                    *origin,
                    &stanza.0,
                    crate::ingress::identity::IngressRelayAdmission::from_parts(
                        canonical.clone(),
                        principal.clone(),
                        stanza_lang.clone(),
                    )
                    .as_ref(),
                    &mut completion,
                ))
                .await,
            );
            outcome.frame_completion =
                completion.map(crate::ingress::execute::RelayFrameReceiptCompletion::new);
            outcome
        }
        OrderedRelayPayload::ProcessedDirectMessage { .. } => {
            let outcome = match target.clone().try_into_full() {
                Ok(full) => match services.web_socket_state.upgrade() {
                    Some(state) => {
                        let mut deps = build_interpret_deps(state.as_ref(), None);
                        deps.ingress_append_context = ingress_append_context.cloned();
                        crate::server::routes::interpret::deliver_direct_to_full_with_registered_remote(
                            &deps, &full, stanza,
                        ).await
                    }
                    None => FullJidDeliveryOutcome::Unavailable,
                },
                Err(_) => FullJidDeliveryOutcome::Unavailable,
            };
            no_client_reply_outcome(outcome)
        }
        OrderedRelayPayload::Message { .. }
        | OrderedRelayPayload::Iq { .. }
        | OrderedRelayPayload::Presence { .. } => no_client_reply_outcome(
            deliver_local_after_target_refresh(services, target, stanza, ingress_append_context)
                .await,
        ),
    }
}
pub(in super::super) async fn deliver_local_after_target_refresh(
    services: &OrderedRelayDeliveryServices,
    target: &jid::Jid,
    stanza: &Stanza,
    ingress_append_context: Option<&crate::server::routes::interpret::SmIngressAppendContext>,
) -> FullJidDeliveryOutcome {
    match target.clone().try_into_full() {
        Ok(full) => {
            deliver_local_full_jid_after_target_refresh(
                services,
                &full,
                stanza,
                ingress_append_context,
            )
            .await
        }
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

pub(in super::super) async fn deliver_local_full_jid_after_target_refresh(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    stanza: &Stanza,
    ingress_append_context: Option<&crate::server::routes::interpret::SmIngressAppendContext>,
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
        ingress_append_context,
    )
    .await
}
