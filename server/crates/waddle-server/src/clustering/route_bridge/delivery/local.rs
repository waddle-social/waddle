use super::*;

impl OrderedRelayDeliveryBridge {
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
}

pub(in super::super) async fn deliver_local_after_target_refresh_outcome(
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
pub(in super::super) async fn deliver_local_after_target_refresh(
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

pub(in super::super) async fn deliver_local_full_jid_after_target_refresh(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    deliver_local_full_jid_after_target_refresh_with_capture(services, target, stanza)
        .await
        .outcome
}

pub(in super::super) async fn deliver_local_full_jid_after_target_refresh_with_capture(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> crate::server::routes::interpret::DetachedDeliveryCapture {
    if matches!(stanza, Stanza::Iq(_)) {
        return match deliver_reserved_full_jid_peer_live_only(services, target, stanza).await {
            Ok(()) => crate::server::routes::interpret::DetachedDeliveryCapture::from_outcome(
                FullJidDeliveryOutcome::Delivered,
            ),
            Err(OrderedRelayNackReason::TargetUnavailable) => {
                crate::server::routes::interpret::DetachedDeliveryCapture::from_outcome(
                    FullJidDeliveryOutcome::Unavailable,
                )
            }
            Err(_) => crate::server::routes::interpret::DetachedDeliveryCapture::from_outcome(
                FullJidDeliveryOutcome::Dropped,
            ),
        };
    }
    crate::server::routes::interpret::deliver_peer_to_full_with_detached_capture(
        Some(&services.user_registry),
        Some(&services.sm_session_registry),
        target,
        stanza,
    )
    .await
}
