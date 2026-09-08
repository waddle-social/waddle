//! Run ready plugin observers fairly and persist each completion immediately.
use super::*;
use crate::server::routes::interpret::effects::room::ExternalRoomEffect;

pub(super) fn is_observer(effect: &ExternalEffect) -> bool {
    matches!(
        effect,
        ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage { .. })
    )
}

pub(super) struct Batch<'a> {
    pub decision: &'a IngressDecision,
    pub planned: &'a [PlannedEffect],
    pub report: &'a mut ExecutionReport,
    pub completed: &'a mut [Option<bool>],
    pub proven: &'a mut [Vec<EffectReceiptKey>],
    pub recorded: &'a mut Vec<EffectReceiptKey>,
}

pub(super) async fn execute_ready(
    batch: Batch<'_>,
    db: &Database,
    sink: &ImmediateSink,
    deps: &Deps<'_>,
    deadline: tokio::time::Instant,
) {
    let decision = batch.decision;
    let ready = batch
        .planned
        .iter()
        .enumerate()
        .filter(|(index, planned)| {
            batch.completed[*index].is_none()
                && is_observer(&decision.external[*index])
                && dependencies::ready(&planned.dependencies, &decision.external, batch.completed)
                    == Some(true)
        })
        .map(|(index, planned)| (index, planned.clone()))
        .collect::<Vec<_>>();
    // Only report bookkeeping is locked. Plugin calls and receipt transactions
    // never hold this lock, so a slow observer cannot delay a sibling's receipt.
    let state = tokio::sync::Mutex::new(batch);
    futures::future::join_all(ready.into_iter().map(|(index, planned)| {
        let state = &state;
        async move {
            let effect = &decision.external[index];
            let already_receipted = !decision.external_receipts[index].is_empty()
                && decision.external_receipts[index]
                    .iter()
                    .all(|key| !decision.receipts_pending.contains(key));
            let result = if already_receipted {
                Ok(EffectOutcome::Completed)
            } else if tokio::time::Instant::now() >= deadline {
                Ok(EffectOutcome::Unavailable)
            } else {
                tokio::time::timeout_at(
                    deadline,
                    sink.execute_with_applied(planned, deps, &decision.applied_durable),
                )
                .await
            };
            let receipts = {
                let mut batch = state.lock().await;
                let mut frames = Vec::new();
                let outcome = match result {
                    Ok(result) => {
                        batch.completed[index] = Some(
                            already_receipted || dependencies::permits_dependents(effect, &result),
                        );
                        batch.proven[index] =
                            proven_receipts(effect, &result, &decision.external_receipts[index]);
                        classify_outcome(effect, result, &mut frames)
                    }
                    Err(_) => {
                        batch.completed[index] = Some(false);
                        ExternalOutcome::Uncertain
                    }
                };
                batch.report.outcomes[index].1 = outcome;
                if !frames.is_empty() {
                    batch.report.frame_obligations.push(FrameObligation {
                        frames,
                        receipt_keys: Vec::new(),
                        effect_index: index,
                    });
                }
                if outcome != ExternalOutcome::Done {
                    meter_unresolved(effect);
                    Vec::new()
                } else {
                    completed_receipts(decision, &batch.report.outcomes, batch.proven, index)
                        .into_iter()
                        .filter(|key| !batch.recorded.contains(key))
                        .collect::<Vec<_>>()
                }
            };
            let Some(message_key) = decision.message_key else {
                return;
            };
            for key in receipts {
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
                let mut batch = state.lock().await;
                match result {
                    Ok(Ok(())) => batch.recorded.push(key),
                    Ok(Err(error)) => {
                        meter_unresolved(effect);
                        batch.report.receipt_failures.push((key, error.into()));
                    }
                    Err(_) => {
                        meter_unresolved(effect);
                        batch
                            .report
                            .receipt_failures
                            .push((key, ExecutionPersistenceFailure::BudgetExhausted));
                        batch.report.outcomes[index].1 = ExternalOutcome::Uncertain;
                    }
                }
            }
        }
    }))
    .await;
}
