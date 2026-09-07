//! Bounded post-commit execution. Failures never revise ingress authority.
use std::time::Duration;

use waddle_xmpp::{ingress::MessageKey, Stanza};

use crate::{
    db::Database,
    ingress_uow::{
        CanonicalMessageRepository, EffectReceiptRepository, IngressUnitOfWork, IngressUowError,
        IngressUowTransaction,
    },
    server::routes::interpret::{
        effects::{
            delivery::ExternalDeliveryEffect, direct::ExternalDirectEffect, Effect, EffectOutcome,
            ExternalEffect, ImmediateSink, PlannedEffect,
        },
        Deps, FullJidDeliveryOutcome,
    },
};

use super::decision::{EffectReceiptKey, IngressDecision};

#[path = "execute_archive.rs"]
mod archive;

#[path = "execute_dependencies.rs"]
mod dependencies;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalOutcome {
    Done,
    Failed,
    Uncertain,
    /// Frames have been prepared but their transport write is not confirmed.
    AwaitingFrameDelivery,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionPersistenceFailure {
    #[error("post-commit persistence failed: {0}")]
    Storage(#[from] IngressUowError),
    #[cfg(feature = "clustering")]
    #[error("relay frame receipt confirmation failed: {0}")]
    RelayConfirmation(#[from] crate::clustering::relay::RelayAskError),
    #[cfg(feature = "clustering")]
    #[error("relay owner could not persist reply receipts")]
    RelayConfirmationDeclined,
    #[error("post-commit persistence budget exhausted")]
    BudgetExhausted,
}

/// Owner receipts carried to the origin's actual response write boundary.
#[cfg(feature = "clustering")]
#[derive(Clone)]
pub struct RelayFrameReceiptCompletion {
    inner: std::sync::Arc<tokio::sync::Mutex<RelayFrameReceiptTarget>>,
}

#[cfg(feature = "clustering")]
enum RelayFrameReceiptTarget {
    Local(crate::clustering::route_bridge::RelayFrameCompletion),
    Remote {
        owner: crate::clustering::NodeId,
        token: crate::clustering::relay::RelayReplyReceiptToken,
        stop_token: tokio_util::sync::CancellationToken,
    },
}

#[cfg(feature = "clustering")]
impl std::fmt::Debug for RelayFrameReceiptCompletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RelayFrameReceiptCompletion")
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "clustering")]
impl RelayFrameReceiptCompletion {
    pub(crate) fn new(completion: crate::clustering::route_bridge::RelayFrameCompletion) -> Self {
        Self {
            inner: std::sync::Arc::new(tokio::sync::Mutex::new(RelayFrameReceiptTarget::Local(
                completion,
            ))),
        }
    }

    pub(crate) fn remote(
        owner: crate::clustering::NodeId,
        token: crate::clustering::relay::RelayReplyReceiptToken,
        stop_token: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            inner: std::sync::Arc::new(tokio::sync::Mutex::new(RelayFrameReceiptTarget::Remote {
                owner,
                token,
                stop_token,
            })),
        }
    }

    pub async fn complete(&self) -> Result<bool, ExecutionPersistenceFailure> {
        let mut target = self.inner.lock().await;
        match &mut *target {
            RelayFrameReceiptTarget::Local(completion) => {
                let authority = std::sync::Arc::clone(&completion.authority);
                Box::pin(authority.complete_frame_obligations(&mut completion.report)).await
            }
            RelayFrameReceiptTarget::Remote {
                owner,
                token,
                stop_token,
            } => {
                let mut handle =
                    crate::clustering::relay::RelayHandle::new(owner.clone(), stop_token.clone());
                if handle.confirm_reply_receipt(*token).await? {
                    Ok(true)
                } else {
                    Err(ExecutionPersistenceFailure::RelayConfirmationDeclined)
                }
            }
        }
    }
}

/// Frames belonging to one external effect, with its durable receipt obligations.
#[derive(Debug)]
pub struct FrameObligation {
    pub frames: Vec<Stanza>,
    pub receipt_keys: Vec<EffectReceiptKey>,
    effect_index: usize,
}

