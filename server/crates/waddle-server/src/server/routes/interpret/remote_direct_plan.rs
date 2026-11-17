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
use waddle_xmpp::ingress::{
    InboxProjectionMutation, IngressEffectIntent, PendingDeliveryMutation, PlannedDmCallState,
    TombstoneReplayTarget,
};
use waddle_xmpp::tombstone::TombstoneTarget;
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
    /// done: the plan may be committed. `held` when the plan hands the
    /// message to a detached session or to the offline store, which keep
    /// it for a later session.
    Recipient { held: bool },
    /// Nothing is to be done (a chat state alone to an account with no
    /// resource): nothing is committed.
    Empty,
    /// The plan fails, refuses the message, answers its sender, or holds an
    /// effect outside the recipient's account: nothing is committed, and
    /// the sender is answered nothing (gate G18).
    Refused,
}

/// Classify a plan for a remote `sender`'s direct message to `recipient`.
///
/// Only work on the recipient's own account is accepted: its archive and
/// inbox, deliveries to its resources, its detached sessions and its
/// offline store, received carbons to its resources, its notification and
/// link-preview state, and the state of its own conversation with the
/// sender (a call thread between the two, the removal of the sender's
/// retracted message from what the recipient's sessions would replay).
/// Every intent and every effect is classified, and each match is
/// exhaustive, so a new variant does not compile until it is classified.
/// Anything addressed to the sender (an error reply, a bounce, a frame), to
/// anybody else, or that this function does not classify refuses the whole
/// plan, so that no path answers a remote sender and no state of the
/// sender's account, or of a third account, is kept or changed here.
pub(crate) fn classify_remote_direct_plan(
    plan: &IngressPlan,
    recipient: &BareJid,
    sender: &BareJid,
) -> RemoteDirectPlan {
    if plan.failure.is_some()
        || plan.rejection.is_some()
        || plan.error_reply.is_some()
        || !matches!(plan.room_execution, RoomExecutionPath::None)
    {
        return RemoteDirectPlan::Refused;
    }
    if !plan
        .intents
        .iter()
        .all(|intent| recipient_intent(intent, recipient, sender))
    {
        return RemoteDirectPlan::Refused;
    }
    if !plan
        .plan
        .iter()
        .all(|planned| recipient_effect(&planned.effect, recipient, sender))
    {
        return RemoteDirectPlan::Refused;
    }
    if plan.plan.is_empty() && plan.intents.is_empty() {
        return RemoteDirectPlan::Empty;
    }
    let held = plan.plan.iter().any(|planned| {
        matches!(
            planned.effect,
            Effect::External(ExternalEffect::Delivery(
                ExternalDeliveryEffect::QueueDetached { .. }
                    | ExternalDeliveryEffect::QueueOfflineDelivery { .. }
            ))
        )
    });
    RemoteDirectPlan::Recipient { held }
}

