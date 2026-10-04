//! Keep a frozen invitation's live fanout mutually exclusive with its fallback.
use std::time::Duration;

use waddle_xmpp::{
    ingress::{IngressEffectIntent, MessageKey, PendingDeliveryMutation},
    pending_delivery::{InsertOutcome, PendingPayload, QuotaPolicy},
    Stanza,
};

use crate::{
    ingress::{decision::IngressDecision, live_delivery},
    ingress_uow::{
        settle_recorded, ArchiveDispatchRepository, CanonicalMessageRepository,
        EffectIntentRepository, EffectReceiptRepository, IngressUnitOfWork, IngressUowError,
        IngressUowTransaction, PendingReceiptRepository, SendAttemptRepository, SendObligation,
    },
    server::routes::interpret::{
        deliver_direct_to_full_locally, deliver_registered_remote_resource,
        effects::{invite::MucUserRoute, EffectOutcome, SettledCompletion, SettledOutcome},
        Deps, FullJidDeliveryOutcome, SmIngressAppendContext,
    },
};

#[derive(Debug, thiserror::Error)]
enum StoreError {
    #[error(transparent)]
    Ingress(#[from] IngressUowError),
    #[error(transparent)]
    Pending(#[from] waddle_xmpp::pending_delivery::storage::PendingStorageError),
}

struct Invitation {
    key: MessageKey,
    evidence: [IngressEffectIntent; 2],
    context: SmIngressAppendContext,
}

enum Prepared {
    Pending(Box<Invitation>),
    Settled(Vec<IngressEffectIntent>),
}

enum Finished {
    Settled(Vec<IngressEffectIntent>),
    Uncertain,
}

#[cfg(test)]
tokio::task_local! {
    pub(crate) static PAUSE_BEFORE_INVITATION_SETTLEMENT: (
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    );
}

pub(super) async fn execute(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    route: &MucUserRoute,
    deps: &Deps<'_>,
) -> EffectOutcome {
    let Some(storage) = deps.pending_delivery_storage.or_else(|| {
        deps.web_socket_state
            .map(|state| &state.deps.protocol.pending_delivery_storage)
    }) else {
        return EffectOutcome::Unavailable;
    };
    let result = execute_inner(uow, decision, index, route, deps, storage.quota_policy()).await;
    match result {
        Ok(Finished::Settled(persisted)) => settled(persisted, SettledCompletion::Complete),
        Ok(Finished::Uncertain) => settled(Vec::new(), SettledCompletion::Uncertain),
        Err(error) => {
            // A failed/unknown database outcome cannot prove delivery failed,
            // so it must never revoke membership or remove the invitation.
            tracing::warn!(%error, "invitation delivery remains unresolved");
            settled(Vec::new(), SettledCompletion::Uncertain)
        }
    }
}

async fn execute_inner(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    route: &MucUserRoute,
    deps: &Deps<'_>,
    quota: QuotaPolicy,
) -> Result<Finished, StoreError> {
    check_stop(deps)?;
    let invitation = match prepare(uow, decision, index, route).await? {
        Prepared::Settled(persisted) => return Ok(Finished::Settled(persisted)),
        Prepared::Pending(invitation) => invitation,
    };
    let stanza = Stanza::Message(*route.message.clone());
    let mut uncertain = false;
    for resource in &route.resources {
        check_stop(deps)?;
        let mut immediate = deps.clone();
        immediate.ingress_delivery_uow = Some(uow.clone());
        immediate.ingress_append_context = Some(invitation.context.clone());
        let mut outcome = deliver_direct_to_full_locally(&immediate, resource, &stanza).await;
        if outcome == FullJidDeliveryOutcome::Unavailable {
            // Owner-side remote mirrors represent live sockets too. Use their
            // acknowledged, keyed gateway; an inviter's ordered-relay origin
            // does not authorize the generated room sender, and this route's
            // whole-invitation fallback must not become a detached SM append.
            if let Some(remote) = deliver_registered_remote_resource(
                &immediate,
                resource,
                &stanza,
                waddle_xmpp::registry::DeliveryKind::DirectFrame,
            )
            .await
            {
                outcome = remote;
            }
        }
        uncertain |= outcome == FullJidDeliveryOutcome::MaybeCommitted;
    }
    check_stop(deps)?;
    #[cfg(test)]
    if let Ok((entered, resume)) = PAUSE_BEFORE_INVITATION_SETTLEMENT.try_with(Clone::clone) {
        entered.notify_one();
        resume.notified().await;
    }
    check_stop(deps)?;
    finish(uow, &invitation, route, quota, uncertain).await
}

async fn transaction(
    uow: &IngressUnitOfWork,
) -> Result<IngressUowTransaction<'_>, IngressUowError> {
    uow.begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
        .await
}

async fn prepare(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    route: &MucUserRoute,
) -> Result<Prepared, StoreError> {
    let key = decision
        .message_key
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    let route_identity = route
        .route_identity
        .clone()
        .ok_or(IngressUowError::EffectIntentConflict)?;
    // The exact immutable route and fallback identity authorize this arm, and
    // both sinks must use the same typed message. Generated room payloads are
    // additionally reconstructed from frozen source and ledger state below.
    if route.fallback.recipient != route.recipient
        || !matches!(&route.fallback.payload, PendingPayload::Transient(message) if message == &route.message)
        || route.message.to.as_ref() != Some(&route.recipient.clone().into())
        || route.message.from.is_none()
        || route
            .resources
            .iter()
            .any(|resource| resource.to_bare() != route.recipient)
    {
        return Err(IngressUowError::EffectIntentConflict.into());
    }
    let mut evidence = [
        IngressEffectIntent::RouteDirect {
            recipient: route.recipient.clone(),
            fanout: route.resources.clone(),
            route_identity,
        },
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient {
                recipient: route.recipient.clone(),
                row_id: route.fallback.id.clone(),
            },
        },
    ];
    for intent in &mut evidence {
        // The durable codec sorts and deduplicates resource sets. Compare its
        // canonical representation without making caller iteration order part
        // of an invitation's delivery authority.
        *intent = intent
            .with_encoded_v1(IngressEffectIntent::decode_v1)
            .map_err(IngressUowError::from)?
            .map_err(IngressUowError::from)?;
    }
    let mut tx = transaction(uow).await?;
    let recorded = EffectIntentRepository::load(&mut tx, key).await?;
    for intent in &evidence {
        if !recorded.contains(intent)
            || !decision.external_receipts[index].contains(&crate::ingress::receipt_key(intent)?)
        {
            return Err(IngressUowError::EffectIntentConflict.into());
        }
    }
    let receipt = crate::ingress::receipt_key(&evidence[0])?;
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await?
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    if let Some(expected) =
        crate::ingress::invitation_authority::recorded_message(&envelope, &recorded, &receipt)?
    {
        if !crate::ingress::append_authority::same_message_content(&expected, &route.message) {
            return Err(IngressUowError::EffectIntentConflict.into());
        }
    }
    let archive_positions = ArchiveDispatchRepository::positions(&mut tx, key, &receipt).await?;
    let invitation = Invitation {
        key,
        evidence,
        context: SmIngressAppendContext {
            message_key: key,
            receipt,
            received_at: Some(route.fallback.original_receipt_at),
            archive_positions,
            dispatch_stream: None,
        },
    };
    if already_resolved(&mut tx, &invitation, route).await? {
        let persisted = settle_pair(&mut tx, &invitation).await?;
        tx.commit().await?;
        return Ok(Prepared::Settled(persisted));
    }
    tx.commit().await?;
    Ok(Prepared::Pending(Box::new(invitation)))
}

