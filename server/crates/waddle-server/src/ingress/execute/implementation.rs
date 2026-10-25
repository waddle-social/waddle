//! Implementation behind the parent module facade.
#[cfg(test)]
use super::test_hooks;

use super::carbon_progress;
use super::dependencies;
use super::dispatch_rounds;
use super::muc_fanout;
use super::observers;
use super::AppendAuthority;
use super::Database;
use super::DispatchReadiness;
use super::Duration;
use super::EffectReceiptKind;
use super::ExternalDeliveryEffect;
use super::ExternalDirectEffect;
use super::IngressEffectExecutionPhase;
use super::InviteLedgerError;
use super::MessageKey;
#[cfg(feature = "clustering")]
use super::NodeId;
use super::PlanEffectDependency;
#[cfg(feature = "clustering")]
use super::RelayAskError;
#[cfg(feature = "clustering")]
use super::RelayFrameCompletion;
#[cfg(feature = "clustering")]
use super::RelayHandle;
#[cfg(feature = "clustering")]
use super::RelayReplyReceiptToken;
use super::SettledRefusal;
use super::Stanza;
use super::TerminalizeOutcome;
use super::{
    CanonicalMessageRepository, EffectReceiptRepository, IngressUnitOfWork, IngressUowError,
    IngressUowTransaction,
};
use super::{Deps, FullJidDeliveryOutcome, SmIngressAppendContext};
use super::{
    Effect, EffectOutcome, ExternalEffect, ImmediateSink, PlannedEffect, SettledCompletion,
    SettledOutcome,
};
use super::{EffectReceiptKey, IngressDecision};

