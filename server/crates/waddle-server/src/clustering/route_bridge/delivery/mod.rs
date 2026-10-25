use super::{
    ask_error_allows_target_refresh, ask_error_is_unseen, ask_error_maybe_committed,
    ask_error_proves_remote_resource_ref_stale, build_interpret_deps, caller_delivery_outcome,
    channel_diversion_for_ask_error, definite_no_effect_outcome, log_remote_resource_route_outcome,
    outcome_for_ask_error, outcome_for_nack, owner_remote_entry_if_current,
    replies_for_origin_handoff, route_outcome_log, route_target_stanza_is_iq, stanza_message_id,
    validate_claims, Arc, AtomicBool, BroadcastOutcome, CancellationToken, ClaimSnapshot,
    ClaimStore, DeliveryKind, Entity, EntityType, FullJidDeliveryOutcome, MucProxyOrigin, Mutex,
    NackChannelAction, NodeId, NodeIdentity, OrderedRelayChannel, OrderedRelayClaim,
    OrderedRelayClaimRole, OrderedRelayDeliveryBridge, OrderedRelayDeliveryServices,
    OrderedRelayDiversion, OrderedRelayDiversionReason, OrderedRelayEnvelopeClaims,
    OrderedRelayMucProxyKind, OrderedRelayNack, OrderedRelayNackReason, OrderedRelayOrigin,
    OrderedRelayOriginProof, OrderedRelayPayload, OrderedRelayRecipient, OrderedRelayReply,
    OrderedRelayRouteOrigin, OrderedRelayRouteOriginKind, OriginInboundSequence, OutboundStanza,
    OutboundWriteAcceptance, PreparedRemoteDelivery, RelayAskError,
    RelayDeliverRemoteResourceFrame, RelayDeliverRemoteResourceWriteAcceptedFrame, RelayHandle,
    RelayRemoteResourceFrameReply, RelayRemoteResourceFrameStatus, RelayRemoteResourceUpdateStatus,
    RelayRemoteResourceWriteAcceptedReply, RelayRemoteResourceWriteAcceptedStatus,
    RelayRouteRemoteResourceStanza, RelayRouteRemoteResourceStanzaReply, RelayTraceContext,
    RemoteDeliveryFuture, RemoteDeliveryOutcome, RemoteDeliverySeed, RemoteOwnerRegistration,
    RemoteResourceOriginRefresh, RemoteResourceOriginSnapshot, RemoteResourceOutboundFrame,
    RemoteResourceRegisterOutcome, RemoteResourceRegistrationId, RemoteResourceRouteOutcome,
    RemoteResourceRouteTarget, RemoteResourceWriteAcceptedOutboundFrame, RemoteStanza,
    RemoteStanzaEnvelope, Stanza, WebSocketState, MAX_ORDERED_RELAY_CHANNEL_LOCKS,
    ORDERED_DELIVERY_MAILBOX_TIMEOUT, ORDERED_RECEIVER_DELIVERY_TIMEOUT,
};
use crate::auth::Session;
use crate::ingress::execute::RelayFrameReceiptCompletion;
use crate::server::routes::interpret::OrderedRelayHandoffHandle;

pub(super) mod channels;
pub(super) mod ingress_append;
pub(super) mod local;
pub(super) mod muc;
pub(super) mod ordered;
pub(super) mod ordered_send;
pub(super) mod receiver;
pub(super) mod remote;
pub(super) mod remote_route_helpers;
pub(super) mod remote_socket;
pub(super) mod telemetry;

pub(super) use local::*;
pub(crate) use muc::reserved::RelayFrameCompletion;
pub(super) use muc::reserved::{
    deliver_reserved_muc_proxy, muc_proxy_result_to_ordered_outcome, muc_proxy_result_to_outcome,
};
#[cfg(test)]
pub(crate) use muc::MucProxyRouteAttempt;
pub(crate) use muc::{MucProxyRouteDecision, OrderedRelayMucProxyOutcome};
use receiver::{
    current_claim, deliver_reserved_bare_jid, deliver_reserved_full_jid_peer_live_only,
    rebind_stanza_sender, relay_payload_target, room_entity, route_local_bare_jid_with_timeout,
    user_entity, RelayPayloadTarget,
};
pub(super) use remote_route_helpers::*;
pub(crate) use remote_socket::RegisteredRemoteWriteAcceptedDelivery;

