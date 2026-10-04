//! Durable exclusion immediately before a captured socket owner's synchronous enqueue.
//!
//! Unknown starts suppress retry for a bounded grace interval. The completion
//! row is delivery evidence even when the subsequent effect receipt is lost.
use std::{future::Future, time::Duration};

use jid::FullJid;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::{
    ingress::IngressEffectIntent, ownership::NodeIdentity, registry::BroadcastOutcome, Stanza,
};

use super::{identity::IngressAppendObligationRef, IngressAuthority};
use crate::{
    ingress_uow::{
        ArchiveDispatchRepository, CanonicalMessageRepository, DeliveryProgressRepository,
        EffectIntentRepository, EffectReceiptRepository, IngressFencing, IngressUnitOfWork,
        IngressUowError, IngressUowTransaction, SendAttemptRepository, SendAttemptStatus,
        SendClaim, SendObligation,
    },
    server::routes::interpret::{FullJidDeliveryOutcome, SmIngressAppendContext},
};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(1);
const LEASE_DURATION: Duration = Duration::from_secs(5);

impl IngressAuthority {
    /// Admitted execution keeps its parent admission guard and carries this
    /// token to the sink instead of reacquiring the writer-preferring lock.
    pub(crate) fn delivery_stop_token(&self) -> CancellationToken {
        self.force_stop.clone()
    }

    pub(crate) async fn live_delivery_status(
        &self,
        context: &SmIngressAppendContext,
        target: &FullJid,
    ) -> Result<Option<FullJidDeliveryOutcome>, IngressUowError> {
        bounded(Box::pin(async {
            let admission = self.admission.read().await;
            self.check_live_admission(*admission)?;
            live_delivery_status(&self.uow, Some(&self.force_stop), context, target).await
        }))
        .await
    }

    pub(crate) async fn accept_live_delivery(
        &self,
        context: &SmIngressAppendContext,
        target: &FullJid,
        stanza: &Stanza,
        enqueue: impl FnOnce() -> BroadcastOutcome,
    ) -> FullJidDeliveryOutcome {
        let result = bounded(Box::pin(async {
            let admission = self.admission.read().await;
            Box::pin(accept_live_delivery_inner(
                &self.uow,
                context,
                target,
                stanza,
                enqueue,
                || self.check_live_admission(*admission),
            ))
            .await
        }))
        .await;
        report_outcome(result, context, target)
    }

    fn check_live_admission(&self, admitted: bool) -> Result<(), IngressUowError> {
        if !admitted || self.cancellation.is_cancelled() || self.force_stop.is_cancelled() {
            Err(IngressUowError::AuthorityStopped)
        } else {
            Ok(())
        }
    }
}

/// Consult durable acceptance before checking whether the former socket still
/// exists. A lost reply must never select detached fallback.
pub(crate) async fn live_delivery_status(
    uow: &IngressUnitOfWork,
    stop: Option<&CancellationToken>,
    context: &SmIngressAppendContext,
    target: &FullJid,
) -> Result<Option<FullJidDeliveryOutcome>, IngressUowError> {
    // Keep transaction and canonical decoding state off the deeply nested
    // interpreter future: unoptimized builds must fit the default thread stack.
    bounded(Box::pin(async {
        check_stop(stop)?;
        let mut tx = transaction(uow).await?;
        authorize_resource(&mut tx, context, target).await?;
        let status = delivery_status(&mut tx, &obligation(context, target)).await?;
        tx.commit().await?;
        Ok(status)
    }))
    .await
}

/// The caller captures a socket-owner witness before entering this method.
/// `enqueue` synchronously revalidates that witness without network waits or
/// replacement socket lookup. Execution-owned UOW callers retain their parent
/// execution admission and its forced-stop token; node rotation is fenced here
/// at the actual sink. A stop check after database awaits prevents an admitted
/// but stalled execution from enqueueing beyond its shutdown deadline.
pub(crate) async fn accept_live_delivery(
    uow: &IngressUnitOfWork,
    stop: Option<&CancellationToken>,
    context: &SmIngressAppendContext,
    target: &FullJid,
    stanza: &Stanza,
    enqueue: impl FnOnce() -> BroadcastOutcome,
) -> FullJidDeliveryOutcome {
    report_outcome(
        bounded(Box::pin(accept_live_delivery_inner(
            uow,
            context,
            target,
            stanza,
            enqueue,
            || check_stop(stop),
        )))
        .await,
        context,
        target,
    )
}