pub(in crate::ingress) fn supplies_dependency(
    effect: &ExternalEffect,
    dependency: &PlanEffectDependency,
) -> bool {
    dependencies::produces(effect, dependency)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalOutcome {
    Done,
    Failed,
    Uncertain,
    /// Frames are prepared but their transport write and delivery receipts are not confirmed.
    AwaitingFrameDelivery,
    AwaitingPredecessor,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionPersistenceFailure {
    #[error("post-commit persistence failed: {0}")]
    Storage(#[from] IngressUowError),
    #[cfg(feature = "clustering")]
    #[error("relay frame receipt confirmation failed: {0}")]
    RelayConfirmation(#[from] RelayAskError),
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
    pub(in crate::ingress::execute) receipts:
        Vec<waddle_xmpp::stream_management::SmIngressFrameReceipt>,
    pub(in crate::ingress::execute) inner:
        std::sync::Arc<tokio::sync::Mutex<RelayFrameReceiptTarget>>,
}

#[cfg(feature = "clustering")]
pub(in crate::ingress::execute) enum RelayFrameReceiptTarget {
    Local(RelayFrameCompletion),
    Remote {
        owner: NodeId,
        token: RelayReplyReceiptToken,
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
    pub(crate) fn new(completion: RelayFrameCompletion) -> Self {
        Self {
            receipts: completion.report.frame_receipts(),
            inner: std::sync::Arc::new(tokio::sync::Mutex::new(RelayFrameReceiptTarget::Local(
                completion,
            ))),
        }
    }

    pub(crate) fn remote(
        owner: NodeId,
        token: RelayReplyReceiptToken,
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
                let mut handle = RelayHandle::new(owner.clone(), stop_token.clone());
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
    pub(in crate::ingress) effect_index: usize,
}

#[derive(Debug)]
pub struct ExecutionReport {
    pub(in crate::ingress::execute) phase: IngressEffectExecutionPhase,
    pub refusal: Option<SettledRefusal>,
    pub outcomes: Vec<(ExternalEffect, ExternalOutcome)>,
    pub frame_obligations: Vec<FrameObligation>,
    #[cfg(feature = "clustering")]
    pub(in crate::ingress::execute) relay_frame_completions: Vec<RelayFrameReceiptCompletion>,
    pub(in crate::ingress::execute) message_key: Option<MessageKey>,
    pub(in crate::ingress::execute) frame_completion_receipts: Vec<EffectReceiptKey>,
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
                meter_unresolved(effect, self.phase);
            }
        }
    }
}

impl ExecutionReport {
    pub(crate) fn new(phase: IngressEffectExecutionPhase) -> Self {
        Self {
            phase,
            refusal: None,
            outcomes: Vec::new(),
            frame_obligations: Vec::new(),
            #[cfg(feature = "clustering")]
            relay_frame_completions: Vec::new(),
            message_key: None,
            frame_completion_receipts: Vec::new(),
            receipt_failures: Vec::new(),
            terminalization_failure: None,
        }
    }

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
                kind: EffectReceiptKind::from_storage(receipt.kind.to_storage()),
                semantic_identity_hash: receipt.semantic_identity_hash,
            };
            match reports
                .iter_mut()
                .find(|report| report.message_key == Some(receipt.message_key))
            {
                Some(report) => report.frame_completion_receipts.push(key),
                None => reports.push(Self {
                    phase: IngressEffectExecutionPhase::Live,
                    refusal: None,
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
    pub(in crate::ingress) async fn complete_relay_frame_obligations(
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
        tokio::time::timeout(
            remaining,
            terminalize_if_complete(uow, message_key, self.phase),
        )
        .await
        .map_err(|_| ExecutionPersistenceFailure::BudgetExhausted)?
        .map_err(ExecutionPersistenceFailure::from)
    }
}

/// A later confirmed snapshot of the same call subsumes every earlier transition.
/// Receipt repair must not reinstall an intermediate anchor/projection snapshot.
pub(in crate::ingress::execute) fn call_state_superseded(
    decision: &IngressDecision,
    effect: &ExternalEffect,
) -> bool {
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
    let mut scoped_deps = deps.clone();
    scoped_deps.ingress_delivery_uow = Some(uow.clone());
    if scoped_deps.ingress_delivery_stop.is_none() {
        scoped_deps.ingress_delivery_stop = scoped_deps
            .web_socket_state
            .map(|state| state.deps.protocol.ingress.delivery_stop_token());
    }
    let probe_budget = scoped_deps
        .dispatch_probe_budget
        .get_or_insert_with(Default::default)
        .clone();
    let deps = &scoped_deps;
    let mut report = ExecutionReport::new(deps.delivery_execution_context.into());
    if !decision.class.advances() {
        return report;
    }
    report.message_key = decision.message_key;
    let deadline = tokio::time::Instant::now() + budget;
    let mut recorded = Vec::new();
    let mut proven = vec![Vec::new(); decision.external.len()];
    let mut completed = vec![None; decision.external.len()];
    let mut recheck = vec![false; decision.external.len()];
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
    loop {
        if completed.iter().all(Option::is_some)
            && !dispatch_rounds::resume_deferred(
                &report.outcomes,
                &recheck,
                &mut completed,
                &probe_budget,
                deadline,
            )
            .await
        {
            break;
        }
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
                    meter_unresolved(&decision.external[index], report.phase);
                }
            }
            break;
        };
        // Recipient snapshots are unordered. A stalled remote occupant must
        // not consume the whole budget before ready local copies of the same
        // broadcast can record progress. Failed dependency results retain
        // their original order, and reflection is excluded by RouteProgress.
        let index = if ready {
            muc_fanout::local_before_remote(decision, &planned, &completed, index).unwrap_or(index)
        } else {
            index
        };
        let effect = &decision.external[index];
        recheck[index] = false;
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
        let archived_reflection = !decision.archive_ids.is_empty()
            && decision.external_receipts[index].iter().any(|receipt| {
                receipt.kind.to_storage()
                    == waddle_xmpp::ingress::IngressEffectKind::RouteDirect.storage_tag()
            })
            && match effect {
                ExternalEffect::Frame(stanza)
                | ExternalEffect::Delivery(
                    ExternalDeliveryEffect::RouteToPeer { stanza, .. }
                    | ExternalDeliveryEffect::QueueDetached { stanza, .. }
                    | ExternalDeliveryEffect::RelayFullJid { stanza, .. }
                    | ExternalDeliveryEffect::HostOwnedCopy { stanza, .. },
                ) => {
                    matches!(stanza.as_ref(), Stanza::Message(message) if message.type_ == xmpp_parsers::message::MessageType::Groupchat)
                }
                _ => false,
            };
        let already_receipted = (archived_reflection || matches!(
            effect,
            ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons { .. } | ExternalDeliveryEffect::Carbons { .. })
                | ExternalEffect::Room(crate::server::routes::interpret::effects::room::ExternalRoomEffect::ObserveRoomMessage { .. })
                | ExternalEffect::Room(crate::server::routes::interpret::effects::room::ExternalRoomEffect::RoomActorMutation { .. })
                | ExternalEffect::Direct(ExternalDirectEffect::DmCallThreadState { .. } | ExternalDirectEffect::NotificationActivity { .. })
        )) && !decision.external_receipts[index].is_empty()
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
            if report.outcomes[index].1 == ExternalOutcome::AwaitingPredecessor {
                ExternalOutcome::AwaitingPredecessor
            } else {
                ExternalOutcome::Failed
            }
        } else {
            match tokio::time::timeout_at(
                deadline,
                async {
                    if decision.alias != super::super::decision::AliasOutcomeClass::Existing
                        && decision.external_receipts[index].is_empty()
                        && super::super::storage_hint::is_transient_delivery(effect)
                    {
                        // A policy-settled route has no ledger append authority.
                        // The existing sink may write now or append to an accepted
                        // SM stream; no executor or maintenance retry is retained.
                        let mut immediate = deps.clone();
                        immediate.ingress_append_context = None;
                        if let ExternalEffect::Delivery(delivery @ (ExternalDeliveryEffect::QueueDetached { .. } | ExternalDeliveryEffect::RouteToPeer { .. } | ExternalDeliveryEffect::RelayFullJid { .. })) = effect {
                            let Some(message_key) = decision.message_key else {
                                return EffectOutcome::Unavailable;
                            };
                            let received_at = match super::super::storage_hint::received_at(uow, message_key).await {
                                Ok(received_at) => received_at,
                                Err(error) => {
                                    tracing::warn!(%error, "transient delivery receipt time unavailable; dropping initial attempt");
                                    return EffectOutcome::Unavailable;
                                }
                            };
                            return crate::server::routes::interpret::effects::execute_delivery_with_received_at(
                                delivery.clone(), &immediate, Some(received_at),
                            ).await;
                        }
                        return sink.execute_with_applied(planned[index].clone(), &immediate, &decision.applied_durable).await;
                    }
                    if let Some(outcome) = super::super::execute_uow::execute_legacy_remote(uow, decision, index, effect, deps).await {
                        return outcome;
                    }
                    let dispatch_stream = match super::super::archive_dispatch::effect_ready(uow, decision, index, effect, deps).await {
                        Ok((DispatchReadiness::Ready, stream)) => stream,
                        Ok((crate::ingress_uow::DispatchReadiness::Blocked(predecessors), _)) => return EffectOutcome::Settled(SettledOutcome {
                            refusal: None, persisted: Vec::new(), detached: None,
                            completion: if predecessors.is_empty() { SettledCompletion::DeferredPending } else { SettledCompletion::Deferred },
                        }),
                        Ok((DispatchReadiness::Completed, _)) => return EffectOutcome::Settled(SettledOutcome {
                            refusal: None, persisted: Vec::new(), detached: None, completion: SettledCompletion::Complete,
                        }),
                        Err(error) => {
                            tracing::warn!(%error, "archive dispatch ordering unavailable; leaving obligation pending");
                            return EffectOutcome::Settled(SettledOutcome {
                                refusal: None, persisted: Vec::new(), detached: None, completion: SettledCompletion::Uncertain,
                            });
                        }
                    };
                    // Membership outcomes resolve compensation on the planned
                    // invitation. Preserve that decision when its delivery is
                    // executed transactionally rather than by the generic sink.
                    let resolved_effect = match &planned[index].effect {
                        Effect::External(resolved @ (ExternalEffect::RouteToPeer(_) | ExternalEffect::QueueOfflineDelivery(_))) => resolved,
                        _ => effect,
                    };
                    if let Some(result) = super::super::execute_uow::execute_with_uow(uow, db, decision, index, resolved_effect, deps, deadline).await {
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
                        // Carry the recorded receipt into every live sink, including
                        // a remote plan that becomes local before execution.
                        let mut effect_deps = deps.clone();
                        effect_deps.ingress_append_context = None;
                        if let Some(message_key) = decision.message_key {
                            match carbon_progress::append_context(uow, message_key, effect, &decision.external_receipts[index]).await {
                                Ok(mut context) => {
                                    if let Some(context) = context.as_mut() { context.dispatch_stream = dispatch_stream.clone(); }
                                    effect_deps.ingress_append_context = context;
                                },
                                Err(error) => {
                                    tracing::warn!(%error, "carbon append authority unavailable; leaving obligation pending");
                                    return EffectOutcome::Unavailable;
                                }
                            }
                        }
                        if let ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { target, .. } | ExternalDeliveryEffect::RouteToPeer { jid: target, .. }) = effect {
                            if let Some(message_key) = decision.message_key {
                                if let Some(progress) = decision.route_progress.iter().find(|progress| {
                                    progress.matches(effect) && progress.fanout.contains(target)
                                        && decision.external_receipts[index].contains(&progress.receipt)
                                }) {
                                    let archive_positions = match super::super::archive_dispatch::positions(uow, message_key, &progress.receipt).await {
                                        Ok(positions) => positions,
                                        Err(error) => {
                                            tracing::warn!(%error, "relay archive positions unavailable; preserving delivery");
                                            return EffectOutcome::Unavailable;
                                        }
                                    };
                                    effect_deps.ingress_append_context = Some(SmIngressAppendContext {
                                        message_key,
                                        receipt: progress.receipt.clone(),
                                        received_at: progress.received_at,
                                        archive_positions,
                                        dispatch_stream: dispatch_stream.clone(),
                                        authority: AppendAuthority::Verified,
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
                    recheck[index] = matches!(
                        &result,
                        EffectOutcome::Settled(settled)
                            if settled.completion == SettledCompletion::Deferred
                    );
                    if let EffectOutcome::Settled(settled) = &result {
                        report.refusal = report.refusal.or(settled.refusal);
                    }
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
                        && decision.alias == super::super::decision::AliasOutcomeClass::Existing;
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
        if matches!(
            outcome,
            ExternalOutcome::AwaitingFrameDelivery | ExternalOutcome::AwaitingPredecessor
        ) {
            continue;
        }
        if outcome != ExternalOutcome::Done {
            meter_unresolved(effect, report.phase);
            continue;
        }
        if !settled_complete
            && decision.external_receipts[index]
                .iter()
                .any(|key| !proven[index].contains(key))
        {
            meter_unresolved(effect, report.phase);
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
                meter_unresolved(effect, report.phase);
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
                    meter_unresolved(effect, report.phase);
                    report.receipt_failures.push((key, error.into()));
                }
                Err(_) => {
                    // The receipt may have committed. Do not repeat the side effect.
                    meter_unresolved(effect, report.phase);
                    report
                        .receipt_failures
                        .push((key, ExecutionPersistenceFailure::BudgetExhausted));
                    report.outcomes[index].1 = ExternalOutcome::Uncertain;
                }
            }
        }
    }
    // Intermediate ordering deferrals may have completed in a later round.
    // Only final deferrals contribute unresolved-effect diagnostics.
    for (effect, outcome) in &report.outcomes {
        if *outcome == ExternalOutcome::AwaitingPredecessor {
            meter_unresolved(effect, report.phase);
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
        match tokio::time::timeout(budget, terminalize_if_complete(uow, key, report.phase)).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => report.terminalization_failure = Some(error.into()),
            Err(_) => {
                report.terminalization_failure = Some(ExecutionPersistenceFailure::BudgetExhausted);
                for (effect, _) in &report.outcomes {
                    meter_unresolved(effect, report.phase);
                }
            }
        }
    }
    report
}

pub(in crate::ingress::execute) fn completed_receipts(
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

pub(in crate::ingress::execute) fn proven_receipts(
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
            .filter_map(|intent| super::super::durable::receipt_key(intent).ok())
            .collect::<Vec<_>>();
        return candidates
            .iter()
            .filter(|key| proven.contains(key))
            .cloned()
            .collect();
    }
    if let ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate { receipt, .. }) = effect {
        let (
            Some(IngressEffectIntent::RouteDirect {
                fanout,
                route_identity,
                ..
            }),
            EffectOutcome::InboxPush(resources),
        ) = (receipt.as_deref(), outcome)
        else {
            return Vec::new();
        };
        // A committed inbox projection is queryable after reconnect. Its live
        // refresh ends after the attempt, including disconnected resources;
        // ordinary direct-message routes still require delivery evidence.
        return if matches!(
            route_identity,
            waddle_xmpp::ingress::EffectMessageIdentity::InboxPush(_)
        ) || fanout.iter().all(|resource| resources.contains(resource))
        {
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
                prepared: None,
                recipient: route.recipient.clone(),
                fanout: route.resources.clone(),
                route_identity: identity.clone(),
            });
        }
    }
    let proven = intents
        .iter()
        .filter_map(|intent| super::super::durable::receipt_key(intent).ok())
        .collect::<Vec<_>>();
    candidates
        .iter()
        .filter(|key| proven.contains(key))
        .cloned()
        .collect()
}

pub(in crate::ingress::execute) fn subject_bounce_receipt(
    message: &xmpp_parsers::message::Message,
) -> Option<EffectReceiptKey> {
    let recipient = message.to.as_ref()?.try_as_full().ok()?.clone();
    let error = message.payloads.iter().find_map(|payload| {
        xmpp_parsers::stanza_error::StanzaError::try_from(payload.clone()).ok()
    })?;
    let error = waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&error).ok()?;
    super::super::durable::receipt_key(&waddle_xmpp::ingress::IngressEffectIntent::ErrorReply {
        recipient,
        error,
    })
    .ok()
}

pub(in crate::ingress::execute) fn classify_outcome(
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
                SettledCompletion::Deferred | SettledCompletion::DeferredPending => ExternalOutcome::AwaitingPredecessor,
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
             use crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerOutcome;
             use crate::server::routes::websocket::muc_invites::RecordOutcome;
            match outcome {
                InviteLedgerOutcome::Recorded(
                    RecordOutcome::New { .. } | RecordOutcome::AlreadyOutstanding,
                )
                | InviteLedgerOutcome::Claimed(_) => ExternalOutcome::Done,
            }
        }
        // A storage failure may still have committed its row, so it can never
        // prove that nothing landed. Quota refusal and an unavailable executor
        // are deterministic: nothing was written.
        EffectOutcome::MucUserDelivery(Err(
            crate::server::routes::websocket::handlers::message::muc_invite::MucUserDeliveryError::Storage(_),
        ))
        | EffectOutcome::InviteLedger(Err(
            InviteLedgerError::Storage,
        )) => ExternalOutcome::Uncertain,
        EffectOutcome::PlannedInbox(_) | EffectOutcome::MucUserDelivery(Err(_)) => {
            ExternalOutcome::Failed
        }
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
        EffectOutcome::Archive(Err(_)) | EffectOutcome::Inbox(Err(_)) => ExternalOutcome::Failed,
        EffectOutcome::Delivery(outcome) | EffectOutcome::CarbonFanout { outcome, .. } => {
            match outcome {
                FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                    ExternalOutcome::Done
                }
                FullJidDeliveryOutcome::Unavailable => ExternalOutcome::Failed,
                FullJidDeliveryOutcome::Dropped => ExternalOutcome::Uncertain,
                FullJidDeliveryOutcome::MaybeCommitted => ExternalOutcome::Uncertain,
            }
        }
    }
}