#[derive(Debug, Default)]
pub struct ExecutionReport {
    pub outcomes: Vec<(ExternalEffect, ExternalOutcome)>,
    pub frame_obligations: Vec<FrameObligation>,
    #[cfg(feature = "clustering")]
    relay_frame_completions: Vec<RelayFrameReceiptCompletion>,
    message_key: Option<MessageKey>,
    frame_completion_receipts: Vec<EffectReceiptKey>,
    /// A completed side effect can remain unresolved when its receipt write fails.
    pub receipt_failures: Vec<(EffectReceiptKey, ExecutionPersistenceFailure)>,
    pub terminalization_failure: Option<ExecutionPersistenceFailure>,
}

impl ExecutionReport {
    #[cfg(feature = "clustering")]
    pub(crate) fn retain_relay_frame_completion(
        &mut self,
        completion: RelayFrameReceiptCompletion,
    ) {
        self.relay_frame_completions.push(completion);
    }

    #[cfg(feature = "clustering")]
    pub(super) async fn complete_relay_frame_obligations(
        &self,
    ) -> Result<(), ExecutionPersistenceFailure> {
        for completion in &self.relay_frame_completions {
            completion.complete().await?;
        }
        Ok(())
    }

    /// Call only after every frame in `frame_obligations` was successfully written.
    /// Dropping the report on cancellation or write failure leaves receipts pending.
    /// Receipt persistence is idempotent, so this may be retried without writing frames again.
    pub async fn complete_frame_obligations(
        &mut self,
        uow: &IngressUnitOfWork,
        db: &Database,
        budget: Duration,
    ) -> Result<bool, ExecutionPersistenceFailure> {
        if budget.is_zero() {
            return Err(ExecutionPersistenceFailure::BudgetExhausted);
        }
        let Some(message_key) = self.message_key else {
            return Ok(false);
        };
        for obligation in &self.frame_obligations {
            if self.outcomes[obligation.effect_index].1 == ExternalOutcome::AwaitingFrameDelivery {
                self.outcomes[obligation.effect_index].1 = ExternalOutcome::Done;
            }
        }
        tokio::time::timeout(budget, async {
            for key in &self.frame_completion_receipts {
                EffectReceiptRepository::record_receipt_pooled(
                    db,
                    message_key,
                    key.kind,
                    &key.semantic_identity_hash,
                )
                .await?;
            }
            terminalize_if_complete(uow, message_key).await
        })
        .await
        .map_err(|_| ExecutionPersistenceFailure::BudgetExhausted)?
        .map_err(ExecutionPersistenceFailure::from)
    }
}

