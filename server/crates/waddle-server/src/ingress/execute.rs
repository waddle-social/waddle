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
            ExternalEffect, ImmediateSink, PlannedEffect, SettledCompletion,
        },
        Deps, FullJidDeliveryOutcome, SmIngressAppendContext,
    },
};

use super::decision::{EffectReceiptKey, IngressDecision};

#[path = "execute_dependencies.rs"]
mod dependencies;

#[path = "execute_carbon_progress.rs"]
mod carbon_progress;

#[path = "execute_observers.rs"]
mod observers;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalOutcome {
    Done,
    Failed,
    Uncertain,
    /// Frames are prepared but their transport write and delivery receipts are not confirmed.
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
    receipts: Vec<waddle_xmpp::stream_management::SmIngressFrameReceipt>,
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
            receipts: completion.report.frame_receipts(),
            inner: std::sync::Arc::new(tokio::sync::Mutex::new(RelayFrameReceiptTarget::Local(
                completion,
            ))),
        }
    }

    pub(crate) fn remote(
        owner: crate::clustering::NodeId,
        token: crate::clustering::relay::RelayReplyReceiptToken,
        receipts: Vec<waddle_xmpp::stream_management::SmIngressFrameReceipt>,
        stop_token: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            receipts,
            inner: std::sync::Arc::new(tokio::sync::Mutex::new(RelayFrameReceiptTarget::Remote {
                owner,
                token,
                stop_token,
            })),
        }
    }

    pub(crate) fn frame_receipts(
        &self,
    ) -> Vec<waddle_xmpp::stream_management::SmIngressFrameReceipt> {
        self.receipts.clone()
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
    pub(super) effect_index: usize,
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

impl Drop for ExecutionReport {
    fn drop(&mut self) {
        // Awaiting a write is healthy. Only abandoned completion obligations
        // represent unresolved effects (including cancellation and disconnect).
        for (effect, outcome) in &self.outcomes {
            if *outcome == ExternalOutcome::AwaitingFrameDelivery {
                meter_unresolved(effect);
            }
        }
    }
}

impl ExecutionReport {
    /// The durable receipt identities this batch's frames discharge. The
    /// websocket writer carries them with the XEP-0198 replay entry so a
    /// transport failure can still receipt them once the retained frames are
    /// written on resume.
    pub(crate) fn frame_receipts(
        &self,
    ) -> Vec<waddle_xmpp::stream_management::SmIngressFrameReceipt> {
        use waddle_xmpp::stream_management::{SmIngressFrameReceipt, SmIngressReceiptKind};
        // Owner receipts precede origin dispatch receipts. Canonical identities
        // are cluster-global; resume uses the same authority confirmation path
        // even after the original owner's process and reply token have expired.
        let mut receipts = Vec::new();
        #[cfg(feature = "clustering")]
        for completion in &self.relay_frame_completions {
            receipts.extend(completion.frame_receipts());
        }
        if let Some(message_key) = self.message_key {
            receipts.extend(self.frame_completion_receipts.iter().map(|key| {
                SmIngressFrameReceipt {
                    message_key,
                    kind: SmIngressReceiptKind::from_storage(key.kind.to_storage()),
                    semantic_identity_hash: key.semantic_identity_hash,
                }
            }));
        }
        receipts
    }

    /// Rebuild receipt-only completions, one per canonical message, after a
    /// retained replay prefix reached the wire. These carry no obligations and
    /// re-execute nothing: they only record receipts and terminalize.
    pub(crate) fn replay_frame_completions(
        receipts: &[waddle_xmpp::stream_management::SmIngressFrameReceipt],
    ) -> Vec<Self> {
        let mut reports: Vec<Self> = Vec::new();
        for receipt in receipts {
            let key = EffectReceiptKey {
                kind: crate::ingress_substrate::EffectReceiptKind::from_storage(
                    receipt.kind.to_storage(),
                ),
                semantic_identity_hash: receipt.semantic_identity_hash,
            };
            match reports
                .iter_mut()
                .find(|report| report.message_key == Some(receipt.message_key))
            {
                Some(report) => report.frame_completion_receipts.push(key),
                None => reports.push(Self {
                    outcomes: Vec::new(),
                    frame_obligations: Vec::new(),
                    #[cfg(feature = "clustering")]
                    relay_frame_completions: Vec::new(),
                    message_key: Some(receipt.message_key),
                    frame_completion_receipts: vec![key],
                    receipt_failures: Vec::new(),
                    terminalization_failure: None,
                }),
            }
        }
        reports
    }

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
        let started = tokio::time::Instant::now();
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
            Ok::<(), IngressUowError>(())
        })
        .await
        .map_err(|_| ExecutionPersistenceFailure::BudgetExhausted)?
        .map_err(ExecutionPersistenceFailure::from)?;
        // The frames are written and their receipts are durable: these
        // obligations are resolved regardless of how terminalization below
        // fares, so dropping the report must not meter them as unresolved.
        for obligation in &self.frame_obligations {
            if self.outcomes[obligation.effect_index].1 == ExternalOutcome::AwaitingFrameDelivery {
                self.outcomes[obligation.effect_index].1 = ExternalOutcome::Done;
            }
        }
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(ExecutionPersistenceFailure::BudgetExhausted);
        }
        tokio::time::timeout(remaining, terminalize_if_complete(uow, message_key))
            .await
            .map_err(|_| ExecutionPersistenceFailure::BudgetExhausted)?
            .map_err(ExecutionPersistenceFailure::from)
    }
}