async fn finish(
    uow: &IngressUnitOfWork,
    invitation: &Invitation,
    route: &MucUserRoute,
    quota: QuotaPolicy,
    uncertain: bool,
) -> Result<Finished, StoreError> {
    let mut tx = transaction(uow).await?;
    // Take the same canonical lock as live claims before inspecting any
    // resource, including the absent-row case. Keep it through fallback and
    // pair settlement, so no live attempt can race a pending insertion.
    EffectIntentRepository::load(&mut tx, invitation.key).await?;
    if already_resolved(&mut tx, invitation, route).await? {
        let persisted = settle_pair(&mut tx, invitation).await?;
        tx.commit().await?;
        return Ok(Finished::Settled(persisted));
    }
    let mut accepted = 0;
    let mut blocked = false;
    let mut expired_ambiguity = false;
    for recipient in &route.resources {
        expired_ambiguity |= SendAttemptRepository::has_expired_started(
            &mut tx,
            &SendObligation {
                message: invitation.key,
                receipt: invitation.context.receipt.clone(),
                recipient: recipient.clone(),
            },
        )
        .await?;
        match live_delivery::delivery_status(
            &mut tx,
            &SendObligation {
                message: invitation.key,
                receipt: invitation.context.receipt.clone(),
                recipient: recipient.clone(),
            },
        )
        .await?
        {
            Some(FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached) => {
                accepted += 1;
            }
            Some(_) => blocked = true,
            None => {}
        }
    }
    if accepted > 0 && accepted == route.resources.len() {
        let persisted = settle_pair(&mut tx, invitation).await?;
        tx.commit().await?;
        return Ok(Finished::Settled(persisted));
    }
    // Bound unknown outcomes before whole-invitation fallback. Once an old
    // start expires, pending custody may duplicate an already accepted sibling;
    // it must not leave the missing resource unresolved indefinitely.
    if (accepted > 0 && !expired_ambiguity) || blocked || uncertain {
        tx.commit().await?;
        return Ok(Finished::Uncertain);
    }
    let inserted = PendingReceiptRepository::insert(&mut tx, &route.fallback, quota).await?;
    if inserted == InsertOutcome::QuotaExceeded {
        // Canonical acceptance already authorized the membership and ledger.
        // Their best-effort rollback is neither durable nor generation-fenced:
        // terminal receipts followed by compensation can strand authorization
        // on a crash, while replaying compensation can revoke a newer grant.
        // Keep delivery retryable until quota is available, just as for an
        // uncertain send. Do not report a terminal refusal or roll back grants.
        tx.commit().await?;
        return Ok(Finished::Uncertain);
    }
    for recipient in &route.resources {
        SendAttemptRepository::retire_expired_attempt(
            &mut tx,
            &SendObligation {
                message: invitation.key,
                receipt: invitation.context.receipt.clone(),
                recipient: recipient.clone(),
            },
        )
        .await?;
    }
    let persisted = settle_pair(&mut tx, invitation).await?;
    tx.commit().await?;
    Ok(Finished::Settled(persisted))
}

