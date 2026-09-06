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
    commit_hooks::observe_class(class);
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
    if let Some(error) = commit_hooks::take_failure() {
        return Err(error);
    }
    // A relayed retry must prove the room claim before touching canonical identity.
    if matches!(submission.identity, IngressStreamIdentity::Relayed { .. }) {
        super::commit_room::assert_room(&mut tx, submission, false).await?;
    }
    let stream = super::commit_stream::lock_stream(&mut tx, &submission.identity).await?;
    let digest = waddle_xmpp::ingress::digest::v1::digest(&submission.digest_input);
    let envelope = MessageEnvelope::new(submission.plan.sanitized_message.clone())?;
    let mut rejection = super::rejection::planned_rejection(&submission.plan)?;
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
    if commit_hooks::consume_serialization_failure() {
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
        accepted.rejection = None;
        accepted
    } else if let Some(class) = rejection {
        super::rejection::rejection_plan(&submission.plan, class, &submission.sender)?
    } else {
        super::restamp::restamp_plan(&submission.plan, &recorded_ids)
    };
    // Each generated message retains its own timestamp and assigning authority.
    for intent in &mut plan.intents {
        let authority = intent.authority_key();
        if let IngressEffectIntent::ArchiveAuthoritative { archived_at, .. }
        | IngressEffectIntent::SystemMessageArchive { archived_at, .. } = intent
        {
            if let Some(
                IngressEffectIntent::ArchiveAuthoritative {
                    archived_at: stored,
                    ..
                }
                | IngressEffectIntent::SystemMessageArchive {
                    archived_at: stored,
                    ..
                },
            ) = recorded.iter().find(|row| row.authority_key() == authority)
            {
                *archived_at = *stored;
            }
        }
    }
    // Archive-free plans (for example invitations) have no archive authority to
    // recover. A remote room origin still needs its recorded dispatch obligation
    // until the owner has supplied the canonical archive identity.
    if alias == AliasOutcomeClass::Existing
        && !owner_first
        && rejection.is_none()
        && (missing_planned_archive_authority(&submission.plan.intents, &recorded)
            || (recorded_ids.is_empty()
                && matches!(
                    &submission.plan.room_execution,
                    super::RoomExecutionPath::Remote { .. }
                )))
        && !owner_acceptance_pending(submission, &recorded)
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
    #[cfg(feature = "clustering")]
    let external = {
        use crate::server::routes::interpret::effects::room::ExternalRoomEffect;
        let mut external = external;
        for effect in &mut external {
            if let super::ExternalEffect::Room(ExternalRoomEffect::RelayMucProxy {
                admission,
                ..
            }) = effect
            {
                *admission = Some(super::identity::IngressRelayAdmission {
                    canonical: super::IngressCanonicalRef {
                        message_key: key,
                        sender_bare: submission.principal.bare_jid().clone(),
                        origin_id: submission.digest_input.origin().cloned(),
                    },
                    principal: submission.principal.clone(),
                    stanza_lang: submission.digest_input.stanza_lang().cloned(),
                });
            }
        }
        external
    };
    let external_dependencies =
        super::suppression::external_effect_indices(&plan, filter_verdict, &applied.archives)
            .into_iter()
            .map(|index| plan.plan[index].dependencies.clone())
            .collect();
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
        archive_ids: archive_ids(&intents)
            .into_iter()
            .map(|(archive, _, id)| (archive, id))
            .collect(),
        applied_durable: std::sync::Arc::new(applied.outcomes),
        external_dependencies,
        external,
        external_receipts,
        receipts_pending: pending,
    };
    commit_transaction(tx).await?;
    Ok(decision)
}

/// A remote groupchat origin commits before the owner can assign its archive ID.
/// Only that recorded room obligation can justify an alias without archive IDs.
fn owner_acceptance_pending(
    submission: &IngressSubmission,
    recorded: &[IngressEffectIntent],
) -> bool {
    use super::RoomExecutionPath;
    use waddle_xmpp::ingress::NormalizedTarget;
    let NormalizedTarget::Bare(target) = &submission.target else {
        return false;
    };
    submission.plan.sanitized_message.type_ == xmpp_parsers::message::MessageType::Groupchat
        && matches!(&submission.plan.room_execution, RoomExecutionPath::Remote { room, .. } if room == target)
        && recorded.iter().any(|intent| {
            matches!(intent,
            IngressEffectIntent::DispatchToRoomRemote { room, .. } if room == target)
        })
}

pub(crate) async fn commit_transaction(
    tx: IngressUowTransaction<'_>,
) -> Result<(), IngressUowError> {
    let result = tx.commit().await;
    if result.is_ok() && commit_hooks::ambiguous_commit() {
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
fn missing_planned_archive_authority(
    planned: &[IngressEffectIntent],
    recorded: &[IngressEffectIntent],
) -> bool {
    planned.iter().any(|intent| {
        matches!(
            intent,
            IngressEffectIntent::ArchiveAuthoritative { .. }
                | IngressEffectIntent::SystemMessageArchive { .. }
        ) && !recorded
            .iter()
            .any(|stored| stored.authority_key() == intent.authority_key())
    })
}

fn archive_ids(
    intents: &[IngressEffectIntent],
) -> Vec<(
    jid::BareJid,
    waddle_xmpp::ingress::ArchiveRole,
    waddle_xmpp_core::xep0359::StanzaId,
)> {
    intents
        .iter()
        .filter_map(|intent| match intent {
            IngressEffectIntent::ArchiveAuthoritative {
                archive, stanza_id, ..
            } => Some((
                archive.clone(),
                waddle_xmpp::ingress::ArchiveRole::Sender,
                stanza_id.clone(),
            )),
            IngressEffectIntent::SystemMessageArchive {
                archive,
                sequence,
                stanza_id,
                ..
            } => Some((
                archive.clone(),
                waddle_xmpp::ingress::ArchiveRole::SystemMessage {
                    sequence: *sequence,
                },
                stanza_id.clone(),
            )),
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

pub(crate) mod commit_hooks;
#[cfg(test)]
#[path = "commit_tests.rs"]
mod tests;