/// A later confirmed snapshot of the same call subsumes every earlier transition.
/// Receipt repair must not reinstall an intermediate anchor/projection snapshot.
fn call_state_superseded(decision: &IngressDecision, effect: &ExternalEffect) -> bool {
    use waddle_xmpp::ingress::IngressEffectIntent;

    let ExternalEffect::Direct(ExternalDirectEffect::DmCallThreadState {
        state,
        receipt: Some(receipt),
    }) = effect
    else {
        return false;
    };
    let IngressEffectIntent::DmCallThreadState { sequence, .. } = receipt.as_ref() else {
        return false;
    };
    decision.external.iter().enumerate().any(|(index, later)| {
        let ExternalEffect::Direct(ExternalDirectEffect::DmCallThreadState {
            state: later_state,
            receipt: Some(later_receipt),
        }) = later
        else {
            return false;
        };
        matches!(later_receipt.as_ref(),
            IngressEffectIntent::DmCallThreadState { sequence: later_sequence, .. }
                if later_state.key == state.key && later_sequence > sequence)
            && !decision.external_receipts[index].is_empty()
            && decision.external_receipts[index]
                .iter()
                .all(|key| !decision.receipts_pending.contains(key))
    })
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
    let mut discharged_invite_deliveries = vec![false; decision.external.len()];
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
        if ready && observers::is_observer(effect) {
            observers::execute_ready(
                observers::Batch {
                    decision,
                    planned: &planned,
                    report: &mut report,
                    completed: &mut completed,
                    proven: &mut proven,
                    recorded: &mut recorded,
                },
                db,
                sink,
                deps,
                deadline,
            )
            .await;
            continue;
        }
        // Confirmed fanout and state mutations must not execute again.
        // Replaying historical activity could overwrite newer state.
        let already_receipted = matches!(
            effect,
            ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons { .. } | ExternalDeliveryEffect::Carbons { .. })
                | ExternalEffect::Room(crate::server::routes::interpret::effects::room::ExternalRoomEffect::ObserveRoomMessage { .. })
                | ExternalEffect::Direct(ExternalDirectEffect::DmCallThreadState { .. } | ExternalDirectEffect::NotificationActivity { .. })
        ) && !decision.external_receipts[index].is_empty()
            && decision.external_receipts[index]
                .iter()
                .all(|key| !decision.receipts_pending.contains(key));
        let mut settled_complete = false;
        let outcome = if discharged_invite_deliveries[index] {
            // The ledger confirmed an outstanding invitation or a losing claim.
            // Its mutually exclusive live and offline obligations are no-ops.
            completed[index] = Some(false);
            proven[index] = decision.external_receipts[index].clone();
            ExternalOutcome::Done
        } else if already_receipted || call_state_superseded(decision, effect) {
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
                    if let Some(result) = super::execute_uow::execute_with_uow(uow, db, decision, index, effect, deps, deadline).await {
                        result
                    } else {
                        let mut execution = planned[index].clone();
                        if let Some(message) = decision.message_key {
                            match carbon_progress::prepare(uow, message, effect).await {
                                Ok(prepared) => execution.effect = Effect::External(prepared),
                                Err(error) => {
                                    tracing::warn!(%error, "remote carbon progress unavailable; leaving obligation pending");
                                    return EffectOutcome::Unavailable;
                                }
                            }
                        }
                        // A remote plan may become local before execution. Carry
                        // only its exact recorded direct receipt into that fallback;
                        // unrelated effects (including MUC) get no append context.
                        let mut effect_deps = deps.clone();
                        effect_deps.ingress_append_context = None;
                        if let ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { target, .. }) = effect {
                            if let Some(message_key) = decision.message_key {
                                if let Some(progress) = decision.route_progress.iter().find(|progress| {
                                    progress.matches(effect)
                                        && progress.fanout.contains(target)
                                        && decision.external_receipts[index].contains(&progress.receipt)
                                }) {
                                    effect_deps.ingress_append_context = Some(SmIngressAppendContext {
                                        message_key,
                                        receipt: progress.receipt.clone(),
                                    });
                                }
                            }
                        }
                        let result = sink.execute_with_applied(execution, &effect_deps, &decision.applied_durable).await;
                        if let Some(message) = decision.message_key {
                            if let Err(error) = carbon_progress::persist(uow, message, effect, &result).await {
                                tracing::warn!(%error, "remote carbon progress persistence failed; leaving obligation pending");
                                return EffectOutcome::Unavailable;
                            }
                        }
                        result
                    }
                },
            )
            .await
            {
                Ok(result) => {
                    // Completion attests that this effect's work committed, even
                    // when another effect still owns unfinished aggregate work.
                    // This affects diagnostics only; persisted values remain
                    // the sole receipt proof.
                    settled_complete = matches!(
                        &result,
                        EffectOutcome::Settled(settled)
                            if settled.completion == SettledCompletion::Complete
                    );
                    let ledger_noop = invite_ledger_noop(&result);
                    // A recorded authority can have written the ledger row and
                    // then lost its connection before the invitation was sent.
                    // The row proves only itself, so a replay of that authority
                    // must still run its pending delivery instead of
                    // discharging it as an outstanding duplicate.
                    let recorded_ledger_repair = matches!(&result, EffectOutcome::InviteLedger(Ok(
                        crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerOutcome::Recorded(
                            crate::server::routes::websocket::muc_invites::RecordOutcome::AlreadyOutstanding
                        )
                    )))
                        && decision.alias == super::decision::AliasOutcomeClass::Existing;
                    if ledger_noop && !recorded_ledger_repair {
                        for (dependent_index, dependent) in planned.iter().enumerate() {
                            discharged_invite_deliveries[dependent_index] |=
                                is_invite_delivery_dependent(effect, dependent);
                        }
                    }
                    completed[index] = Some(
                        recorded_ledger_repair
                            || dependencies::permits_dependents(effect, &result),
                    );
                    proven[index] =
                        proven_receipts(effect, &result, &decision.external_receipts[index]);
                    // These receipts committed with the arm's work, including
                    // any partial progress in an incomplete/uncertain outcome.
                    if matches!(&result, EffectOutcome::Settled(_)) {
                        for key in &proven[index] {
                            if !recorded.contains(key) {
                                recorded.push(key.clone());
                            }
                        }
                    }
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
        if outcome == ExternalOutcome::AwaitingFrameDelivery {
            continue;
        }
        if outcome != ExternalOutcome::Done {
            meter_unresolved(effect);
            continue;
        }
        if !settled_complete
            && decision.external_receipts[index]
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
            #[cfg(test)]
            if test_hooks::take_receipt_failure(message_key, &key) {
                meter_unresolved(effect);
                report
                    .receipt_failures
                    .push((key, IngressUowError::Timeout.into()));
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
        #[cfg(test)]
        test_hooks::before_terminalization(key).await;
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
        .filter(|key| !decision.arm_owned_receipts.contains(key))
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
    if let ExternalEffect::Room(
        crate::server::routes::interpret::effects::room::ExternalRoomEffect::RoomActorMutation {
            mutation:
                crate::server::routes::interpret::effects::room::RoomActorMutation::SetSubject {
                    rejection_reply,
                    ..
                },
            ..
        },
    ) = effect
    {
        let bounce = subject_bounce_receipt(rejection_reply);
        // A successful mutation discharges its mutually exclusive bounce.
        // Failure proves only the bounce, and only after its frame is written.
        return candidates
            .iter()
            .filter(|key| match outcome {
                EffectOutcome::Completed => true,
                EffectOutcome::Unavailable => Some(*key) == bounce.as_ref(),
                _ => false,
            })
            .cloned()
            .collect();
    }
    if let EffectOutcome::ConfirmedIntents(intents)
    | EffectOutcome::Settled(crate::server::routes::interpret::effects::SettledOutcome {
        persisted: intents,
        ..
    }) = outcome
    {
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
        MucUserDeliveryProof::Queued { row_id } => row_id == &route.fallback.id,
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

fn subject_bounce_receipt(message: &xmpp_parsers::message::Message) -> Option<EffectReceiptKey> {
    let recipient = message.to.as_ref()?.try_as_full().ok()?.clone();
    let error = message.payloads.iter().find_map(|payload| {
        xmpp_parsers::stanza_error::StanzaError::try_from(payload.clone()).ok()
    })?;
    let error = waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&error).ok()?;
    super::durable::receipt_key(&waddle_xmpp::ingress::IngressEffectIntent::ErrorReply {
        recipient,
        error,
    })
    .ok()
}

fn classify_outcome(
    effect: &ExternalEffect,
    outcome: EffectOutcome,
    frames: &mut Vec<Stanza>,
) -> ExternalOutcome {
    match outcome {
        EffectOutcome::Settled(settled) => {
            if let Some(detached) = settled.detached {
                for (resource, outcome) in detached {
                    tracing::debug!(%resource, ?outcome, "transactional detached delivery completed");
                }
            }
            match settled.completion {
                SettledCompletion::Complete => ExternalOutcome::Done,
                SettledCompletion::Incomplete => ExternalOutcome::Failed,
                SettledCompletion::Uncertain => ExternalOutcome::Uncertain,
            }
        }
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
                InviteLedgerOutcome::Recorded(
                    RecordOutcome::New { .. } | RecordOutcome::AlreadyOutstanding,
                )
                | InviteLedgerOutcome::Claimed(_) => ExternalOutcome::Done,
            }
        }
        EffectOutcome::PlannedInbox(_)
        | EffectOutcome::MucUserDelivery(Err(_))
        | EffectOutcome::InviteLedger(Err(_)) => ExternalOutcome::Failed,
        EffectOutcome::Completed if !has_confirmed_completion(effect) => ExternalOutcome::Uncertain,
        EffectOutcome::Completed | EffectOutcome::Archive(Ok(_)) | EffectOutcome::Inbox(Ok(_)) => {
            ExternalOutcome::Done
        }
        EffectOutcome::Unavailable => {
            if let ExternalEffect::Room(crate::server::routes::interpret::effects::room::ExternalRoomEffect::RoomActorMutation {
                mutation: crate::server::routes::interpret::effects::room::RoomActorMutation::SetSubject { rejection_reply, .. }, ..
            }) = effect {
                frames.push(Stanza::Message((**rejection_reply).clone()));
                return ExternalOutcome::Done;
            }
            ExternalOutcome::Failed
        }
        EffectOutcome::Archive(Err(_))
        | EffectOutcome::Inbox(Err(_))
        | EffectOutcome::OfflineDeliveryQuotaExceeded => ExternalOutcome::Failed,
        EffectOutcome::Delivery(outcome) | EffectOutcome::CarbonFanout { outcome, .. } => {
            match outcome {
                FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                    ExternalOutcome::Done
                }
                FullJidDeliveryOutcome::Unavailable => ExternalOutcome::Failed,
                FullJidDeliveryOutcome::Dropped => ExternalOutcome::Uncertain,
                #[cfg(feature = "clustering")]
                FullJidDeliveryOutcome::MaybeCommitted => ExternalOutcome::Uncertain,
            }
        }
    }
}

