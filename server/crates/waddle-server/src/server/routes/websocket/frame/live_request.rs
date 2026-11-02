//! Connection-bound provenance for a live-message handoff.
//!
//! This is not a database authorization. The existing ingress commit still
//! asserts the principal and, where configured, the SM claim in its own
//! transaction. In particular, a future dark operation must also verify the
//! existing stream and exact replay binding in that transaction.

use super::super::ConnectionPhase;
use crate::auth::Session;
use crate::clustering::NodeAdmissionPermit;
use crate::ingress::{IngressAuthority, IngressDecision, IngressPrincipal, IngressStreamIdentity};
use crate::ingress::{IngressSubmission, RoomExecutionPath};
use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
use crate::server::routes::interpret::effects::direct::{
    DurableDirectEffect, ExternalDirectEffect,
};
use crate::server::routes::interpret::effects::{DurableEffect, Effect, ExternalEffect};
use crate::server::routes::interpret::OrderedRelayInboundSequence;
use crate::server::routes::interpret::SmInboundCompletionTracker;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ingress::{
    ConnectionGeneration, IngressEffectIntent, NormalizedTarget, TransportGeneration,
};
use waddle_xmpp::stream_management::persistence::SmClaimFence;
use waddle_xmpp::stream_management::StreamManagementState;
use xmpp_parsers::message::MessageType;

/// Fields projected from one connection at the sole frame-dispatch call site.
/// Individual references are needed because the existing planning dependencies
/// already share-borrow the authenticated session through post-commit work.
pub(super) struct ConnectionParts<'connection> {
    pub(super) phase: &'connection ConnectionPhase,
    pub(super) authenticated_session: &'connection Option<Session>,
    pub(super) ingress_generation: &'connection ConnectionGeneration,
    pub(super) sm_state: &'connection StreamManagementState,
    pub(super) sm_ingress_fence: &'connection Option<SmClaimFence>,
    pub(super) sm_inbound_completion: &'connection SmInboundCompletionTracker,
}

/// A provenance scaffold, not an acceptance capability. Its private fields
/// retain the source frame's connection values through the existing commit.
pub(super) struct FrameProvenance<'connection> {
    submission: IngressSubmission,
    _connection: ConnectionParts<'connection>,
}

pub(super) enum FrameHandoff<'connection> {
    Provenance(FrameProvenance<'connection>),
    Legacy(IngressSubmission),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnsupportedFrame {
    Admission,
    Principal,
    Stream,
    Reservation,
    Origin,
    Room,
    Plan,
}

#[cfg(test)]
tokio::task_local! {
    pub(super) static CAPTURE_RESULT: std::cell::Cell<Option<Result<(), UnsupportedFrame>>>;
}

fn observe(_result: Result<(), UnsupportedFrame>) {
    #[cfg(test)]
    let _ = CAPTURE_RESULT.try_with(|result| result.set(Some(_result)));
}

/// Only the frame dispatcher calls this, immediately after it planned the
/// offered message. An unsupported handoff keeps the existing commit behavior.
pub(super) fn capture<'connection>(
    connection: ConnectionParts<'connection>,
    admission: Option<(&NodeAdmissionPermit, &CancellationToken)>,
    sequence: Option<OrderedRelayInboundSequence>,
    room_target: bool,
    submission: IngressSubmission,
) -> FrameHandoff<'connection> {
    let result = validate(&connection, admission, sequence, room_target, &submission);
    observe(result);
    if result.is_ok() {
        FrameHandoff::Provenance(FrameProvenance {
            submission,
            _connection: connection,
        })
    } else {
        FrameHandoff::Legacy(submission)
    }
}

impl FrameProvenance<'_> {
    pub(super) async fn commit(self, authority: &IngressAuthority) -> IngressDecision {
        let decision = authority.commit(&self.submission).await;
        // Keep the source projections live for the full commit, including
        // when the commit future is cancelled before it returns.
        let _connection = &self._connection;
        decision
    }
}

