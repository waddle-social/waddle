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
pub(super) use muc::reserved::{
    deliver_reserved_muc_proxy, muc_proxy_result_to_ordered_outcome, muc_proxy_result_to_outcome,
};
pub(crate) use muc::{MucProxyRouteDecision, OrderedRelayMucProxyOutcome};
use receiver::*;
pub(super) use remote_route_helpers::*;

pub(super) fn no_client_reply_outcome(delivery: FullJidDeliveryOutcome) -> RemoteDeliveryOutcome {
    no_client_reply_outcome_with_commit_state(delivery, false)
}

pub(super) fn remote_resource_route_reply(
    outcome: RemoteResourceRouteOutcome,
) -> RelayRouteRemoteResourceStanzaReply {
    RelayRouteRemoteResourceStanzaReply {
        outcome,
        replies: Vec::new(),
    }
}

pub(super) fn remote_resource_muc_outcome(
    reply: RelayRouteRemoteResourceStanzaReply,
) -> OrderedRelayMucProxyOutcome {
    match reply.outcome {
        RemoteResourceRouteOutcome::Delivered | RemoteResourceRouteOutcome::QueuedDetached => {
            OrderedRelayMucProxyOutcome::Delivered(
                reply.replies.into_iter().map(|reply| reply.0).collect(),
            )
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
        delivery,
        client_replies: Vec::new(),
        maybe_committed,
        join_repair_allowed,
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