/// Whether `intent`, as the acceptance would record it, names `recipient`'s
/// account only, or the conversation between `recipient` and `sender`.
fn recipient_intent(intent: &IngressEffectIntent, recipient: &BareJid, sender: &BareJid) -> bool {
    match intent {
        IngressEffectIntent::ArchiveAuthoritative { archive, by, .. } => {
            archive == recipient && by == recipient
        }
        IngressEffectIntent::RouteDirect {
            recipient: routed,
            fanout,
            ..
        } => {
            routed == recipient
                && fanout
                    .iter()
                    .all(|resource| resource.to_bare() == *recipient)
        }
        IngressEffectIntent::Carbons {
            carbon_recipients,
            excluded_source,
            ..
        } => {
            excluded_source.to_bare() == *recipient
                && carbon_recipients
                    .iter()
                    .all(|resource| resource.to_bare() == *recipient)
        }
        IngressEffectIntent::RelayCarbons { owner, exclude, .. } => {
            owner == recipient
                && exclude
                    .iter()
                    .all(|resource| resource.to_bare() == *recipient)
        }
        IngressEffectIntent::InboxProject { owner, mutation } => {
            owner == recipient
                && match mutation {
                    InboxProjectionMutation::Direct { .. }
                    | InboxProjectionMutation::DirectCallThreadAnchor { .. }
                    | InboxProjectionMutation::DirectCallThreadEnded { .. } => true,
                    InboxProjectionMutation::GroupchatChannel { .. }
                    | InboxProjectionMutation::GroupchatThread { .. }
                    | InboxProjectionMutation::GroupchatChannelRead { .. }
                    | InboxProjectionMutation::GroupchatThreadRead { .. }
                    | InboxProjectionMutation::GroupchatChannelAndThread { .. } => false,
                }
        }
        IngressEffectIntent::NotificationActivityPreview { owner, .. } => owner == recipient,
        IngressEffectIntent::PendingDelivery { mutation } => match mutation {
            PendingDeliveryMutation::Archived {
                recipient: stored, ..
            }
            | PendingDeliveryMutation::Transient {
                recipient: stored, ..
            } => stored == recipient,
        },
        IngressEffectIntent::LinkPreviewMediaRef { mutation } => mutation.archive == *recipient,
        IngressEffectIntent::RetractionTombstone { mutation } => mutation.archive == *recipient,
        IngressEffectIntent::TombstoneReplayDeletion { target, .. } => match target {
            TombstoneReplayTarget::Direct {
                author, archive, ..
            } => archive == recipient && author == sender,
            TombstoneReplayTarget::Groupchat { .. } => false,
        },
        IngressEffectIntent::DmCallThreadState { state, .. } => {
            conversation_call_state(state, recipient, sender)
        }
        // A remote sender's direct message does nothing else here: no room,
        // no other account's state, no pin or invitation, and no answer.
        IngressEffectIntent::SystemMessageArchive { .. }
        | IngressEffectIntent::RouteMucGroupchat { .. }
        | IngressEffectIntent::RouteMucSystemBroadcast { .. }
        | IngressEffectIntent::RouteOccupantPm { .. }
        | IngressEffectIntent::DispatchToRoomRemote { .. }
        | IngressEffectIntent::GroupchatNotificationRecovery { .. }
        | IngressEffectIntent::DmPinMutation { .. }
        | IngressEffectIntent::MucInviteMembershipGrant { .. }
        | IngressEffectIntent::MucInviteLedger { .. }
        | IngressEffectIntent::GroupDmMembershipGrant { .. }
        | IngressEffectIntent::GroupDmInviteLedger { .. }
        | IngressEffectIntent::RoomSubjectMutation { .. }
        | IngressEffectIntent::CallSignal { .. }
        | IngressEffectIntent::Pin { .. }
        | IngressEffectIntent::RoomObserver { .. }
        | IngressEffectIntent::Extension { .. }
        | IngressEffectIntent::ErrorReply { .. } => false,
    }
}

/// Whether `state` is the call state of the conversation between
/// `recipient` and `sender` that only the recipient's side has projected:
/// the sender's call-thread projection is its own server's.
fn conversation_call_state(
    state: &PlannedDmCallState,
    recipient: &BareJid,
    sender: &BareJid,
) -> bool {
    let key = &state.key;
    let pair = (&key.low_peer == recipient && &key.high_peer == sender)
        || (&key.low_peer == sender && &key.high_peer == recipient);
    pair && state.projected.iter().all(|owner| owner == recipient)
}

/// Whether `target` names a message `sender` wrote in `recipient`'s
/// archive.
fn sender_tombstone(target: &TombstoneTarget, recipient: &BareJid, sender: &BareJid) -> bool {
    match target {
        TombstoneTarget::Direct {
            author, archive, ..
        } => archive == recipient && author == sender,
        TombstoneTarget::Groupchat { .. } => false,
    }
}

/// Whether `effect` acts on `recipient`'s account only, or on its
/// conversation with `sender`.
fn recipient_effect(effect: &Effect, recipient: &BareJid, sender: &BareJid) -> bool {
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
            ExternalDirectEffect::LinkPreviewRefs { mutations }
            | ExternalDirectEffect::ClearLinkPreviewRefs { mutations } => mutations
                .iter()
                .all(|mutation| mutation.archive == *recipient),
            ExternalDirectEffect::DmCallThreadState { state, .. } => {
                conversation_call_state(state, recipient, sender)
            }
            ExternalDirectEffect::ScrubReplayForTombstone { target } => {
                sender_tombstone(target, recipient, sender)
            }
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