pub async fn execute_effects(
    uow: &IngressUnitOfWork,
    db: &Database,
    decision: &IngressDecision,
    sink: &ImmediateSink,
    deps: &Deps<'_>,
    budget: Duration,
) -> ExecutionReport {
    let mut report = ExecutionReport::default();
    if !decision.class.advances() {
        return report;
    }
    report.message_key = decision.message_key;
    let deadline = tokio::time::Instant::now() + budget;
    let mut recorded = Vec::new();
    let mut proven = vec![Vec::new(); decision.external.len()];
    let mut completed = vec![None; decision.external.len()];
    let mut planned = decision
        .external
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, effect)| {
            let mut planned = PlannedEffect::new(Effect::External(effect));
            planned.dependencies = decision
                .external_dependencies
                .get(index)
                .cloned()
                .unwrap_or_default();
            planned
        })
        .collect::<Vec<_>>();
    report.outcomes = decision
        .external
        .iter()
        .cloned()
        .map(|effect| (effect, ExternalOutcome::Failed))
        .collect();
    while completed.iter().any(Option::is_none) {
        let next = planned.iter().enumerate().find_map(|(index, effect)| {
            if completed[index].is_some() {
                return None;
            }
            dependencies::ready(&effect.dependencies, &decision.external, &completed)
                .map(|ready| (index, ready))
        });
        let Some((index, ready)) = next else {
            // A dependency cycle cannot execute; preserve and meter every obligation.
            for (index, result) in completed.iter_mut().enumerate() {
                if result.is_none() {
                    *result = Some(false);
                    meter_unresolved(&decision.external[index]);
                }
            }
            break;
        };
        let effect = &decision.external[index];
        // Remote fanout is replayable only until its owner supplied a receipt.
        // A same-origin retry must not duplicate an already confirmed fanout.
        let already_receipted = matches!(
            effect,
            ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons { .. })
                | ExternalEffect::Room(crate::server::routes::interpret::effects::room::ExternalRoomEffect::ObserveRoomMessage { .. })
        ) && !decision.external_receipts[index].is_empty()
            && decision.external_receipts[index]
                .iter()
                .all(|key| !decision.receipts_pending.contains(key));
        let outcome = if already_receipted {
            completed[index] = Some(true);
            proven[index] = decision.external_receipts[index].clone();
            ExternalOutcome::Done
        } else if !ready || tokio::time::Instant::now() >= deadline {
            completed[index] = Some(false);
            ExternalOutcome::Failed
        } else {
            match tokio::time::timeout_at(
                deadline,
                async {
                    if let ExternalEffect::Room(room_effect @ crate::server::routes::interpret::effects::room::ExternalRoomEffect::ArchiveAfterPin { .. }) = effect {
                        archive::execute(uow, room_effect).await
                    } else {
                        sink.execute_with_applied(planned[index].clone(), deps, &decision.applied_durable).await
                    }
                },
            )
            .await
            {
                Ok(result) => {
                    completed[index] = Some(dependencies::permits_dependents(effect, &result));
                    proven[index] =
                        proven_receipts(effect, &result, &decision.external_receipts[index]);
                    if let (
                        ExternalEffect::RoomMembershipMutation(mutation),
                        EffectOutcome::Membership(outcome),
                    ) = (effect, &result)
                    {
                        let (room, member) = dependencies::membership_identity(mutation);
                        for dependent in &mut planned {
                            dependent.resolve_membership_outcome(room, member, *outcome);
                        }
                    }
                    #[cfg(feature = "clustering")]
                    let result = match result {
                        EffectOutcome::RelayFrames { frames, completion } => {
                            report.retain_relay_frame_completion(completion);
                            EffectOutcome::Frames(frames)
                        }
                        result => result,
                    };
                    let mut frames = Vec::new();
                    let outcome = classify_outcome(effect, result, &mut frames);
                    if frames.is_empty() {
                        outcome
                    } else {
                        report.frame_obligations.push(FrameObligation {
                            frames,
                            receipt_keys: if outcome == ExternalOutcome::Done {
                                proven[index].clone()
                            } else {
                                Vec::new()
                            },
                            effect_index: index,
                        });
                        if outcome == ExternalOutcome::Done {
                            ExternalOutcome::AwaitingFrameDelivery
                        } else {
                            outcome
                        }
                    }
                }
                Err(_) => {
                    completed[index] = Some(false);
                    ExternalOutcome::Uncertain
                }
            }
        };
        report.outcomes[index].1 = outcome;
        if outcome != ExternalOutcome::Done {
            meter_unresolved(effect);
            continue;
        }
        if decision.external_receipts[index]
            .iter()
            .any(|key| !proven[index].contains(key))
        {
            meter_unresolved(effect);
        }
        let Some(message_key) = decision.message_key else {
            continue;
        };
        for key in completed_receipts(decision, &report.outcomes, &proven, index) {
            if recorded.contains(&key) {
                continue;
            }
            let result = tokio::time::timeout_at(
                deadline,
                EffectReceiptRepository::record_receipt_pooled(
                    db,
                    message_key,
                    key.kind,
                    &key.semantic_identity_hash,
                ),
            )
            .await;
            match result {
                Ok(Ok(())) => recorded.push(key),
                Ok(Err(error)) => {
                    meter_unresolved(effect);
                    report.receipt_failures.push((key, error.into()));
                }
                Err(_) => {
                    // The receipt may have committed. Do not repeat the side effect.
                    meter_unresolved(effect);
                    report
                        .receipt_failures
                        .push((key, ExecutionPersistenceFailure::BudgetExhausted));
                    report.outcomes[index].1 = ExternalOutcome::Uncertain;
                }
            }
        }
    }
    // Compute receipts that become provable only when all prepared frames are written.
    let mut confirmed = report.outcomes.clone();
    for (_, outcome) in &mut confirmed {
        if *outcome == ExternalOutcome::AwaitingFrameDelivery {
            *outcome = ExternalOutcome::Done;
        }
    }
    for obligation in &report.frame_obligations {
        for key in completed_receipts(decision, &confirmed, &proven, obligation.effect_index) {
            if !report.frame_completion_receipts.contains(&key) {
                report.frame_completion_receipts.push(key);
            }
        }
    }
    if !report.frame_obligations.is_empty() {
        return report;
    }
    if let Some(key) = decision.message_key {
        // Tokio's timeout polls its future before checking the deadline. With
        // no budget, do not start a transaction that must immediately cancel.
        if budget.is_zero() {
            report.terminalization_failure = Some(ExecutionPersistenceFailure::BudgetExhausted);
            return report;
        }
        // Terminalization is maintenance, independently bounded from side effects.
        match tokio::time::timeout(budget, terminalize_if_complete(uow, key)).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => report.terminalization_failure = Some(error.into()),
            Err(_) => {
                report.terminalization_failure = Some(ExecutionPersistenceFailure::BudgetExhausted);
                for (effect, _) in &report.outcomes {
                    meter_unresolved(effect);
                }
            }
        }
    }
    report
}