fn check_stop(stop: Option<&CancellationToken>) -> Result<(), IngressUowError> {
    if stop.is_some_and(CancellationToken::is_cancelled) {
        Err(IngressUowError::AuthorityStopped)
    } else {
        Ok(())
    }
}

fn report_outcome(
    result: Result<FullJidDeliveryOutcome, IngressUowError>,
    context: &SmIngressAppendContext,
    target: &FullJid,
) -> FullJidDeliveryOutcome {
    match result {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::debug!(%error, message_key = ?context.message_key, %target,
                "live delivery authority unresolved; suppressing retry and fallback");
            FullJidDeliveryOutcome::MaybeCommitted
        }
    }
}

async fn transaction(
    uow: &IngressUnitOfWork,
) -> Result<IngressUowTransaction<'_>, IngressUowError> {
    uow.begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
        .await
}

async fn accept_live_delivery_inner(
    uow: &IngressUnitOfWork,
    context: &SmIngressAppendContext,
    target: &FullJid,
    stanza: &Stanza,
    enqueue: impl FnOnce() -> BroadcastOutcome,
    check_admission: impl Fn() -> Result<(), IngressUowError>,
) -> Result<FullJidDeliveryOutcome, IngressUowError> {
    check_admission()?;
    let owner = match uow.fencing() {
        #[cfg(feature = "clustering")]
        IngressFencing::Clustered(identity) => identity.current(),
        IngressFencing::SingleNode => NodeIdentity::local(),
    };
    // Carbon and room envelopes have dedicated full-payload validators.
    // Run their pooled reads before taking the canonical transaction lock.
    let reference = IngressAppendObligationRef::for_message(Some(context), stanza)
        .ok_or(IngressUowError::EffectIntentConflict)?;
    super::append_authority::check_resource_binding(
        stanza,
        context.receipt.kind.to_storage(),
        target,
    )
    .map_err(|_| IngressUowError::EffectIntentConflict)?;
    if context.receipt.kind.to_storage()
        != waddle_xmpp::ingress::IngressEffectKind::RouteDirect.storage_tag()
        || matches!(stanza, Stanza::Message(message) if message.type_ == xmpp_parsers::message::MessageType::Groupchat)
    {
        super::append_authority::check_canonical_obligation(uow.database(), stanza, &reference)
            .await
            .map_err(|_| IngressUowError::EffectIntentConflict)?;
    }
    let obligation = obligation(context, target);
    let mut tx = transaction(uow).await?;
    let intents = authorize_resource(&mut tx, context, target).await?;
    authorize_direct_stanza(&mut tx, context, target, stanza, &intents).await?;
    if let Some(status) = delivery_status(&mut tx, &obligation).await? {
        tx.commit().await?;
        return Ok(status);
    }
    check_admission()?;
    let claim = SendAttemptRepository::claim(&mut tx, &obligation, &owner, LEASE_DURATION).await?;
    tx.commit().await?;
    let lease = match claim {
        SendClaim::Acquired(lease) => lease,
        SendClaim::Completed => return Ok(FullJidDeliveryOutcome::Delivered),
        SendClaim::Busy | SendClaim::Ambiguous => {
            return Ok(FullJidDeliveryOutcome::MaybeCommitted)
        }
    };
    #[cfg(test)]
    test_hooks::after_claim(context.message_key, target).await;
    // Hold the real rotation gate across the start commit and synchronous
    // sink call. A previously captured identity is not itself authority.
    #[cfg(feature = "clustering")]
    let _node_guard = match uow.fencing() {
        IngressFencing::Clustered(identity) => Some(
            identity
                .guard_if_current(&owner)
                .await
                .ok_or(IngressUowError::AuthorityStopped)?,
        ),
        IngressFencing::SingleNode => None,
    };
    check_admission()?;
    let mut tx = transaction(uow).await?;
    authorize_resource(&mut tx, context, target).await?;
    if let Some(status) = accepted_status(&mut tx, &obligation).await? {
        tx.commit().await?;
        return Ok(status);
    }
    check_admission()?;
    if !SendAttemptRepository::start(&mut tx, &lease).await? {
        return Ok(FullJidDeliveryOutcome::MaybeCommitted);
    }
    tx.commit().await?;
    check_admission()?;
    #[cfg(test)]
    test_hooks::after_start(context.message_key, target).await;
    check_admission()?;
    let outcome = enqueue();
    // No transaction or canonical lock is held while touching the sink.
    // Failed completion leaves Started, excluding retry until its recovery deadline.
    let mut tx = transaction(uow).await?;
    let changed = if outcome == BroadcastOutcome::Delivered {
        SendAttemptRepository::complete(&mut tx, &lease).await?
    } else {
        SendAttemptRepository::release_proven_not_enqueued(&mut tx, &lease).await?
    };
    if !changed {
        return Err(IngressUowError::InvalidStoredSendAttempt);
    }
    tx.commit().await?;
    Ok(match outcome {
        BroadcastOutcome::Delivered => FullJidDeliveryOutcome::Delivered,
        BroadcastOutcome::DroppedFull => FullJidDeliveryOutcome::Dropped,
        BroadcastOutcome::NotConnected | BroadcastOutcome::DroppedClosed => {
            FullJidDeliveryOutcome::Unavailable
        }
    })
}