fn validate(
    connection: &ConnectionParts<'_>,
    admission: Option<(&NodeAdmissionPermit, &CancellationToken)>,
    sequence: Option<OrderedRelayInboundSequence>,
    room_target: bool,
    submission: &IngressSubmission,
) -> Result<(), UnsupportedFrame> {
    let (permit, shutdown) = admission.ok_or(UnsupportedFrame::Admission)?;
    if shutdown.is_cancelled() || permit.revalidate().is_err() {
        return Err(UnsupportedFrame::Admission);
    }

    let IngressPrincipal::Authenticated(principal) = &submission.principal else {
        return Err(UnsupportedFrame::Principal);
    };
    let current_principal = connection
        .authenticated_session
        .as_ref()
        .and_then(|session| session.authenticated_principal_ref().ok());
    if current_principal.as_ref() != Some(principal)
        || connection.phase.bound_jid() != Some(&submission.sender)
        || submission.sender.to_bare() != *principal.bare_jid()
        || submission.connection_generation
            != TransportGeneration::Connection(*connection.ingress_generation)
    {
        return Err(UnsupportedFrame::Principal);
    }

    let IngressStreamIdentity::Resumable {
        stream_id,
        reserved_wire_position,
        checkpoint_h,
        ..
    } = &submission.identity
    else {
        return Err(UnsupportedFrame::Stream);
    };
    if !connection.sm_state.is_resumable()
        || connection.sm_state.stream_id.as_deref() != Some(stream_id.as_str())
    {
        return Err(UnsupportedFrame::Stream);
    }
    let fence = connection
        .sm_ingress_fence
        .as_ref()
        .ok_or(UnsupportedFrame::Stream)?;
    #[cfg(feature = "clustering")]
    if let IngressStreamIdentity::Resumable {
        owner, claim_epoch, ..
    } = &submission.identity
    {
        if fence.owner() != owner || fence.epoch() != *claim_epoch {
            return Err(UnsupportedFrame::Stream);
        }
    }
    #[cfg(not(feature = "clustering"))]
    let _ = fence;

    let sequence = sequence.ok_or(UnsupportedFrame::Reservation)?;
    if !connection.sm_inbound_completion.is_pending(sequence)
        || reserved_wire_position.to_storage() != sequence.0
        || checkpoint_h.to_storage() != connection.sm_inbound_completion.checkpoint_for(sequence)
    {
        return Err(UnsupportedFrame::Reservation);
    }
    if submission.digest_input.origin().is_none() {
        return Err(UnsupportedFrame::Origin);
    }
    if room_target
        || !matches!(submission.plan.room_execution, RoomExecutionPath::None)
        || submission.plan.room_canonical_message.is_some()
    {
        return Err(UnsupportedFrame::Room);
    }

    let plan = &submission.plan;
    let message = &plan.sanitized_message;
    let target = match message.to.as_ref() {
        None => NormalizedTarget::Absent,
        Some(jid) => match jid.try_as_full() {
            Ok(full) => NormalizedTarget::Full(full.clone()),
            Err(bare) => NormalizedTarget::Bare(bare.clone()),
        },
    };
    if plan.failure.is_some()
        || plan.rejection.is_some()
        || plan.error_reply.is_some()
        || message.type_ != MessageType::Chat
        || message.from.as_ref().and_then(|jid| jid.try_as_full().ok()) != Some(&submission.sender)
        || target != submission.target
        || submission.digest_input.target() != &submission.target
        || crate::ingress::storage_hint::transient_target(message).is_some()
        || !plan.intents.iter().all(is_direct_intent)
        || !plan
            .plan
            .iter()
            .all(|planned| is_direct_effect(&planned.effect))
    {
        return Err(UnsupportedFrame::Plan);
    }
    Ok(())
}

fn is_direct_intent(intent: &IngressEffectIntent) -> bool {
    if let IngressEffectIntent::InboxProject { mutation, .. } = intent {
        use waddle_xmpp::inbox::ConversationKind;
        use waddle_xmpp::ingress::InboxProjectionMutation;
        return match mutation {
            InboxProjectionMutation::Direct { entry, .. } => entry.kind == ConversationKind::Direct,
            InboxProjectionMutation::GroupchatChannel { .. }
            | InboxProjectionMutation::GroupchatThread { .. }
            | InboxProjectionMutation::GroupchatChannelRead { .. }
            | InboxProjectionMutation::GroupchatThreadRead { .. }
            | InboxProjectionMutation::GroupchatChannelAndThread { .. }
            | InboxProjectionMutation::DirectCallThreadAnchor { .. }
            | InboxProjectionMutation::DirectCallThreadEnded { .. } => false,
        };
    }
    matches!(
        intent,
        IngressEffectIntent::ArchiveAuthoritative { .. }
            | IngressEffectIntent::RouteDirect { .. }
            | IngressEffectIntent::RelayCarbons { .. }
            | IngressEffectIntent::Carbons { .. }
            | IngressEffectIntent::NotificationActivityPreview { .. }
            | IngressEffectIntent::PendingDelivery { .. }
    )
}

fn is_direct_effect(effect: &Effect) -> bool {
    match effect {
        Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect { .. })) => true,
        Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ProjectInbox {
            entry, ..
        })) => entry.kind == waddle_xmpp::inbox::ConversationKind::Direct,
        Effect::External(ExternalEffect::Direct(
            ExternalDirectEffect::NotificationActivity { .. }
            | ExternalDirectEffect::PushInboxUpdate { .. },
        ))
        | Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer { .. }
            | ExternalDeliveryEffect::QueueDetached { .. }
            | ExternalDeliveryEffect::RelayFullJid { .. }
            | ExternalDeliveryEffect::RelayBareJid { .. }
            | ExternalDeliveryEffect::RelayCarbons { .. }
            | ExternalDeliveryEffect::Carbons { .. }
            | ExternalDeliveryEffect::QueueOfflineDelivery { .. },
        ))
        | Effect::External(ExternalEffect::Frame(_)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
