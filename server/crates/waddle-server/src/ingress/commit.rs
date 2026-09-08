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
        IngressUowError::RoomGenerationStale
        | IngressUowError::Plan(
            crate::server::routes::interpret::effects::PlanFailure::RoomClaimStale,
        ) => IngressDecisionClass::RoomGenerationStale,
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
    if let Some(failure) = submission.plan.failure {
        return Err(failure.into());
    }
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
    let envelope = MessageEnvelope::new(submission.plan.sanitized_message.clone());
    let mut rejection = super::rejection::planned_rejection(&submission.plan)?;
    let (key, alias) = if let Some((key, _)) = stream.as_ref().and_then(|stream| stream.bound) {
        (key, AliasOutcomeClass::Existing)
    } else if rejection.is_some() {
        // A committed denial owns its origin id like any acceptance: the alias
        // is what lets a retransmission on a new wire position find the
        // recorded authority instead of re-deciding under today's policy.
        match submission.digest_input.origin() {
            Some(origin) => match CanonicalMessageRepository::resolve_and_record_alias(
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
            },
            None => (MessageKey::new(), AliasOutcomeClass::NoOrigin),
        }
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
    let replay_acceptance =
        alias == AliasOutcomeClass::Existing && !super::rejection::is_recorded_rejection(&recorded);
    let discard_new_denial = replay_acceptance && rejection.take().is_some();
    let recorded_ids = archive_ids(&recorded);
    let owner_first = matches!(&submission.identity, IngressStreamIdentity::Relayed { room, .. } if !recorded.iter().any(|intent| matches!(intent, IngressEffectIntent::ArchiveAuthoritative { by, .. } if by == room)));
    // A recorded denial is authority: any retransmission that resolves to it
    // re-emits the committed reply, whatever today's policy would decide.
    let mut plan = if alias == AliasOutcomeClass::Existing
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
    // Recorded obligations still missing their receipts. These are the only
    // ones a replay may repair; everything else is provably complete.
    let mut unreceipted = Vec::new();
    for intent in &recorded {
        let receipt = super::durable::receipt_key(intent)?;
        if !crate::ingress_uow::EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await?
        {
            unreceipted.push(intent.clone());
        }
    }
    reconcile_invitation_delivery_receipts(&mut tx, key, &recorded, &mut unreceipted).await?;
    // Recorded work this commit rebuilt from the canonical envelope. Such a
    // repair is not a duplicate fan-out: today's plan could not produce it.
    let mut reconstructed = false;
    if alias == AliasOutcomeClass::Existing {
        retain_live_recipient_plan(submission, &recorded, &mut plan);
        let recorded_envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await?
            .ok_or(IngressUowError::EffectIntentMessageMissing)?;
        reconstructed = crate::server::routes::websocket::handlers::message::dm_pin::restore_recorded_dm_pin_effects(
            &mut plan, &recorded, &recorded_envelope,
        )?;
        reconstructed |= crate::server::routes::websocket::handlers::message::group_dm_invite::restore_recorded_group_dm_invite(
            &mut plan, &submission.plan, &recorded, &unreceipted, &recorded_envelope,
        )?;
        reconstructed |= crate::server::routes::websocket::handlers::message::muc_direct::restore_recorded_muc_decline(
            &mut plan, &recorded, &unreceipted, &recorded_envelope,
        )?;
    }
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
        && (missing_planned_archive_authority(&plan.intents, &recorded)
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
    let verdict = EffectIntentRepository::reconcile(
        &mut tx,
        key,
        &plan.intents,
        alias == AliasOutcomeClass::Existing,
    )
    .await?;
    if matches!(verdict, ReconcileVerdict::Contradiction { .. }) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let intents = EffectIntentRepository::load(&mut tx, key).await?;
    // Divergent and initially empty plans can also insert omitted obligations.
    // Reopen under the canonical lock before committing any newly pending work.
    if intents.len() > recorded.len() {
        CanonicalMessageRepository::clear_terminal(&mut tx, key).await?;
    }
    let mut plan = super::recorded::apply_recorded_intents(&plan, &intents);
    // An accepted canonical row with no recorded obligations authorizes nothing.
    // Current policy may plan work for it again (a block that has since been
    // lifted, a newly enabled extension); replaying it would invent obligations
    // the original acceptance never committed (RFC 0018 §3).
    if alias == AliasOutcomeClass::Existing && intents.is_empty() && !owner_first {
        // Only the sender-visible reply the committed envelope already owns may
        // still be written; it carries no obligation and mutates no host state.
        plan.plan.retain(|planned| {
            matches!(
                &planned.effect,
                crate::server::routes::interpret::effects::Effect::External(
                    crate::server::routes::interpret::effects::ExternalEffect::Frame(_)
                )
            )
        });
    }
    if let Some(observer_envelope) = super::recorded::room_observer_envelope(&plan) {
        CanonicalMessageRepository::record_room_observer_envelope(&mut tx, key, &observer_envelope)
            .await?;
    }
    if intents
        .iter()
        .any(|intent| matches!(intent, IngressEffectIntent::RoomObserver { .. }))
    {
        let recorded_envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await?
            .ok_or(IngressUowError::EffectIntentMessageMissing)?;
        super::recorded::restore_room_observer_envelope(&mut plan, &intents, &recorded_envelope)?;
    }
    if plan
        .intents
        .iter()
        .any(|intent| matches!(intent, IngressEffectIntent::RoomSubjectMutation { .. }))
    {
        let recorded_envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await?
            .ok_or(IngressUowError::EffectIntentMessageMissing)?;
        super::recorded::restore_subject_rejection_replies(&mut plan, &recorded_envelope)?;
    }
    // Actor handles retained during planning are replay context, not authority.
    // Only a recorded grant permits the reconstructed membership mutation.
    plan.plan.retain(|planned| {
        use crate::server::routes::interpret::effects::{early::RoomMembershipMutation, Effect, ExternalEffect};
        match &planned.effect {
            Effect::External(ExternalEffect::RoomMembershipMutation(RoomMembershipMutation::GroupDm(mutation))) =>
                plan.intents.iter().any(|intent| matches!(intent,
                    IngressEffectIntent::GroupDmMembershipGrant { grant } if grant == &mutation.grant)),
            _ => true,
        }
    });
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
    let repairable = if reconstructed {
        unreceipted.as_slice()
    } else {
        &[][..]
    };
    let external = super::suppression::filter_external_effects(
        &plan,
        filter_verdict,
        &applied.archives,
        repairable,
    );
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
    let external_dependencies = super::suppression::external_effect_indices(
        &plan,
        filter_verdict,
        &applied.archives,
        repairable,
    )
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
    let mut external = external;
    for effect in &mut external {
        if let crate::server::routes::interpret::effects::ExternalEffect::InviteLedger(
            crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerMutation::Claim { message_key, .. }
        ) = effect {
            *message_key = Some(key);
        }
    }
    let external_receipts = super::durable::external_receipts(&external, &intents)?;
    let route_progress = Vec::new();
    let mut arm_owned_receipts = Vec::new();
    for (index, effect) in external.iter().enumerate() {
        if super::execute_uow::owns(effect, &route_progress) {
            for receipt in &external_receipts[index] {
                if !arm_owned_receipts.contains(receipt) {
                    arm_owned_receipts.push(receipt.clone());
                }
            }
        }
    }
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
        arm_owned_receipts,
        route_progress,
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
/// A live full-JID route delegates recipient preparation to that connection.
/// Disconnecting later cannot move those obligations into the sender transaction.
fn retain_live_recipient_plan(
    submission: &IngressSubmission,
    recorded: &[IngressEffectIntent],
    plan: &mut super::IngressPlan,
) {
    use super::effects::{
        delivery::ExternalDeliveryEffect,
        direct::{DurableDirectEffect, ExternalDirectEffect},
        DurableEffect, Effect, ExternalEffect,
    };
    use waddle_xmpp::ingress::{
        EffectAuthorityKey, EffectMessageIdentity, NormalizedTarget, PendingDeliveryMutation,
    };
    let NormalizedTarget::Full(full) = &submission.target else {
        return;
    };
    let recipient = full.to_bare();
    if recipient == *submission.principal.bare_jid()
        || recorded.iter().any(|intent| matches!(intent,
            IngressEffectIntent::ArchiveAuthoritative { archive, .. } if archive == &recipient))
        || !recorded.iter().any(|intent| matches!(intent,
            IngressEffectIntent::RouteDirect { recipient: saved, fanout, route_identity: EffectMessageIdentity::CaptureOrdinal(_) }
                if saved == &recipient && fanout.as_slice() == [full.clone()]))
    {
        return;
    }
    plan.intents.retain(|intent| {
        let recipient_preparation = match intent.authority_key() {
            EffectAuthorityKey::Archive { archive, .. }
            | EffectAuthorityKey::Media { archive, .. }
            | EffectAuthorityKey::Retraction { archive, .. } => archive == recipient,
            EffectAuthorityKey::Inbox { owner, .. }
            | EffectAuthorityKey::Conversation { owner, .. } => owner == recipient,
            _ => matches!(intent,
                IngressEffectIntent::PendingDelivery {
                    mutation: PendingDeliveryMutation::Archived { recipient: owner, .. }
                        | PendingDeliveryMutation::Transient { recipient: owner, .. },
                } if owner == &recipient),
        };
        !recipient_preparation || recorded.contains(intent)
    });
    // Archive references describe ordering, not ownership: a sender archive
    // or sent carbon can reference a recipient-assigned stanza-id sibling.
    plan.plan.retain_mut(|planned| {
        if let Effect::External(ExternalEffect::Direct(
            ExternalDirectEffect::LinkPreviewRefs { mutations }
            | ExternalDirectEffect::ClearLinkPreviewRefs { mutations },
        )) = &mut planned.effect
        {
            mutations.retain(|mutation| mutation.archive != recipient);
            return !mutations.is_empty();
        }
        match &planned.effect {
            Effect::Durable(DurableEffect::Direct(effect)) => match effect {
                DurableDirectEffect::ArchiveDirect { archive, .. }
                | DurableDirectEffect::RetractionTombstone { archive, .. } => archive != &recipient,
                DurableDirectEffect::ProjectInbox { owner, .. }
                | DurableDirectEffect::MarkInboxRead { owner, .. }
                | DurableDirectEffect::DmCallThreadProjection { owner, .. } => owner != &recipient,
            },
            Effect::External(ExternalEffect::Direct(
                ExternalDirectEffect::NotificationActivity { owner, .. }
                | ExternalDirectEffect::PushInboxUpdate { owner, .. },
            )) => owner != &recipient,
            Effect::External(ExternalEffect::Delivery(
                ExternalDeliveryEffect::QueueOfflineDelivery { row, .. },
            )) => row.recipient != recipient,
            Effect::External(ExternalEffect::Delivery(
                ExternalDeliveryEffect::Carbons { owner, .. }
                | ExternalDeliveryEffect::RelayCarbons { owner, .. },
            )) => owner != &recipient,
            _ => true,
        }
    });
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

#[cfg(test)]
#[path = "alias_denial_replay_tests.rs"]
mod alias_denial_replay_tests;

/// Live invitation delivery and its offline fallback are mutually exclusive.
/// Repair a partially persisted pair before rebuilding any delivery effects.
async fn reconcile_invitation_delivery_receipts(
    tx: &mut crate::ingress_uow::IngressUowTransaction<'_>,
    key: waddle_xmpp::ingress::MessageKey,
    recorded: &[IngressEffectIntent],
    pending: &mut Vec<IngressEffectIntent>,
) -> Result<(), IngressUowError> {
    use waddle_xmpp::ingress::PendingDeliveryMutation;
    for recipient in recorded.iter().filter_map(|intent| match intent {
        IngressEffectIntent::GroupDmMembershipGrant { grant } => Some(&grant.invitee),
        IngressEffectIntent::MucInviteLedger { mutation }
            if mutation.action == waddle_xmpp::ingress::MucInviteLedgerAction::Claimed =>
        {
            Some(&mutation.inviter)
        }
        _ => None,
    }) {
        let route = recorded.iter().find(|intent| {
            matches!(intent,
            IngressEffectIntent::RouteDirect { recipient: target, .. } if target == recipient)
        });
        let fallback = recorded.iter().find(|intent| {
            matches!(intent,
            IngressEffectIntent::PendingDelivery {
                mutation: PendingDeliveryMutation::Transient { recipient: target, .. }
            } if target == recipient)
        });
        let (Some(route), Some(fallback)) = (route, fallback) else {
            continue;
        };
        if pending.contains(route) && pending.contains(fallback) {
            continue;
        }
        for intent in [route, fallback] {
            if pending.contains(intent) {
                let receipt = super::durable::receipt_key(intent)?;
                crate::ingress_uow::EffectReceiptRepository::record_receipt(
                    tx,
                    key,
                    receipt.kind,
                    &receipt.semantic_identity_hash,
                )
                .await?;
                pending.retain(|candidate| candidate != intent);
            }
        }
    }
    Ok(())
}