fn completed_receipts(
    decision: &IngressDecision,
    outcomes: &[(ExternalEffect, ExternalOutcome)],
    proven: &[Vec<EffectReceiptKey>],
    index: usize,
) -> Vec<EffectReceiptKey> {
    let Some(keys) = decision.external_receipts.get(index) else {
        return Vec::new();
    };
    keys.iter()
        .filter(|key| {
            decision
                .external_receipts
                .iter()
                .enumerate()
                .all(|(other_index, other_keys)| {
                    !other_keys.contains(key)
                        || outcomes
                            .get(other_index)
                            .is_some_and(|(_, outcome)| *outcome == ExternalOutcome::Done)
                            && proven
                                .get(other_index)
                                .is_some_and(|keys| keys.contains(key))
                })
        })
        .cloned()
        .collect()
}

fn proven_receipts(
    effect: &ExternalEffect,
    outcome: &EffectOutcome,
    candidates: &[EffectReceiptKey],
) -> Vec<EffectReceiptKey> {
    use crate::server::routes::interpret::effects::invite::MucUserDeliveryProof;
    use waddle_xmpp::ingress::{IngressEffectIntent, PendingDeliveryMutation};
    if let EffectOutcome::ConfirmedIntents(intents) = outcome {
        let proven = intents
            .iter()
            .filter_map(|intent| super::durable::receipt_key(intent).ok())
            .collect::<Vec<_>>();
        return candidates
            .iter()
            .filter(|key| proven.contains(key))
            .cloned()
            .collect();
    }
    if let ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate { receipt, .. }) = effect {
        let (
            Some(IngressEffectIntent::RouteDirect { fanout, .. }),
            EffectOutcome::InboxPush(resources),
        ) = (receipt.as_deref(), outcome)
        else {
            return Vec::new();
        };
        return if fanout.iter().all(|resource| resources.contains(resource)) {
            candidates.to_vec()
        } else {
            Vec::new()
        };
    }
    let (ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route)) = effect
    else {
        return candidates.to_vec();
    };
    let EffectOutcome::MucUserDelivery(Ok(proof)) = outcome else {
        return Vec::new();
    };
    let mut intents = Vec::new();
    if let MucUserDeliveryProof::Queued { row_id } = proof {
        intents.push(IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient {
                recipient: route.recipient.clone(),
                row_id: row_id.clone(),
            },
        });
    }
    let route_completed = match proof {
        MucUserDeliveryProof::Queued { .. } => route.resources.is_empty(),
        MucUserDeliveryProof::Delivered { resources } => route
            .resources
            .iter()
            .all(|resource| resources.contains(resource)),
    };
    if route_completed {
        if matches!(proof, MucUserDeliveryProof::Delivered { .. }) {
            intents.push(IngressEffectIntent::PendingDelivery {
                mutation: PendingDeliveryMutation::Transient {
                    recipient: route.fallback.recipient.clone(),
                    row_id: route.fallback.id.clone(),
                },
            });
        }
        if let Some(identity) = &route.route_identity {
            intents.push(IngressEffectIntent::RouteDirect {
                recipient: route.recipient.clone(),
                fanout: route.resources.clone(),
                route_identity: identity.clone(),
            });
        }
    }
    let proven = intents
        .iter()
        .filter_map(|intent| super::durable::receipt_key(intent).ok())
        .collect::<Vec<_>>();
    candidates
        .iter()
        .filter(|key| proven.contains(key))
        .cloned()
        .collect()
}

