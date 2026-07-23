use super::*;

pub(super) async fn route_full_jid_iq(
    mut iq: xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    target: FullJid,
    response_from: Option<&str>,
    ordered_relay_origin: Option<crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            None,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let blocking = DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone());
    match blocking
        .is_blocked_jid(&target.to_bare(), &Jid::from(sender_jid.clone()))
        .await
    {
        Ok(true) => {
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                Some(sender_jid.as_str()),
                service_unavailable_iq_error("Service unavailable at this address."),
            )];
        }
        Ok(false) => {}
        Err(error) => {
            warn!(error = %error, target = %target, sender = %sender_jid, "Failed to check blocklist before routing IQ");
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                Some(sender_jid.as_str()),
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    }
    *iq.from_mut() = Some(Jid::from(sender_jid.clone()));
    *iq.to_mut() = Some(Jid::from(target.clone()));
    let stanza = Stanza::Iq(Box::new(iq.clone()));
    #[cfg(feature = "clustering")]
    if let (Some(origin), Some(bridge)) = (
        ordered_relay_origin.as_ref(),
        state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref(),
    ) {
        if let Some(outcome) = bridge
            .try_deliver_full_jid_remote(&target, &stanza, origin)
            .await
        {
            if outcome.is_ambiguous() {
                state.deps.room_serving.mark_unsafe_to_release();
            }
            return match outcome {
                crate::server::routes::interpret::FullJidDeliveryOutcome::Delivered
                | crate::server::routes::interpret::FullJidDeliveryOutcome::QueuedDetached
                | crate::server::routes::interpret::FullJidDeliveryOutcome::Dropped
                | crate::server::routes::interpret::FullJidDeliveryOutcome::MaybeEnqueued => {
                    Vec::new()
                }
                #[cfg(feature = "clustering")]
                crate::server::routes::interpret::FullJidDeliveryOutcome::MaybeCommitted => {
                    Vec::new()
                }
                crate::server::routes::interpret::FullJidDeliveryOutcome::Unavailable => {
                    fallback_iq_frames(&iq)
                }
            };
        }
        if let Some(outcome) = bridge
            .try_deliver_registered_remote_resource(
                &target,
                &stanza,
                waddle_xmpp::registry::DeliveryKind::PeerStanza,
            )
            .await
        {
            if outcome.is_ambiguous() {
                state.deps.room_serving.mark_unsafe_to_release();
            }
            return match outcome {
                crate::server::routes::interpret::FullJidDeliveryOutcome::Delivered
                | crate::server::routes::interpret::FullJidDeliveryOutcome::QueuedDetached
                | crate::server::routes::interpret::FullJidDeliveryOutcome::Dropped
                | crate::server::routes::interpret::FullJidDeliveryOutcome::MaybeEnqueued => {
                    Vec::new()
                }
                #[cfg(feature = "clustering")]
                crate::server::routes::interpret::FullJidDeliveryOutcome::MaybeCommitted => {
                    Vec::new()
                }
                crate::server::routes::interpret::FullJidDeliveryOutcome::Unavailable => {
                    fallback_iq_frames(&iq)
                }
            };
        }
    }
    #[cfg(not(feature = "clustering"))]
    let _ = ordered_relay_origin;
    match state
        .deps
        .protocol
        .connection_registry
        .send_to(&target, stanza)
        .await
    {
        waddle_xmpp::registry::SendResult::Sent => Vec::new(),
        waddle_xmpp::registry::SendResult::NotConnected
        | waddle_xmpp::registry::SendResult::ChannelClosed => fallback_iq_frames(&iq),
    }
}

fn fallback_iq_frames(iq: &xmpp_parsers::iq::Iq) -> Vec<String> {
    let stanza = Stanza::Iq(Box::new(iq.clone()));
    let Some(reply) =
        crate::server::routes::interpret::fallback_reply_for_undeliverable_iq(&stanza)
    else {
        return Vec::new();
    };
    let serialized = match reply {
        Stanza::Iq(reply) => waddle_xmpp::parser::stanza_to_string(*reply),
        Stanza::Message(reply) => waddle_xmpp::parser::stanza_to_string(reply),
        Stanza::Presence(reply) => waddle_xmpp::parser::stanza_to_string(reply),
    };
    match serialized {
        Ok(xml) => vec![xml],
        Err(error) => {
            warn!(%error, "failed to serialize full-JID IQ fallback reply");
            Vec::new()
        }
    }
}