/// Message responsibility is already committed before relay execution. IQ and
/// presence retain socket-acceptance completion as their handled boundary.
fn defer_until_relay_completion(handoff: &OrderedRelayHandoffHandle, stanza: &Stanza) -> bool {
    !matches!(stanza, Stanza::Message(_)) && handoff.mark_deferred()
}

pub(super) fn no_client_reply_outcome(delivery: FullJidDeliveryOutcome) -> RemoteDeliveryOutcome {
    no_client_reply_outcome_with_commit_state(delivery, false)
}

pub(super) fn remote_resource_route_reply(
    outcome: RemoteResourceRouteOutcome,
) -> RelayRouteRemoteResourceStanzaReply {
    RelayRouteRemoteResourceStanzaReply {
        reply_receipt: None,
        owner_receipts: Vec::new(),
        outcome,
        replies: Vec::new(),
    }
}

pub(super) fn remote_resource_muc_outcome(
    reply: RelayRouteRemoteResourceStanzaReply,
    owner: NodeId,
    stop_token: CancellationToken,
) -> OrderedRelayMucProxyOutcome {
    match reply.outcome {
        RemoteResourceRouteOutcome::Delivered | RemoteResourceRouteOutcome::QueuedDetached => {
            let frames = reply.replies.into_iter().map(|reply| reply.0).collect();
            match reply.reply_receipt {
                Some(token) => OrderedRelayMucProxyOutcome::PendingFrames {
                    frames,
                    completion: RelayFrameReceiptCompletion::remote(
                        owner,
                        token,
                        reply.owner_receipts,
                        stop_token,
                    ),
                },
                None => OrderedRelayMucProxyOutcome::Delivered(frames),
            }
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
        RemoteResourceRouteTarget::ProcessedDirectMessage { .. }
        | RemoteResourceRouteTarget::FullJid { .. }
        | RemoteResourceRouteTarget::BareJid { .. } => OrderedRelayMucProxyOutcome::Dropped,
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
        frame_completion: None,
        delivery,
        client_replies: Vec::new(),
        maybe_committed,
        join_repair_allowed,
        relay_target: None,
        target_claim: None,
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

pub(super) fn synthetic_session_for_full_jid(sender_jid: &jid::FullJid) -> Session {
    let sender_bare = sender_jid.to_bare();
    let localpart = sender_bare
        .node()
        .map(|node| node.to_string())
        .unwrap_or_else(|| sender_bare.to_string());
    let sender_bare_string = sender_bare.to_string();
    Session::new(
        sender_bare_string.as_str(),
        localpart.as_str(),
        localpart.as_str(),
    )
}

#[cfg(test)]
mod ingress_handoff_tests {
    use super::*;
    use crate::server::routes::interpret::{
        OrderedRelayHandoffHandle, OrderedRelayInboundSequence,
    };

    #[tokio::test]
    async fn committed_message_never_defers_but_iq_keeps_completion_notification() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let message_handoff =
            OrderedRelayHandoffHandle::new(OrderedRelayInboundSequence(1), tx.clone());
        let message = Stanza::Message(xmpp_parsers::message::Message::new(None));
        assert!(!defer_until_relay_completion(&message_handoff, &message));
        assert!(!message_handoff.was_deferred());
        let iq_handoff = OrderedRelayHandoffHandle::new(OrderedRelayInboundSequence(2), tx);
        let iq = Stanza::Iq(Box::new(xmpp_parsers::iq::Iq::Result {
            from: None,
            to: None,
            id: "reply".to_owned(),
            payload: None,
        }));
        assert!(defer_until_relay_completion(&iq_handoff, &iq));
        assert!(iq_handoff.was_deferred());
        iq_handoff.complete(Vec::new());
        assert_eq!(
            rx.recv().await.expect("IQ completion").inbound_sequence,
            OrderedRelayInboundSequence(2)
        );
        assert!(rx.try_recv().is_err());
    }
}
