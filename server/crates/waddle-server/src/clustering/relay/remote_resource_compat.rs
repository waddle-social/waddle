//! Rolling-upgrade contracts for remote-owned live resources (#1804).
//!
//! `v8`/`v3` are frozen receive contracts, not aliases for evolving domain types.
//! Live operations have their own stable endpoints and cannot carry append
//! obligations. An unknown live endpoint permits exactly one baseline ask;
//! ambiguous failures never permit a second effect. Shared typed leaves are
//! pinned by the codec fixtures in `tests`; changing them requires a new DTO
//! and endpoint while retaining these receivers through the upgrade window.

use super::*;
use crate::clustering::ordered_relay::MucProxyOrigin;
use crate::ingress::identity::IngressAppendObligationRef;
use std::future::Future;
use waddle_xmpp::registry::DeliveryKind;

mod wire;
use wire::*;

#[cfg(test)]
mod tests;

/// A second attempt is safe only when dispatch rejected the message id before
/// decoding or running the handler. In particular, DeserializeMessage can mean
/// the reply failed to decode after the receiver committed the operation.
async fn live_or_baseline<T, E>(
    live: impl Future<Output = Result<T, RemoteSendError<E>>>,
    baseline: impl Future<Output = Result<T, RemoteSendError<E>>>,
) -> Result<T, RemoteSendError<E>> {
    match live.await {
        Err(RemoteSendError::UnknownMessage { .. }) => baseline.await,
        result => result,
    }
}

pub(super) async fn ask_route(
    remote: &RemoteActorRef<RelayActor>,
    message: &RelayRouteRemoteResourceStanza,
    mailbox_timeout: Duration,
    reply_timeout: Duration,
) -> Result<RelayRouteRemoteResourceStanzaReply, RemoteSendError<kameo::error::Infallible>> {
    let baseline = RouteV8::from(message.clone());
    let baseline_ask = async {
        remote
            .ask(&baseline)
            .mailbox_timeout(mailbox_timeout)
            .reply_timeout(reply_timeout)
            .await
            .map(Into::into)
    };
    match LiveRoute::from_current(message) {
        Some(live) => {
            live_or_baseline(
                async {
                    remote
                        .ask(&live)
                        .mailbox_timeout(mailbox_timeout)
                        .reply_timeout(reply_timeout)
                        .await
                        .map(Into::into)
                },
                baseline_ask,
            )
            .await
        }
        None => baseline_ask.await,
    }
}

pub(super) async fn ask_frame(
    remote: &RemoteActorRef<RelayActor>,
    message: &RelayDeliverRemoteResourceFrame,
    mailbox_timeout: Duration,
    reply_timeout: Duration,
) -> Result<RelayRemoteResourceFrameReply, RemoteSendError<kameo::error::Infallible>> {
    let baseline = FrameV3::from(message.clone());
    let baseline_ask = async {
        remote
            .ask(&baseline)
            .mailbox_timeout(mailbox_timeout)
            .reply_timeout(reply_timeout)
            .await
            .map(Into::into)
    };
    match LiveFrame::from_current(message) {
        Some(live) => {
            live_or_baseline(
                async {
                    remote
                        .ask(&live)
                        .mailbox_timeout(mailbox_timeout)
                        .reply_timeout(reply_timeout)
                        .await
                        .map(Into::into)
                },
                baseline_ask,
            )
            .await
        }
        None => baseline_ask.await,
    }
}

async fn route_on_owner(
    bridge: Arc<OrderedRelayDeliveryBridge>,
    receipts: Arc<Mutex<PendingReplyReceipts>>,
    message: RelayRouteRemoteResourceStanza,
) -> RouteReply {
    let Some(permit) = receipts.lock().await.reserve() else {
        return RelayRouteRemoteResourceStanzaReply {
            reply_receipt: None,
            owner_receipts: Vec::new(),
            outcome: RemoteResourceRouteOutcome::Unavailable,
            replies: Vec::new(),
        }
        .into();
    };
    let mut completion = None;
    let mut reply = bridge
        .route_remote_resource_stanza_on_owner(message, &mut completion)
        .await;
    if reply.outcome == RemoteResourceRouteOutcome::Delivered {
        if let Some(completion) = completion {
            reply.owner_receipts = completion.frame_receipts();
            reply.reply_receipt = Some(receipts.lock().await.register_reserved(permit, completion));
        }
    }
    reply.into()
}

#[kameo::remote_message("waddle.clustering.relay.remote_resource_route.v8")]
impl Message<RouteV8> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<RouteReply>;

    async fn handle(
        &mut self,
        message: RouteV8,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span(RelayDispatchKind::RemoteResourceRoute, &message.trace);
        span.record("jid", tracing::field::display(&message.source_jid));
        spawn_in_dispatch_span(
            ctx,
            span,
            route_on_owner(
                Arc::clone(&self.ordered_delivery_bridge),
                Arc::clone(&self.pending_reply_receipts),
                message.into(),
            ),
        )
    }
}

#[kameo::remote_message("waddle.clustering.relay.live_resource_route.v1")]
impl Message<LiveRoute> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<RouteReply>;

    async fn handle(
        &mut self,
        message: LiveRoute,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span(RelayDispatchKind::RemoteResourceRoute, &message.trace);
        span.record("jid", tracing::field::display(&message.source_jid));
        spawn_in_dispatch_span(
            ctx,
            span,
            route_on_owner(
                Arc::clone(&self.ordered_delivery_bridge),
                Arc::clone(&self.pending_reply_receipts),
                message.into(),
            ),
        )
    }
}

#[kameo::remote_message("waddle.clustering.relay.remote_resource_frame.v3")]
impl Message<FrameV3> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<FrameReply>;

    async fn handle(
        &mut self,
        message: FrameV3,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span(RelayDispatchKind::RemoteResourceFrame, &message.trace);
        span.record("jid", tracing::field::display(&message.frame.jid));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            bridge
                .deliver_remote_resource_frame_on_socket(message.into())
                .await
                .into()
        })
    }
}

#[kameo::remote_message("waddle.clustering.relay.live_resource_frame.v1")]
impl Message<LiveFrame> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<FrameReply>;

    async fn handle(
        &mut self,
        message: LiveFrame,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span(RelayDispatchKind::RemoteResourceFrame, &message.trace);
        span.record("jid", tracing::field::display(&message.jid));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            bridge
                .deliver_remote_resource_frame_on_socket(message.into())
                .await
                .into()
        })
    }
}

#[cfg(test)]
pub(super) use wire::{FrameV3 as BaselineFrame, RouteV8 as BaselineRoute};
