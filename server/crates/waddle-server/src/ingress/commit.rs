//! Phase B: one immutable plan, retried as complete fresh transactions.
use super::{
    decision::{AliasOutcomeClass, IngressDecision, IngressDecisionClass},
    identity::IngressStreamIdentity,
    submission::IngressSubmission,
};
use crate::ingress_substrate::MessageEnvelope;
use crate::ingress_uow::{
    run_with_retry, CanonicalMessageRepository, DbRetryClass, EffectIntentRepository,
    IngressUnitOfWork, IngressUowError, IngressUowTransaction, PrincipalAssertion,
    PrincipalRepository, ReconcileVerdict,
};
use std::time::Instant;
use waddle_xmpp::ingress::{AliasOutcome, AliasResolution, IngressEffectIntent, MessageKey};

#[derive(Debug, thiserror::Error)]
#[error("ingress commit failed ({class:?})")]
pub struct IngressCommitFailure {
    pub class: IngressDecisionClass,
    #[source]
    pub source: IngressUowError,
}
impl IngressCommitFailure {
    pub fn class(&self) -> IngressDecisionClass {
        self.class
    }
}

pub async fn commit_submission(
    uow: &IngressUnitOfWork,
    submission: &IngressSubmission,
    attempts: usize,
) -> Result<IngressDecision, IngressCommitFailure> {
    let started = Instant::now();
    let count = std::sync::atomic::AtomicUsize::new(0);
    let result = run_with_retry(attempts.max(1), || {
        if count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 0 {
            waddle_xmpp::telemetry::reliability::increment_ingress_tx_retry();
        }
        commit_attempt(uow, submission)
    })
    .await
    .map_err(|failure| IngressCommitFailure {
        class: if matches!(
            failure.last_error.retry_class(),
            DbRetryClass::SerializationFailure | DbRetryClass::Deadlock
        ) {
            IngressDecisionClass::SerializationExhaustion
        } else {
            classify_failure(&failure.last_error)
        },
        source: failure.last_error,
    });
    waddle_xmpp::telemetry::reliability::record_ingress_tx_duration(started.elapsed());
    let class = match &result {
        Ok(decision) => decision.class,
        Err(failure) => failure.class,
    };
    waddle_xmpp::telemetry::reliability::increment_ingress_decision(class);
    if let Ok(decision) = &result {
        use waddle_xmpp::telemetry::attributes::IngressAliasOutcome;
        waddle_xmpp::telemetry::reliability::increment_ingress_alias_outcome(
            match decision.alias {
                AliasOutcomeClass::Inserted => IngressAliasOutcome::Inserted,
                AliasOutcomeClass::NoOrigin => IngressAliasOutcome::NoOrigin,
                AliasOutcomeClass::Existing => IngressAliasOutcome::Existing,
                AliasOutcomeClass::Conflict => IngressAliasOutcome::Conflict,
            },
        );
    }
    result
}

pub fn classify_failure(error: &IngressUowError) -> IngressDecisionClass {
    use crate::ingress_substrate::IngressSubstrateError;
    match error {
        IngressUowError::Timeout | IngressUowError::Substrate(IngressSubstrateError::Timeout) => {
            IngressDecisionClass::Timeout
        }
        IngressUowError::PrincipalAssertionFailed => IngressDecisionClass::PrincipalMissing,
        IngressUowError::RoomGenerationStale => IngressDecisionClass::RoomGenerationStale,
        IngressUowError::IngressFrontierStale => IngressDecisionClass::FrontierStale,
        IngressUowError::AmbiguousCommit => IngressDecisionClass::AmbiguousCommit,
        IngressUowError::EffectIntentConflict => IngressDecisionClass::IntentContradiction,
        IngressUowError::Substrate(IngressSubstrateError::SmOrdinalConflict) => {
            IngressDecisionClass::SmOrdinalConflict
        }
        IngressUowError::Lineage(_) => IngressDecisionClass::Lineage,
        IngressUowError::EpochUnsupported { .. }
        | IngressUowError::Substrate(IngressSubstrateError::UnsupportedLiveEpoch) => {
            IngressDecisionClass::EpochUnsupported
        }
        #[cfg(feature = "clustering")]
        IngressUowError::ClaimFenceMissing | IngressUowError::NodeIdentityUnbound => {
            IngressDecisionClass::ClaimFenceMissing
        }
        #[cfg(feature = "clustering")]
        IngressUowError::FrontierStale { .. } => IngressDecisionClass::FrontierStale,
        _ => IngressDecisionClass::Storage,
    }
}