fn invite_ledger_noop(outcome: &EffectOutcome) -> bool {
    use crate::server::routes::websocket::{
        handlers::message::muc_invite::InviteLedgerOutcome, muc_invites::RecordOutcome,
    };
    matches!(
        outcome,
        EffectOutcome::InviteLedger(Ok(InviteLedgerOutcome::Recorded(
            RecordOutcome::AlreadyOutstanding
        ) | InviteLedgerOutcome::Claimed(false)))
    )
}

fn is_invite_delivery_dependent(ledger: &ExternalEffect, planned: &PlannedEffect) -> bool {
    matches!(ledger, ExternalEffect::InviteLedger(_))
        && matches!(
            &planned.effect,
            Effect::External(ExternalEffect::RouteToPeer(_) | ExternalEffect::QueueOfflineDelivery(_))
        )
        && planned.dependencies.iter().any(|dependency| {
            matches!(
                dependency,
                crate::server::routes::interpret::effects::PlanEffectDependency::AfterInviteLedger { .. }
            ) && dependencies::produces(ledger, dependency)
        })
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
    Ok(matches!(
        terminalize_if_complete_outcome(uow, message_key).await?,
        Some(
            crate::ingress_substrate::TerminalizeOutcome::Terminalized
                | crate::ingress_substrate::TerminalizeOutcome::AlreadyTerminal
        )
    ))
}