fn classify_outcome(
    effect: &ExternalEffect,
    outcome: EffectOutcome,
    frames: &mut Vec<Stanza>,
) -> ExternalOutcome {
    match outcome {
        #[cfg(feature = "clustering")]
        EffectOutcome::RelayFrames { .. } => ExternalOutcome::Failed,
        EffectOutcome::Frames(mut produced) => {
            if matches!(effect, ExternalEffect::Room(crate::server::routes::interpret::effects::room::ExternalRoomEffect::ObserveRoomMessage { .. })) && !produced.is_empty() {
                frames.append(&mut produced);
                return ExternalOutcome::Failed;
            }
            frames.append(&mut produced);
            if matches!(
                effect,
                ExternalEffect::RoomMembershipMutation(_)
                    | ExternalEffect::DmPinMutation(_)
                    | ExternalEffect::InviteLedger(_)
            ) {
                ExternalOutcome::Failed
            } else {
                ExternalOutcome::Done
            }
        }
        EffectOutcome::Membership(_)
        | EffectOutcome::InboxPush(_)
        | EffectOutcome::ConfirmedIntents(_) => ExternalOutcome::Done,
        EffectOutcome::MucUserDelivery(Ok(proof)) => {
            use crate::server::routes::interpret::effects::invite::MucUserDeliveryProof;
            match (effect, proof) {
                (
                    ExternalEffect::RouteToPeer(route)
                    | ExternalEffect::QueueOfflineDelivery(route),
                    MucUserDeliveryProof::Delivered { resources },
                ) if route
                    .resources
                    .iter()
                    .any(|resource| !resources.contains(resource)) =>
                {
                    ExternalOutcome::Uncertain
                }
                _ => ExternalOutcome::Done,
            }
        }
        EffectOutcome::InviteLedger(Ok(outcome)) => {
            use crate::server::routes::websocket::{
                handlers::message::muc_invite::InviteLedgerOutcome, muc_invites::RecordOutcome,
            };
            match outcome {
                InviteLedgerOutcome::Recorded(RecordOutcome::New { .. })
                | InviteLedgerOutcome::Claimed(true) => ExternalOutcome::Done,
                InviteLedgerOutcome::Recorded(RecordOutcome::AlreadyOutstanding)
                | InviteLedgerOutcome::Claimed(false) => ExternalOutcome::Uncertain,
            }
        }
        EffectOutcome::PlannedInbox(_)
        | EffectOutcome::MucUserDelivery(Err(_))
        | EffectOutcome::InviteLedger(Err(_)) => ExternalOutcome::Failed,
        EffectOutcome::Completed if !has_confirmed_completion(effect) => ExternalOutcome::Uncertain,
        EffectOutcome::Completed | EffectOutcome::Archive(Ok(_)) | EffectOutcome::Inbox(Ok(_)) => {
            ExternalOutcome::Done
        }
        EffectOutcome::Unavailable
        | EffectOutcome::Archive(Err(_))
        | EffectOutcome::Inbox(Err(_)) => ExternalOutcome::Failed,
        EffectOutcome::Delivery(outcome) => match outcome {
            FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                if matches!(effect, ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. }) if resources.len() > 1)
                {
                    // The existing sink reports success if any resource was queued;
                    // it does not prove the whole multi-resource obligation.
                    ExternalOutcome::Uncertain
                } else {
                    ExternalOutcome::Done
                }
            }
            FullJidDeliveryOutcome::Unavailable => ExternalOutcome::Failed,
            FullJidDeliveryOutcome::Dropped => ExternalOutcome::Uncertain,
            #[cfg(feature = "clustering")]
            FullJidDeliveryOutcome::MaybeCommitted => ExternalOutcome::Uncertain,
        },
    }
}