async fn commit_attempt(
    uow: &IngressUnitOfWork,
    submission: &IngressSubmission,
) -> Result<IngressDecision, IngressUowError> {
    let mut tx = uow
        .begin_with_timeouts(
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(250),
        )
        .await?;
    if PrincipalRepository::assert_principal(&mut tx, &submission.principal).await?
        != PrincipalAssertion::Asserted
    {
        return Err(IngressUowError::PrincipalAssertionFailed);
    }
    if matches!(&submission.identity, IngressStreamIdentity::Ephemeral { principal } if principal != &submission.principal)
    {
        return Err(IngressUowError::PrincipalAssertionFailed);
    }
    if let IngressStreamIdentity::Relayed { canonical, .. } = &submission.identity {
        if canonical.sender_bare != *submission.principal.bare_jid()
            || canonical.origin_id.as_ref() != submission.digest_input.origin()
        {
            return Err(IngressUowError::PrincipalAssertionFailed);
        }
    }
    #[cfg(test)]
    if let Some(error) = forced_failures::take_failure() {
        return Err(error);
    }
    let stream = super::commit_stream::lock_stream(&mut tx, &submission.identity).await?;
    let digest = waddle_xmpp::ingress::digest::v1::digest(&submission.digest_input);
    let envelope = MessageEnvelope::new(submission.plan.sanitized_message.clone())?;
    let mut rejection = super::rejection::planned_rejection(&submission.plan);
    let (key, alias) = if let Some((key, _)) = stream.as_ref().and_then(|stream| stream.bound) {
        (key, AliasOutcomeClass::Existing)
    } else if rejection.is_some() {
        (MessageKey::new(), AliasOutcomeClass::NoOrigin)
    } else if let IngressStreamIdentity::Relayed { canonical, .. } = &submission.identity {
        (canonical.message_key, AliasOutcomeClass::Existing)
    } else if let Some(origin) = submission.digest_input.origin() {
        match CanonicalMessageRepository::resolve_and_record_alias(
            &mut tx,
            submission.principal.bare_jid(),
            &submission.target,
            origin,
            &digest,
            MessageKey::new,
        )
        .await?
        {
            AliasResolution::Aliased(AliasOutcome::Existing(key)) => {
                (key, AliasOutcomeClass::Existing)
            }
            AliasResolution::Aliased(AliasOutcome::Inserted(key)) => {
                (key, AliasOutcomeClass::Inserted)
            }
            AliasResolution::NoOrigin(key) => (key, AliasOutcomeClass::NoOrigin),
            AliasResolution::Aliased(AliasOutcome::Conflict(_)) => {
                rejection = Some(IngressDecisionClass::AliasConflict);
                (MessageKey::new(), AliasOutcomeClass::Conflict)
            }
        }
    } else {
        (MessageKey::new(), AliasOutcomeClass::NoOrigin)
    };
    if alias != AliasOutcomeClass::Existing {
        CanonicalMessageRepository::record_message(&mut tx, key, &digest, Some(&envelope)).await?;
    }
    if !CanonicalMessageRepository::lock(&mut tx, key).await? {
        return Err(IngressUowError::EffectIntentMessageMissing);
    }
    if stream.as_ref().is_some_and(|stream| stream.bound.is_some())
        || (alias == AliasOutcomeClass::Existing
            && matches!(submission.identity, IngressStreamIdentity::Relayed { .. }))
    {
        CanonicalMessageRepository::record_message(&mut tx, key, &digest, None).await?;
    }
    #[cfg(test)]
    if forced_failures::consume_serialization_failure() {
        return Err(IngressUowError::Database {
            retry_class: DbRetryClass::SerializationFailure,
        });
    }
    let room_proof =
        super::commit_room::assert_room(&mut tx, submission, rejection.is_none()).await?;
    let recorded = EffectIntentRepository::load(&mut tx, key).await?;
    let bound = stream.as_ref().is_some_and(|stream| stream.bound.is_some());
    // A policy change cannot replace an already committed acceptance with a
    // rejection. Preserve its recorded obligations; any work absent from this
    // new plan stays pending for recovery rather than inventing a new denial.
    let replay_acceptance = bound && !super::rejection::is_recorded_rejection(&recorded);
    let discard_new_denial = replay_acceptance && rejection.take().is_some();
    let recorded_ids = archive_ids(&recorded);
    let owner_first = matches!(&submission.identity, IngressStreamIdentity::Relayed { room, .. } if !recorded.iter().any(|intent| matches!(intent, IngressEffectIntent::ArchiveAuthoritative { by, .. } if by == room)));
    let mut plan = if stream.as_ref().is_some_and(|stream| stream.bound.is_some())
        && super::rejection::is_recorded_rejection(&recorded)
    {
        let recorded_envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await?
            .ok_or(IngressUowError::EffectIntentMessageMissing)?;
        rejection = Some(IngressDecisionClass::ExistingCommitted);
        super::rejection::recorded_rejection_plan(&recorded_envelope, &recorded)?
    } else if discard_new_denial {
        let mut accepted = submission.plan.clone();
        accepted.plan.clear();
        accepted.intents.clear();
        accepted.error_reply = None;
        accepted
    } else if let Some(class) = rejection {
        super::rejection::rejection_plan(&submission.plan, class)?
    } else {
        super::restamp::restamp_plan(&submission.plan, &recorded_ids)
    };
    // Timestamp is part of immutable archive authority, alongside the trusted ID.
    for intent in &mut plan.intents {
        if let IngressEffectIntent::ArchiveAuthoritative {
            archive,
            by,
            archived_at,
            ..
        } = intent
        {
            if let Some(IngressEffectIntent::ArchiveAuthoritative { archived_at: stored, .. }) = recorded.iter().find(|row| matches!(row, IngressEffectIntent::ArchiveAuthoritative { archive: a, by: b, .. } if a == archive && b == by)) {
                *archived_at = *stored;
            }
        }
    }
    if alias == AliasOutcomeClass::Existing
        && !owner_first
        && rejection.is_none()
        && recorded_ids.is_empty()
        && stream.as_ref().is_none_or(|stream| stream.bound.is_none())
    {
        return Err(IngressUowError::EffectIntentMessageMissing);
    }
    let verdict = EffectIntentRepository::reconcile(&mut tx, key, &plan.intents).await?;
    if matches!(verdict, ReconcileVerdict::Contradiction { .. }) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let intents = EffectIntentRepository::load(&mut tx, key).await?;
    let plan = super::recorded::apply_recorded_intents(&plan, &intents);
    let applied =
        super::durable::apply_durable(&mut tx, key, &plan, &recorded, &room_proof).await?;
    let ordinal = stream.as_ref().map(|stream| stream.ordinal);
    super::commit_stream::finish_stream(&mut tx, stream.as_ref(), key).await?;
    let class = rejection.unwrap_or_else(|| {
        decision_class(
            submission,
            &verdict,
            alias,
            stream.as_ref().is_some_and(|s| s.bound.is_some()),
            owner_first,
        )
    });
    let filter_verdict = if owner_first {
        &ReconcileVerdict::FirstCommit
    } else if bound || matches!(class, IngressDecisionClass::OwnerDuplicate) {
        &ReconcileVerdict::Consistent
    } else {
        &verdict
    };
    let external =
        super::suppression::filter_external_effects(&plan, filter_verdict, &applied.archives);
    let mut pending = Vec::new();
    for intent in &intents {
        let receipt = super::durable::receipt_key(intent)?;
        if !crate::ingress_uow::EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await?
        {
            pending.push(receipt);
        }
    }
    let external_receipts = super::durable::external_receipts(&external, &intents)?;
    let decision = IngressDecision {
        class,
        message_key: Some(key),
        ordinal,
        alias,
        verdict: Some(verdict),
        archive_ids: archive_ids(&intents),
        external,
        external_receipts,
        receipts_pending: pending,
    };
    commit_transaction(tx).await?;
    Ok(decision)
}