pub(crate) async fn terminalize_if_complete_outcome(
    uow: &IngressUnitOfWork,
    message_key: MessageKey,
) -> Result<Option<crate::ingress_substrate::TerminalizeOutcome>, IngressUowError> {
    #[cfg(test)]
    if test_hooks::take_terminalization_timeout(message_key) {
        return Err(IngressUowError::Timeout);
    }
    let mut transaction = uow
        .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
        .await?;
    let outcome =
        terminalize_if_complete_outcome_in_transaction(&mut transaction, message_key).await?;
    transaction.commit().await?;
    Ok(outcome)
}

/// Share the receipt proof with stream retirement without opening a nested transaction.
pub(super) async fn terminalize_if_complete_in_transaction(
    transaction: &mut IngressUowTransaction<'_>,
    message_key: MessageKey,
) -> Result<bool, IngressUowError> {
    Ok(matches!(
        terminalize_if_complete_outcome_in_transaction(transaction, message_key).await?,
        Some(
            crate::ingress_substrate::TerminalizeOutcome::Terminalized
                | crate::ingress_substrate::TerminalizeOutcome::AlreadyTerminal
        )
    ))
}

async fn terminalize_if_complete_outcome_in_transaction(
    transaction: &mut IngressUowTransaction<'_>,
    message_key: MessageKey,
) -> Result<Option<crate::ingress_substrate::TerminalizeOutcome>, IngressUowError> {
    if !CanonicalMessageRepository::lock(transaction, message_key).await? {
        return Ok(Some(
            crate::ingress_substrate::TerminalizeOutcome::MessageVanished,
        ));
    }
    if !EffectReceiptRepository::receipts_complete(transaction, message_key).await? {
        waddle_xmpp::telemetry::reliability::increment_ingress_effect_unresolved(
            waddle_xmpp::telemetry::attributes::IngressUnresolvedEffectKind::Terminalization,
        );
        return Ok(None);
    }
    let outcome =
        CanonicalMessageRepository::terminalize(transaction, message_key, chrono::Utc::now())
            .await?;
    Ok(Some(outcome))
}