fn has_confirmed_completion(effect: &ExternalEffect) -> bool {
    // These legacy helpers discard individual delivery/storage failures. A void
    // return proves only that an attempt finished, never a durable receipt.
    !matches!(
        effect,
        ExternalEffect::Delivery(
            ExternalDeliveryEffect::Carbons { .. }
                | ExternalDeliveryEffect::QueueOfflineDelivery { .. }
        ) | ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate { .. })
    )
}

fn meter_unresolved(effect: &ExternalEffect) {
    use waddle_xmpp::telemetry::attributes::IngressUnresolvedEffectKind;
    let kind = match effect {
        ExternalEffect::Frame(_) => IngressUnresolvedEffectKind::Frame,
        ExternalEffect::Direct(_) => IngressUnresolvedEffectKind::Direct,
        ExternalEffect::Room(_)
        | ExternalEffect::RoomMembershipMutation(_)
        | ExternalEffect::InviteLedger(_) => IngressUnresolvedEffectKind::Room,
        ExternalEffect::DmPinMutation(_) => IngressUnresolvedEffectKind::Direct,
        ExternalEffect::RouteToPeer(_) | ExternalEffect::QueueOfflineDelivery(_) => {
            IngressUnresolvedEffectKind::Delivery
        }
        ExternalEffect::Delivery(_) => IngressUnresolvedEffectKind::Delivery,
    };
    waddle_xmpp::telemetry::reliability::increment_ingress_effect_unresolved(kind);
}

/// Returns false while any durable intent remains without a receipt.
pub async fn terminalize_if_complete(
    uow: &IngressUnitOfWork,
    message_key: MessageKey,
) -> Result<bool, IngressUowError> {
    let mut transaction = uow
        .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
        .await?;
    let complete = terminalize_if_complete_in_transaction(&mut transaction, message_key).await?;
    transaction.commit().await?;
    Ok(complete)
}

/// Share the receipt proof with stream retirement without opening a nested transaction.
pub(super) async fn terminalize_if_complete_in_transaction(
    transaction: &mut IngressUowTransaction<'_>,
    message_key: MessageKey,
) -> Result<bool, IngressUowError> {
    if !CanonicalMessageRepository::lock(transaction, message_key).await? {
        return Ok(false);
    }
    if !EffectReceiptRepository::receipts_complete(transaction, message_key).await? {
        waddle_xmpp::telemetry::reliability::increment_ingress_effect_unresolved(
            waddle_xmpp::telemetry::attributes::IngressUnresolvedEffectKind::Terminalization,
        );
        return Ok(false);
    }
    let outcome =
        CanonicalMessageRepository::terminalize(transaction, message_key, chrono::Utc::now())
            .await?;
    Ok(!matches!(
        outcome,
        crate::ingress_substrate::TerminalizeOutcome::MessageVanished
    ))
}