async fn bounded<T>(
    operation: impl Future<Output = Result<T, IngressUowError>>,
) -> Result<T, IngressUowError> {
    tokio::time::timeout(OPERATION_TIMEOUT, operation)
        .await
        .map_err(|_| IngressUowError::Timeout)?
}

fn obligation(context: &SmIngressAppendContext, target: &FullJid) -> SendObligation {
    SendObligation {
        message: context.message_key,
        receipt: context.receipt.clone(),
        recipient: target.clone(),
    }
}

async fn authorize_resource(
    tx: &mut IngressUowTransaction<'_>,
    context: &SmIngressAppendContext,
    target: &FullJid,
) -> Result<Vec<IngressEffectIntent>, IngressUowError> {
    // Loading intents takes the canonical lock, protecting subsequent receipt,
    // custody, claim and progress reads against competing writers.
    let intents = EffectIntentRepository::load(tx, context.message_key).await?;
    let authorized = intents.iter().any(|intent| {
        if super::receipt_key(intent).ok().as_ref() != Some(&context.receipt) {
            return false;
        }
        match intent {
            IngressEffectIntent::RouteDirect {
                recipient, fanout, ..
            } => *recipient == target.to_bare() && fanout.contains(target),
            IngressEffectIntent::RouteMucGroupchat { occupants, .. }
            | IngressEffectIntent::RouteMucSystemBroadcast { occupants, .. } => {
                occupants.contains(target)
            }
            IngressEffectIntent::Carbons {
                excluded_source,
                carbon_recipients,
                ..
            } => excluded_source != target && carbon_recipients.contains(target),
            IngressEffectIntent::RelayCarbons { owner, exclude, .. } => {
                *owner == target.to_bare() && !exclude.contains(target)
            }
            _ => false,
        }
    });
    if !authorized
        || ArchiveDispatchRepository::positions(tx, context.message_key, &context.receipt).await?
            != context.archive_positions
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    Ok(intents)
}