#[cfg(test)]
#[path = "execute_dependency_tests.rs"]
mod dependency_tests;

#[cfg(test)]
#[path = "execute_test_hooks.rs"]
pub(crate) mod test_hooks;

#[cfg(test)]
#[path = "execute_settlement_tests.rs"]
mod settlement_tests;

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
            arm_owned_receipts: Vec::new(),
            route_progress: Vec::new(),
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
    fn detached_batch_completion_preserves_incomplete_outcomes() {
        let effect = ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            route_identity: None,
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
            ExternalOutcome::Done
        );
        assert_eq!(
            classify_outcome(
                &effect,
                EffectOutcome::Delivery(FullJidDeliveryOutcome::Dropped),
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

#[cfg(test)]
#[path = "execute_direct_receipt_tests.rs"]
mod direct_receipt_tests;

#[cfg(test)]
#[path = "execute_local_carbons_tests.rs"]
mod local_carbons_tests;

#[cfg(test)]
#[path = "execute_frame_completion_tests.rs"]
mod frame_completion_tests;

#[cfg(test)]
#[path = "execute_invite_noop_tests.rs"]
mod invite_noop_tests;

#[cfg(test)]
#[path = "execute_detached_receipt_tests.rs"]
mod detached_receipt_tests;

#[cfg(test)]
#[path = "execute_dm_call_tests.rs"]
mod dm_call_tests;

#[cfg(test)]
#[path = "execute_activity_tests.rs"]
mod activity_tests;

#[cfg(all(test, feature = "clustering"))]
#[path = "execute_groupchat_receipt_tests.rs"]
mod groupchat_receipt_tests;

#[cfg(test)]
#[path = "execute_detached_fault_tests.rs"]
mod detached_fault_tests;

#[cfg(test)]
#[path = "execute_relay_detached_tests.rs"]
mod relay_detached_tests;
