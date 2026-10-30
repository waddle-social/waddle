use waddle_xmpp::protocol::handlers::errors::NS_BLOCKING_ERRORS;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::{
    build_iq_error_xml_typed, internal_server_error_iq_error, not_authorized_iq_error,
    service_unavailable_iq_error, stanza_to_xml, warn, BareJid, DatabaseBlockingStorage, FullJid,
    Jid, Stanza, WebSocketState,
};
use crate::db::blocking::BlockingStorageError;
#[cfg(feature = "clustering")]
use crate::server::routes::interpret::FullJidDeliveryOutcome;
use crate::server::routes::interpret::OrderedRelayRouteOrigin;

enum BlockDirection {
    Outgoing,
    Incoming,
}

async fn blocked_direction(
    storage: &DatabaseBlockingStorage,
    sender: &FullJid,
    target: &FullJid,
) -> Result<Option<BlockDirection>, BlockingStorageError> {
    // XEP-0191 note 8: a block rule cannot separate a user's own resources.
    if target.to_bare() == sender.to_bare() {
        return Ok(None);
    }
    if storage
        .is_blocked_jid(&sender.to_bare(), &Jid::from(target.clone()))
        .await?
    {
        return Ok(Some(BlockDirection::Outgoing));
    }
    if storage
        .is_blocked_jid(&target.to_bare(), &Jid::from(sender.clone()))
        .await?
    {
        return Ok(Some(BlockDirection::Incoming));
    }
    Ok(None)
}

fn outgoing_block_frames(iq: &Iq, sender: &FullJid, target: &FullJid) -> Vec<String> {
    // RFC 6120 §8.2.3 prohibits answering result/error IQs with another IQ.
    let payload = match iq {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => payload,
        Iq::Result { .. } | Iq::Error { .. } => return Vec::new(),
    };
    let mut error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::NotAcceptable,
        "en",
        "Recipient is on your blocklist.",
    );
    error.other = Some(Element::builder("blocked", NS_BLOCKING_ERRORS).build());
    error.by = Some(Jid::from(BareJid::from_parts(None, sender.domain())));
    vec![stanza_to_xml(&Stanza::Iq(Box::new(Iq::Error {
        from: Some(Jid::from(target.clone())),
        to: Some(Jid::from(sender.clone())),
        id: iq.id().into(),
        payload: Some(payload.clone()),
        error,
    })))]
}

pub(super) async fn route_full_jid_iq(
    mut iq: xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    target: FullJid,
    response_from: Option<&str>,
    ordered_relay_origin: Option<OrderedRelayRouteOrigin>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            None,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let is_request = matches!(
        &iq,
        xmpp_parsers::iq::Iq::Get { .. } | xmpp_parsers::iq::Iq::Set { .. }
    );
    let blocking = DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone());
    match blocked_direction(&blocking, sender_jid, &target).await {
        Ok(Some(BlockDirection::Outgoing)) => {
            return outgoing_block_frames(&iq, sender_jid, &target)
        }
        Ok(Some(BlockDirection::Incoming)) => {
            // XEP-0191 §3.3: blocked IQ replies are silently discarded.
            if !is_request {
                return Vec::new();
            }
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                Some(sender_jid.as_str()),
                service_unavailable_iq_error("Service unavailable at this address."),
            )];
        }
        Ok(None) => {}
        Err(error) => {
            warn!(error = %error, target = %target, sender = %sender_jid, "Failed to check blocklist before routing IQ");
            // RFC 6120 §8.2.3: never answer a result/error with another IQ.
            if !is_request {
                return Vec::new();
            }
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
            .try_deliver_full_jid_remote(&target, &stanza, origin, None, None)
            .await
        {
            return match outcome {
                FullJidDeliveryOutcome::Delivered
                | FullJidDeliveryOutcome::QueuedDetached
                | FullJidDeliveryOutcome::Dropped => Vec::new(),
                #[cfg(feature = "clustering")]
                FullJidDeliveryOutcome::MaybeCommitted => Vec::new(),
                FullJidDeliveryOutcome::Unavailable => fallback_iq_frames(&iq, state),
            };
        }
        if let Some(outcome) = bridge
            .try_deliver_registered_remote_resource(
                &target,
                &stanza,
                waddle_xmpp::registry::DeliveryKind::PeerStanza,
                None,
            )
            .await
        {
            return match outcome {
                FullJidDeliveryOutcome::Delivered
                | FullJidDeliveryOutcome::QueuedDetached
                | FullJidDeliveryOutcome::Dropped => Vec::new(),
                #[cfg(feature = "clustering")]
                FullJidDeliveryOutcome::MaybeCommitted => Vec::new(),
                FullJidDeliveryOutcome::Unavailable => fallback_iq_frames(&iq, state),
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
        | waddle_xmpp::registry::SendResult::ChannelClosed => fallback_iq_frames(&iq, state),
    }
}

fn fallback_iq_frames(iq: &xmpp_parsers::iq::Iq, state: &WebSocketState) -> Vec<String> {
    let stanza = Stanza::Iq(Box::new(iq.clone()));
    let Some(reply) = crate::server::routes::interpret::bounce_undeliverable_iq(
        &stanza,
        state.deps.protocol.sfu.as_deref(),
    ) else {
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