#[cfg(test)]
#[path = "execute_dependency_tests.rs"]
mod dependency_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress::{decision::AliasOutcomeClass, decision::IngressDecisionClass};
    use crate::ingress_substrate::EffectReceiptKind;
    use xmpp_parsers::message::Message;

    fn frame() -> ExternalEffect {
        ExternalEffect::Frame(Box::new(Stanza::Message(Message::new(None))))
    }

    #[test]
    fn external_outcome_preserves_delivery_uncertainty() {
        let cases = [
            (EffectOutcome::Completed, ExternalOutcome::Done),
            (EffectOutcome::Unavailable, ExternalOutcome::Failed),
            (
                EffectOutcome::Delivery(FullJidDeliveryOutcome::Delivered),
                ExternalOutcome::Done,
            ),
            (
                EffectOutcome::Delivery(FullJidDeliveryOutcome::QueuedDetached),
                ExternalOutcome::Done,
            ),
            (
                EffectOutcome::Delivery(FullJidDeliveryOutcome::Unavailable),
                ExternalOutcome::Failed,
            ),
            (
                EffectOutcome::Delivery(FullJidDeliveryOutcome::Dropped),
                ExternalOutcome::Uncertain,
            ),
        ];
        for (result, expected) in cases {
            assert_eq!(
                classify_outcome(&frame(), result, &mut Vec::new()),
                expected
            );
        }
        #[cfg(feature = "clustering")]
        assert_eq!(
            classify_outcome(
                &frame(),
                EffectOutcome::Delivery(FullJidDeliveryOutcome::MaybeCommitted),
                &mut Vec::new()
            ),
            ExternalOutcome::Uncertain
        );
    }

    #[test]
    fn external_receipt_requires_every_mapped_effect_to_finish() {
        let key = EffectReceiptKey {
            kind: EffectReceiptKind::from_storage(1),
            semantic_identity_hash: [1; 32],
        };
        let decision = IngressDecision {
            class: IngressDecisionClass::Accepted,
            message_key: None,
            ordinal: None,
            alias: AliasOutcomeClass::NoOrigin,
            verdict: None,
            archive_ids: vec![],
            applied_durable: Default::default(),
            external_dependencies: vec![vec![], vec![]],
            external: vec![frame(), frame()],
            external_receipts: vec![vec![key.clone()], vec![key.clone()]],
            receipts_pending: vec![key.clone()],
        };
        let mut outcomes = vec![(frame(), ExternalOutcome::Done)];
        assert!(
            completed_receipts(&decision, &outcomes, &decision.external_receipts, 0).is_empty()
        );
        outcomes.push((frame(), ExternalOutcome::Failed));
        assert!(
            completed_receipts(&decision, &outcomes, &decision.external_receipts, 1).is_empty()
        );
        outcomes[1].1 = ExternalOutcome::Done;
        assert_eq!(
            completed_receipts(&decision, &outcomes, &decision.external_receipts, 1),
            vec![key]
        );
    }

    #[test]
    fn frames_are_collected_without_changing_authority() {
        let mut frames = vec![];
        let stanza = Stanza::Message(Message::new(None));
        assert_eq!(
            classify_outcome(&frame(), EffectOutcome::Frames(vec![stanza]), &mut frames),
            ExternalOutcome::Done
        );
        assert_eq!(frames.len(), 1);
    }
    #[test]
    fn partial_detached_success_is_not_a_complete_receipt() {
        let effect = ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            call_setup: None,
            bare: "peer@example.com".parse().expect("bare"),
            resources: vec![
                "peer@example.com/one".parse().expect("first"),
                "peer@example.com/two".parse().expect("second"),
            ],
            stanza: Box::new(Stanza::Message(Message::new(None))),
        });
        assert_eq!(
            classify_outcome(
                &effect,
                EffectOutcome::Delivery(FullJidDeliveryOutcome::QueuedDetached),
                &mut Vec::new()
            ),
            ExternalOutcome::Uncertain
        );
    }

    #[test]
    fn void_push_completion_does_not_prove_delivery() {
        let owner = "peer@example.com".parse().expect("owner");
        let effect = ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
            owner,
            projection: crate::server::routes::interpret::effects::ProjectionRef(0),
            receipt: None,
        });
        assert_eq!(
            classify_outcome(&effect, EffectOutcome::Completed, &mut Vec::new()),
            ExternalOutcome::Uncertain
        );
    }
}

#[cfg(test)]
#[path = "execute_delivery_tests.rs"]
mod delivery_tests;

#[cfg(test)]
#[path = "execute_carbons_retry_tests.rs"]
mod carbons_retry_tests;

#[cfg(all(test, feature = "clustering"))]
#[path = "execute_carbon_fanout_tests.rs"]
mod carbon_fanout_tests;

#[cfg(test)]
#[path = "execute_observer_tests.rs"]
mod observer_tests;

#[cfg(test)]
#[path = "execute_inbox_offline_tests.rs"]
mod inbox_offline_tests;