async fn authorize_direct_stanza(
    tx: &mut IngressUowTransaction<'_>,
    context: &SmIngressAppendContext,
    target: &FullJid,
    stanza: &Stanza,
    intents: &[IngressEffectIntent],
) -> Result<(), IngressUowError> {
    let Some(intent @ IngressEffectIntent::RouteDirect { route_identity, .. }) = intents
        .iter()
        .find(|intent| super::receipt_key(intent).ok().as_ref() == Some(&context.receipt))
    else {
        return Ok(());
    };
    let envelope = CanonicalMessageRepository::load_envelope(tx, context.message_key)
        .await?
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    let Stanza::Message(message) = stanza else {
        return Err(IngressUowError::EffectIntentConflict);
    };
    if let Some(expected) =
        super::invitation_authority::recorded_message(&envelope, intents, &context.receipt)?
    {
        return if super::append_authority::same_message_content(&expected, message) {
            Ok(())
        } else {
            Err(IngressUowError::EffectIntentConflict)
        };
    }
    if let waddle_xmpp::ingress::EffectMessageIdentity::StanzaId(stanza_id) = route_identity {
        let mut pin_owned = false;
        for intent in intents {
            let IngressEffectIntent::DmPinMutation {
                pair,
                target_stanza_id,
                action,
            } = intent
            else {
                continue;
            };
            pin_owned = true;
            let mutation =
                crate::server::routes::websocket::handlers::message::dm_pin::DmPinMutation {
                    pair: crate::server::routes::websocket::DmPairKey::new(
                        pair.0.clone(),
                        pair.1.clone(),
                    ),
                    target_stanza_id: target_stanza_id.clone(),
                    action: action.clone(),
                };
            let expected =
                crate::server::routes::websocket::handlers::message::dm_pin::recorded_pin_message(
                    &envelope, &mutation, stanza_id,
                )?;
            if expected == *message {
                return Ok(());
            }
        }
        if pin_owned {
            return Err(IngressUowError::EffectIntentConflict);
        }
    }
    if !message.to.as_ref().is_some_and(|to| {
        to.try_as_full()
            .map_or_else(|_| to.to_bare() == target.to_bare(), |full| full == target)
    }) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    if super::recovery_rebuild::rebuildable_direct_route(&envelope, intents, intent) {
        let mut expected = envelope.message().clone();
        let recipient = target.to_bare();
        let mut archived_recipient = false;
        for intent in intents {
            if let IngressEffectIntent::ArchiveAuthoritative {
                archive, stanza_id, ..
            } = intent
            {
                if *archive == recipient {
                    archived_recipient = true;
                    waddle_xmpp_core::xep0359::add_stanza_id(&mut expected, stanza_id);
                }
            }
        }
        if !super::append_authority::recipient_copy_matches(
            &expected,
            message,
            &recipient,
            archived_recipient,
        ) {
            return Err(IngressUowError::EffectIntentConflict);
        }
    } else if matches!(
        route_identity,
        waddle_xmpp::ingress::EffectMessageIdentity::StanzaId(_)
            | waddle_xmpp::ingress::EffectMessageIdentity::OriginId(_)
    ) && !super::receipts::routing::message_identity(message, route_identity)
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    // Generated CaptureOrdinal/InboxPush effects have no recorded payload or
    // wire identity. Their typed planner association supplies stanza authority;
    // the exact immutable receipt and resource still authorize the lease.
    Ok(())
}

async fn accepted_status(
    tx: &mut IngressUowTransaction<'_>,
    obligation: &SendObligation,
) -> Result<Option<FullJidDeliveryOutcome>, IngressUowError> {
    if SendAttemptRepository::has_custody(tx, obligation).await? {
        return Ok(Some(FullJidDeliveryOutcome::QueuedDetached));
    }
    if EffectReceiptRepository::contains(
        tx,
        obligation.message,
        obligation.receipt.kind,
        &obligation.receipt.semantic_identity_hash,
    )
    .await?
        || DeliveryProgressRepository::load(tx, obligation.message, &obligation.receipt)
            .await?
            .contains(&obligation.recipient)
        || SendAttemptRepository::has_resource_receipt(tx, obligation).await?
    {
        return Ok(Some(FullJidDeliveryOutcome::Delivered));
    }
    Ok(None)
}

pub(super) async fn delivery_status(
    tx: &mut IngressUowTransaction<'_>,
    obligation: &SendObligation,
) -> Result<Option<FullJidDeliveryOutcome>, IngressUowError> {
    if let Some(status) = accepted_status(tx, obligation).await? {
        return Ok(Some(status));
    }
    Ok(match SendAttemptRepository::status(tx, obligation).await? {
        Some(SendAttemptStatus::Completed) => Some(FullJidDeliveryOutcome::Delivered),
        Some(SendAttemptStatus::Leased | SendAttemptStatus::Started) => {
            Some(FullJidDeliveryOutcome::MaybeCommitted)
        }
        None | Some(SendAttemptStatus::ExpiredStarted) => None,
    })
}

#[cfg(test)]
#[path = "live_delivery_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "live_delivery_test_hooks.rs"]
pub(crate) mod test_hooks;
