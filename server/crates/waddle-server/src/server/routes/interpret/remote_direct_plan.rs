//! Planning a direct message that an account at another server sent to an
//! account of this deployment (ADR-0020 D7; gate G18).
//!
//! The message takes the recipient's half of a local direct message and
//! nothing else: the recipient pass of the selected resources (XEP-0191
//! incoming block, the XEP-0359 id in the recipient's archive, the XEP-0313
//! recipient archive, the inbox projection, XEP-0280 received carbons), the
//! delivery to those resources, a detached session's queue, or, with no
//! resource, the offline store and its push notification. The sender's half
//! (its archive, its inbox, sent carbons, its blocking list and the early
//! handlers of a local session) is its own server's, and no connection
//! state of this deployment belongs to the sender.

use jid::{BareJid, Jid};
use waddle_xmpp::ingress::IngressEffectIntent;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;

use super::effects::delivery::ExternalDeliveryEffect;
use super::effects::direct::{DurableDirectEffect, ExternalDirectEffect};
use super::effects::{
    DurableEffect, Effect, EffectSink, ExternalEffect, IngressPlan, PlanSink, RoomExecutionPath,
};
use super::Deps;
use crate::ingress::IngressEffectCapture;

/// Plan `message`, a `chat` or `normal` message from the remote sender its
/// `from` names to `recipient`'s account, routed to `route`: the full JID
/// of the resource selected for it, or `recipient`'s bare JID when it has
/// none. The message keeps its own `to`.
pub(crate) async fn plan_remote_direct_message(
    deps: &Deps<'_>,
    route: Jid,
    message: Message,
) -> IngressPlan {
    let sink = PlanSink::new();
    let sender = message
        .from
        .as_ref()
        .and_then(|jid| jid.try_as_full().ok())
        .cloned();
    if let Some(sender) = &sender {
        sink.observe_sender(sender);
    }
    sink.observe_message(&message);
    let capture = IngressEffectCapture::new();
    let planned =
        super::build_plan_deps(deps, &sink).with_ingress_effect_capture(Some(capture.clone()));
    let _ = super::route_to_connection(
        &planned,
        route,
        Box::new(Stanza::Message(message.clone())),
        0,
        None,
    )
    .await;
    super::message_plan::finish_plan(&sink, &capture, message, sender)
}

/// What a remote sender's planned direct message does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteDirectPlan {
    /// Every effect is the recipient account's own, and something is to be
    /// done: the plan may be committed.
    Recipient,
    /// Nothing is to be done (a chat state alone to an account with no
    /// resource): nothing is committed.
    Empty,
    /// The plan fails, refuses the message, answers its sender, or holds an
    /// effect outside the recipient's account: nothing is committed, and
    /// the sender is answered nothing (gate G18).
    Refused,
}

/// Classify a plan for a remote sender's direct message to `recipient`.
///
/// Only effects on the recipient's own account are accepted: its archive
/// and inbox, deliveries to its resources, its detached sessions and its
/// offline store, received carbons to its resources and its notification
/// and link-preview state. Anything addressed to the sender (an error
/// reply, a bounce, a frame) or to anybody else refuses the whole plan, so
/// that no path answers a remote sender and no state of the sender's
/// account is kept here.
pub(crate) fn classify_remote_direct_plan(
    plan: &IngressPlan,
    recipient: &BareJid,
) -> RemoteDirectPlan {
    if plan.failure.is_some()
        || plan.rejection.is_some()
        || plan.error_reply.is_some()
        || !matches!(plan.room_execution, RoomExecutionPath::None)
        || plan
            .intents
            .iter()
            .any(|intent| matches!(intent, IngressEffectIntent::ErrorReply { .. }))
    {
        return RemoteDirectPlan::Refused;
    }
    if !plan
        .plan
        .iter()
        .all(|planned| recipient_effect(&planned.effect, recipient))
    {
        return RemoteDirectPlan::Refused;
    }
    if plan.plan.is_empty() && plan.intents.is_empty() {
        return RemoteDirectPlan::Empty;
    }
    RemoteDirectPlan::Recipient
}

/// Whether `effect` acts on `recipient`'s account only.
fn recipient_effect(effect: &Effect, recipient: &BareJid) -> bool {
    match effect {
        Effect::Durable(DurableEffect::Direct(direct)) => match direct {
            DurableDirectEffect::ArchiveDirect { archive, .. }
            | DurableDirectEffect::RetractionTombstone { archive, .. } => archive == recipient,
            DurableDirectEffect::ProjectInbox { owner, .. }
            | DurableDirectEffect::MarkInboxRead { owner, .. }
            | DurableDirectEffect::DmCallThreadProjection { owner, .. } => owner == recipient,
        },
        Effect::External(ExternalEffect::Delivery(delivery)) => match delivery {
            ExternalDeliveryEffect::RouteToPeer { jid, .. } => jid.to_bare() == *recipient,
            ExternalDeliveryEffect::QueueDetached {
                bare, resources, ..
            } => {
                bare == recipient
                    && resources
                        .iter()
                        .all(|resource| resource.to_bare() == *recipient)
            }
            ExternalDeliveryEffect::RelayFullJid { target, .. } => target.to_bare() == *recipient,
            ExternalDeliveryEffect::RelayBareJid { target, .. } => target == recipient,
            ExternalDeliveryEffect::RelayCarbons { owner, .. } => owner == recipient,
            ExternalDeliveryEffect::Carbons {
                owner,
                recipient: resource,
                ..
            } => owner == recipient && resource.to_bare() == *recipient,
            ExternalDeliveryEffect::QueueOfflineDelivery { row, .. } => row.recipient == *recipient,
            ExternalDeliveryEffect::HostOwnedCopy { .. }
            | ExternalDeliveryEffect::UndeliverableBounce { .. }
            | ExternalDeliveryEffect::SfuRevokeToken { .. } => false,
        },
        Effect::External(ExternalEffect::Direct(direct)) => match direct {
            ExternalDirectEffect::NotificationActivity { owner, .. }
            | ExternalDirectEffect::PushInboxUpdate { owner, .. } => owner == recipient,
            ExternalDirectEffect::LinkPreviewRefs { .. }
            | ExternalDirectEffect::ClearLinkPreviewRefs { .. }
            | ExternalDirectEffect::DmCallThreadState { .. }
            | ExternalDirectEffect::ScrubReplayForTombstone { .. } => true,
        },
        Effect::Durable(DurableEffect::Room(_))
        | Effect::External(
            ExternalEffect::RouteToPeer(_)
            | ExternalEffect::QueueOfflineDelivery(_)
            | ExternalEffect::RoomMembershipMutation(_)
            | ExternalEffect::InviteLedger(_)
            | ExternalEffect::DmPinMutation(_)
            | ExternalEffect::Frame(_)
            | ExternalEffect::Room(_),
        )
        | Effect::Immediate(_) => false,
    }
}