pub(in crate::ingress::execute) fn invite_ledger_noop(outcome: &EffectOutcome) -> bool {
    use crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerOutcome;
    use crate::server::routes::websocket::muc_invites::RecordOutcome;
    matches!(
        outcome,
        EffectOutcome::InviteLedger(Ok(InviteLedgerOutcome::Recorded(
            RecordOutcome::AlreadyOutstanding
        ) | InviteLedgerOutcome::Claimed(false)))
    )
}

pub(in crate::ingress::execute) fn is_invite_delivery_dependent(
    ledger: &ExternalEffect,
    planned: &PlannedEffect,
) -> bool {
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

pub(in crate::ingress::execute) fn has_confirmed_completion(effect: &ExternalEffect) -> bool {
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

pub(in crate::ingress::execute) fn meter_unresolved(
    effect: &ExternalEffect,
    phase: IngressEffectExecutionPhase,
) {
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
    waddle_xmpp::telemetry::reliability::increment_ingress_effect_unresolved(kind, phase);
}

/// Returns false while any durable intent remains without a receipt.
pub async fn terminalize_if_complete(
    uow: &IngressUnitOfWork,
    message_key: MessageKey,
    phase: IngressEffectExecutionPhase,
) -> Result<bool, IngressUowError> {
    Ok(matches!(
        terminalize_if_complete_outcome(uow, message_key, phase).await?,
        Some(
            crate::ingress_substrate::TerminalizeOutcome::Terminalized
                | crate::ingress_substrate::TerminalizeOutcome::AlreadyTerminal
        )
    ))
}

pub(crate) async fn terminalize_if_complete_outcome(
    uow: &IngressUnitOfWork,
    message_key: MessageKey,
    phase: IngressEffectExecutionPhase,
) -> Result<Option<TerminalizeOutcome>, IngressUowError> {
    #[cfg(test)]
    if test_hooks::take_terminalization_timeout(message_key) {
        return Err(IngressUowError::Timeout);
    }
    let mut transaction = uow
        .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
        .await?;
    let outcome =
        terminalize_if_complete_outcome_in_transaction(&mut transaction, message_key, phase)
            .await?;
    transaction.commit().await?;
    Ok(outcome)
}

/// Share the receipt proof with stream retirement without opening a nested transaction.
pub(in crate::ingress) async fn terminalize_if_complete_in_transaction(
    transaction: &mut IngressUowTransaction<'_>,
    message_key: MessageKey,
    phase: IngressEffectExecutionPhase,
) -> Result<bool, IngressUowError> {
    Ok(matches!(
        terminalize_if_complete_outcome_in_transaction(transaction, message_key, phase).await?,
        Some(
            crate::ingress_substrate::TerminalizeOutcome::Terminalized
                | crate::ingress_substrate::TerminalizeOutcome::AlreadyTerminal
        )
    ))
}

pub(in crate::ingress::execute) async fn terminalize_if_complete_outcome_in_transaction(
    transaction: &mut IngressUowTransaction<'_>,
    message_key: MessageKey,
    phase: IngressEffectExecutionPhase,
) -> Result<Option<TerminalizeOutcome>, IngressUowError> {
    if !CanonicalMessageRepository::lock(transaction, message_key).await? {
        return Ok(Some(TerminalizeOutcome::MessageVanished));
    }
    if !EffectReceiptRepository::receipts_complete(transaction, message_key).await? {
        waddle_xmpp::telemetry::reliability::increment_ingress_effect_unresolved(
            waddle_xmpp::telemetry::attributes::IngressUnresolvedEffectKind::Terminalization,
            phase,
        );
        return Ok(None);
    }
    let outcome =
        CanonicalMessageRepository::terminalize(transaction, message_key, chrono::Utc::now())
            .await?;
    Ok(Some(outcome))
}
