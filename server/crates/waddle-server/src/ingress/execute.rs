//! Bounded post-commit execution. Failures never revise ingress authority.
use std::time::Duration;

use waddle_xmpp::{ingress::MessageKey, Stanza};

use crate::{
    db::Database,
    ingress_uow::{
        CanonicalMessageRepository, EffectReceiptRepository, IngressUnitOfWork, IngressUowError,
    },
    server::routes::interpret::{
        effects::{
            delivery::ExternalDeliveryEffect, direct::ExternalDirectEffect, Effect, EffectOutcome,
            EffectSink, ExternalEffect, ImmediateSink, PlannedEffect,
        },
        Deps, FullJidDeliveryOutcome,
    },
};

use super::decision::{EffectReceiptKey, IngressDecision};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalOutcome {
    Done,
    Failed,
    Uncertain,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionPersistenceFailure {
    #[error("post-commit persistence failed: {0}")]
    Storage(#[from] IngressUowError),
    #[error("post-commit persistence budget exhausted")]
    BudgetExhausted,
}

#[derive(Debug, Default)]
pub struct ExecutionReport {
    pub outcomes: Vec<(ExternalEffect, ExternalOutcome)>,
    pub frames: Vec<Stanza>,
    /// A completed side effect can remain unresolved when its receipt write fails.
    pub receipt_failures: Vec<(EffectReceiptKey, ExecutionPersistenceFailure)>,
    pub terminalization_failure: Option<ExecutionPersistenceFailure>,
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
    let deadline = tokio::time::Instant::now() + budget;
    let mut recorded = Vec::new();
    for (index, effect) in decision.external.iter().enumerate() {
        let outcome = if tokio::time::Instant::now() >= deadline {
            ExternalOutcome::Failed
        } else {
            match tokio::time::timeout_at(
                deadline,
                sink.execute(PlannedEffect::new(Effect::External(effect.clone())), deps),
            )
            .await
            {
                Ok(result) => classify_outcome(effect, result, &mut report.frames),
                Err(_) => ExternalOutcome::Uncertain,
            }
        };
        report.outcomes.push((effect.clone(), outcome));
        if outcome != ExternalOutcome::Done {
            meter_unresolved(effect);
            continue;
        }
        let Some(message_key) = decision.message_key else {
            continue;
        };
        for key in completed_receipts(decision, &report.outcomes, index) {
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
    if let Some(key) = decision.message_key {
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
                })
        })
        .cloned()
        .collect()
}

fn classify_outcome(
    effect: &ExternalEffect,
    outcome: EffectOutcome,
    frames: &mut Vec<Stanza>,
) -> ExternalOutcome {
    match outcome {
        EffectOutcome::Frames(mut produced) => {
            frames.append(&mut produced);
            ExternalOutcome::Done
        }
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
        ) | ExternalEffect::Direct(
            ExternalDirectEffect::PushInboxUpdate { .. }
                | ExternalDirectEffect::ScrubReplayForTombstone { .. }
        )
    )
}

fn meter_unresolved(effect: &ExternalEffect) {
    use waddle_xmpp::telemetry::attributes::IngressUnresolvedEffectKind;
    let kind = match effect {
        ExternalEffect::Frame(_) => IngressUnresolvedEffectKind::Frame,
        ExternalEffect::Direct(_) => IngressUnresolvedEffectKind::Direct,
        ExternalEffect::Room(_) => IngressUnresolvedEffectKind::Room,
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
    if !CanonicalMessageRepository::lock(&mut transaction, message_key).await? {
        return Ok(false);
    }
    if !EffectReceiptRepository::receipts_complete(&mut transaction, message_key).await? {
        return Ok(false);
    }
    let outcome =
        CanonicalMessageRepository::terminalize(&mut transaction, message_key, chrono::Utc::now())
            .await?;
    transaction.commit().await?;
    Ok(!matches!(
        outcome,
        crate::ingress_substrate::TerminalizeOutcome::MessageVanished
    ))
}

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
            external: vec![frame(), frame()],
            external_receipts: vec![vec![key.clone()], vec![key.clone()]],
            receipts_pending: vec![key.clone()],
        };
        let mut outcomes = vec![(frame(), ExternalOutcome::Done)];
        assert!(completed_receipts(&decision, &outcomes, 0).is_empty());
        outcomes.push((frame(), ExternalOutcome::Failed));
        assert!(completed_receipts(&decision, &outcomes, 1).is_empty());
        outcomes[1].1 = ExternalOutcome::Done;
        assert_eq!(completed_receipts(&decision, &outcomes, 1), vec![key]);
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
            entry: Box::new(waddle_xmpp::inbox::InboxEntry::new(
                "sender@example.com".parse().expect("sender"),
                waddle_xmpp::inbox::ConversationKind::Direct,
                "id",
                0,
            )),
        });
        assert_eq!(
            classify_outcome(&effect, EffectOutcome::Completed, &mut Vec::new()),
            ExternalOutcome::Uncertain
        );
    }
}