pub(crate) async fn commit_transaction(
    tx: IngressUowTransaction<'_>,
) -> Result<(), IngressUowError> {
    let result = tx.commit().await;
    #[cfg(test)]
    if result.is_ok() && forced_failures::ambiguous_commit() {
        return Err(IngressUowError::AmbiguousCommit);
    }
    result.map_err(|error| {
        if matches!(
            error.retry_class(),
            DbRetryClass::SerializationFailure | DbRetryClass::Deadlock
        ) {
            error
        } else {
            IngressUowError::AmbiguousCommit
        }
    })
}
fn archive_ids(
    intents: &[IngressEffectIntent],
) -> Vec<(jid::BareJid, waddle_xmpp_core::xep0359::StanzaId)> {
    intents
        .iter()
        .filter_map(|intent| match intent {
            IngressEffectIntent::ArchiveAuthoritative {
                archive, stanza_id, ..
            } => Some((archive.clone(), stanza_id.clone())),
            _ => None,
        })
        .collect()
}
fn decision_class(
    submission: &IngressSubmission,
    verdict: &ReconcileVerdict,
    alias: AliasOutcomeClass,
    bound: bool,
    owner_first: bool,
) -> IngressDecisionClass {
    if bound {
        return IngressDecisionClass::ExistingCommitted;
    }
    if matches!(submission.identity, IngressStreamIdentity::Relayed { .. }) {
        return if owner_first {
            IngressDecisionClass::OwnerFirstAcceptance
        } else {
            IngressDecisionClass::OwnerDuplicate
        };
    }
    if alias != AliasOutcomeClass::Existing {
        return IngressDecisionClass::Accepted;
    }
    match verdict {
        ReconcileVerdict::Repaired { .. } => IngressDecisionClass::ExistingRepaired,
        ReconcileVerdict::Divergent { .. } => IngressDecisionClass::ExistingDivergent,
        _ => IngressDecisionClass::ExistingConsistent,
    }
}

#[cfg(test)]
mod forced_failures {
    use crate::ingress_uow::IngressUowError;
    tokio::task_local! {
        pub static SERIALIZATION_FAILURES: std::cell::Cell<usize>;
        pub static AMBIGUOUS_COMMIT: bool;
        pub static FAILURE: std::cell::RefCell<Option<IngressUowError>>;
    }
    pub(super) fn consume_serialization_failure() -> bool {
        SERIALIZATION_FAILURES
            .try_with(|remaining| {
                let count = remaining.get();
                remaining.set(count.saturating_sub(1));
                count > 0
            })
            .unwrap_or(false)
    }
    pub(super) fn take_failure() -> Option<IngressUowError> {
        FAILURE
            .try_with(|failure| failure.borrow_mut().take())
            .ok()
            .flatten()
    }
    pub(super) fn ambiguous_commit() -> bool {
        AMBIGUOUS_COMMIT.try_with(|value| *value).unwrap_or(false)
    }
}
#[cfg(test)]
#[path = "commit_tests.rs"]
mod tests;
