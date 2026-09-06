use super::*;

pub(super) mod channels;
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
use receiver::*;
pub(super) use remote_route_helpers::*;
pub(crate) use remote_socket::RegisteredRemoteWriteAcceptedDelivery;

/// Message responsibility is already committed before relay execution. IQ and
/// presence retain socket-acceptance completion as their handled boundary.
fn defer_until_relay_completion(
    handoff: &crate::server::routes::interpret::OrderedRelayHandoffHandle,
    stanza: &Stanza,
) -> bool {
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
        outcome,
        replies: Vec::new(),
        recipient_sm_append_streams: Vec::new(),
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
                    completion: crate::ingress::execute::RelayFrameReceiptCompletion::remote(
                        owner, token, stop_token,
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
        RemoteResourceRouteTarget::FullJid { .. } | RemoteResourceRouteTarget::BareJid { .. } => {
            OrderedRelayMucProxyOutcome::Dropped
        }
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

pub(super) fn synthetic_session_for_full_jid(sender_jid: &jid::FullJid) -> crate::auth::Session {
    let sender_bare = sender_jid.to_bare();
    let localpart = sender_bare
        .node()
        .map(|node| node.to_string())
        .unwrap_or_else(|| sender_bare.to_string());
    let sender_bare_string = sender_bare.to_string();
    crate::auth::Session::new(
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