async fn already_resolved(
    tx: &mut IngressUowTransaction<'_>,
    invitation: &Invitation,
    route: &MucUserRoute,
) -> Result<bool, StoreError> {
    for intent in &invitation.evidence {
        let receipt = crate::ingress::receipt_key(intent)?;
        if EffectReceiptRepository::contains(
            tx,
            invitation.key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await?
        {
            return Ok(true);
        }
    }
    // Repair an older insert-before-receipt crash before attempting any live
    // delivery. New fallback inserts and both receipts now commit together.
    Ok(PendingReceiptRepository::contains(tx, &route.fallback.id).await?)
}

async fn settle_pair(
    tx: &mut IngressUowTransaction<'_>,
    invitation: &Invitation,
) -> Result<Vec<IngressEffectIntent>, IngressUowError> {
    let persisted = settle_recorded(tx, invitation.key, &invitation.evidence).await?;
    if !invitation
        .evidence
        .iter()
        .all(|intent| persisted.contains(intent))
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    Ok(persisted)
}

fn check_stop(deps: &Deps<'_>) -> Result<(), IngressUowError> {
    if deps
        .ingress_delivery_stop
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    {
        Err(IngressUowError::AuthorityStopped)
    } else {
        Ok(())
    }
}

fn settled(persisted: Vec<IngressEffectIntent>, completion: SettledCompletion) -> EffectOutcome {
    EffectOutcome::Settled(SettledOutcome {
        refusal: None,
        persisted,
        completion,
        detached: None,
    })
}
