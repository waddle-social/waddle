//! Freeze recorded authority, release the lock, then replay existing effect arms.

use std::time::Duration;

use chrono::{DateTime, Utc};
use jid::BareJid;
use tokio::time::Instant;
use waddle_xmpp::ingress::{IngressEffectIntent, IngressEffectKind, MessageKey};
use waddle_xmpp::protocol::Blocklist;

use crate::{
    db::Database,
    ingress_substrate::MessageEnvelope,
    ingress_uow::{
        CanonicalMessageRepository, DeliveryProgressRepository, EffectIntentRepository,
        EffectReceiptRepository, IngressUnitOfWork, IngressUowError, IngressUowTransaction,
    },
};

use super::{recovery_rebuild, Deps, EffectReceiptKey, ImmediateSink, RouteProgress};
use crate::server::routes::interpret::DeliveryExecutionContext;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AttemptClassification {
    Evaluable,
    Inconclusive,
}

pub(super) enum RowRecovery {
    Vanished,
    NothingPending,
    Executed {
        unrecoverable: Vec<IngressEffectKind>,
        pending: Vec<IngressEffectKind>,
        classification: AttemptClassification,
        /// Nothing left on the row can make progress until its evidence changes.
        unsupported: bool,
    },
}

struct FrozenRecovery {
    envelope: MessageEnvelope,
    created_at: DateTime<Utc>,
    recorded: Vec<IngressEffectIntent>,
    unreceipted: Vec<IngressEffectIntent>,
    route_progress: Vec<RouteProgress>,
}

/// Freeze under the canonical lock, release it, rebuild, then run the arms.
/// Progress accounting is the caller's: it compares receipt counts around this
/// call so a row deadline cancelling any await here cannot lose credit.
pub(super) async fn recover_row(
    database: &Database,
    uow: &IngressUnitOfWork,
    deps: &Deps<'_>,
    key: MessageKey,
    deadline: Instant,
) -> Result<RowRecovery, IngressUowError> {
    #[cfg(test)]
    record_attempt(key);
    let Some(mut frozen) = freeze(uow, key).await? else {
        return Ok(RowRecovery::Vanished);
    };
    if frozen.unreceipted.is_empty() {
        return Ok(RowRecovery::NothingPending);
    }
    #[cfg(test)]
    super::execute::test_hooks::after_recovery_freeze(key).await;
    let mut classification = AttemptClassification::Evaluable;
    let departed = discharge_departed_copies(uow, deps, key, &mut frozen).await?;
    if departed.classification == AttemptClassification::Inconclusive {
        classification = AttemptClassification::Inconclusive;
    }
    let pending = pending_kinds(&frozen.unreceipted);
    let blocked_recipients = blocked_recipients(deps, &frozen).await?;
    let host_owned_resources = frozen
        .route_progress
        .iter()
        .flat_map(|progress| &progress.fanout)
        .filter(|target| deps.owns_host_resource(target))
        .cloned()
        .collect();
    let rebuilt = recovery_rebuild::rebuild(recovery_rebuild::RecoveryInput {
        key,
        envelope: &frozen.envelope,
        created_at: frozen.created_at,
        recorded: &frozen.recorded,
        unreceipted: &frozen.unreceipted,
        route_progress: frozen.route_progress,
        host_owned_resources,
        departed_occupants: departed.occupants,
        blocked_recipients: &blocked_recipients,
    })?;
    record_discarded_receipts(uow, key, &rebuilt.discarded_receipts).await?;
    let mut unsupported = rebuilt.decision.external.is_empty()
        && rebuilt.delegated.is_empty()
        && !rebuilt.unsupported_receipts.is_empty();
    let mut unrecoverable = rebuilt.unrecoverable;
    // Receipts that only a frame to the vanished sender or an unrebuildable
    // obligation could still settle. Non-empty once a frame-only effect ran.
    let mut settled_here: Vec<&EffectReceiptKey> = Vec::new();
    if !rebuilt.decision.external.is_empty() {
        let mut recovery_deps = deps.clone();
        recovery_deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
        let report = super::execute::execute_effects(
            uow,
            database,
            &rebuilt.decision,
            &ImmediateSink,
            &recovery_deps,
            deadline.saturating_duration_since(Instant::now()),
        )
        .await;
        classification = classify_report(&report);
        // Frames belong to the sender's connection, which no longer exists
        // during recovery. A warning reply an observer produced cannot be
        // delivered; retrying every tick would only re-invoke the plugin.
        for obligation in &report.frame_obligations {
            let effect = &rebuilt.decision.external[obligation.effect_index];
            if let Some(kind) = frame_only_kind(effect) {
                if !unrecoverable.contains(&kind) {
                    unrecoverable.push(kind);
                }
                settled_here.extend(&rebuilt.decision.external_receipts[obligation.effect_index]);
            }
        }
    }
    for row in &rebuilt.delegated {
        let Some(state) = deps.web_socket_state else {
            classification = AttemptClassification::Inconclusive;
            tracing::debug!(?key, "groupchat recovery has no websocket state");
            continue;
        };
        let outcome =
            crate::server::routes::interpret::reconcile_groupchat_notification_recovery(state, row)
                .await?;
        if outcome != super::RecoverySweepOutcome::Completed {
            classification = AttemptClassification::Inconclusive;
        }
    }
    // Classify after every arm and delegation on this row has settled: the row
    // is cached only when nothing left on it can make progress until its
    // evidence changes.
    if !settled_here.is_empty() {
        settled_here.extend(&rebuilt.unsupported_receipts);
        let missing = missing_receipts(uow, key, &rebuilt.decision.receipts_pending).await?;
        unsupported = !missing.is_empty()
            && missing
                .iter()
                .all(|receipt| settled_here.contains(&receipt));
    }
    if unsupported {
        classification = AttemptClassification::Inconclusive;
    }
    Ok(RowRecovery::Executed {
        unrecoverable,
        pending,
        classification,
        unsupported,
    })
}

/// Settle the frozen copies this node can prove the room no longer owes, then
/// drop every obligation the settlement discharged from the frozen authority so
/// the rebuild neither replays those copies nor reports them unrecoverable.
async fn discharge_departed_copies(
    uow: &IngressUnitOfWork,
    deps: &Deps<'_>,
    key: MessageKey,
    frozen: &mut FrozenRecovery,
) -> Result<super::recovery_departed::DepartedSettlement, IngressUowError> {
    let departed =
        super::recovery_departed::settle_departed_occupants(uow, deps, key, &frozen.route_progress)
            .await?;
    if departed.settled.is_empty() {
        return Ok(departed);
    }
    frozen
        .unreceipted
        .retain(|intent| !departed.settled.contains(intent));
    frozen
        .route_progress
        .retain(|progress| !departed.settled.contains(&progress.settle_evidence()));
    // A row whose last obligation the settlement discharged must leave the
    // backlog in this pass rather than wait for the next terminalization phase.
    super::execute::terminalize_if_complete_outcome(
        uow,
        key,
        DeliveryExecutionContext::MaintenanceRecovery.into(),
    )
    .await?;
    Ok(departed)
}

fn pending_kinds(intents: &[IngressEffectIntent]) -> Vec<IngressEffectKind> {
    let mut kinds = Vec::new();
    for intent in intents {
        let kind = intent.kind();
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

fn classify_report(report: &super::execute::ExecutionReport) -> AttemptClassification {
    if report
        .outcomes
        .iter()
        .any(|(_, outcome)| *outcome == super::execute::ExternalOutcome::Uncertain)
        || !report.receipt_failures.is_empty()
        || report.terminalization_failure.is_some()
    {
        AttemptClassification::Inconclusive
    } else {
        AttemptClassification::Evaluable
    }
}

/// Consult current policy only after freeze released the ingress transaction.
async fn blocked_recipients(
    deps: &Deps<'_>,
    frozen: &FrozenRecovery,
) -> Result<Vec<BareJid>, IngressUowError> {
    let Some(storage) = deps.blocking_storage else {
        return Ok(Vec::new());
    };
    // Policy I/O only for routes some rebuild path can produce: a lost
    // groupchat row records one inbox-push route per occupant, none of which
    // any path rebuilds, and one blocklist read per occupant would spend the
    // row deadline before its observer or notification recovery ran.
    let recipients: std::collections::BTreeSet<_> = frozen
        .unreceipted
        .iter()
        .filter(|intent| {
            recovery_rebuild::policy_checked_direct_route(
                &frozen.envelope,
                &frozen.recorded,
                intent,
            )
        })
        .filter_map(|intent| match intent {
            IngressEffectIntent::RouteDirect { recipient, .. } => Some(recipient),
            _ => None,
        })
        .collect();
    let mut blocked = Vec::new();
    for recipient in recipients {
        let entries = storage
            .list_blocked_jid_entries(recipient)
            .await
            .map_err(IngressUowError::BlocklistUnavailable)?;
        if let Some(sender) = frozen.envelope.message().from.as_ref() {
            if Blocklist::new(entries).contains_jid(sender) {
                tracing::debug!(%recipient, %sender, "recovery dropping route: recipient blocked sender post-intake");
                blocked.push(recipient.clone());
            }
        }
    }
    Ok(blocked)
}

async fn record_discarded_receipts(
    uow: &IngressUnitOfWork,
    key: MessageKey,
    discarded: &[EffectReceiptKey],
) -> Result<(), IngressUowError> {
    if discarded.is_empty() {
        return Ok(());
    }
    let mut tx = uow
        .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
        .await?;
    if !CanonicalMessageRepository::lock(&mut tx, key).await? {
        return Err(IngressUowError::EffectIntentMessageMissing);
    }
    for receipt in discarded {
        EffectReceiptRepository::record_receipt(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await?;
    }
    super::execute::terminalize_if_complete_in_transaction(
        &mut tx,
        key,
        DeliveryExecutionContext::MaintenanceRecovery.into(),
    )
    .await?;
    tx.commit().await
}

/// Effects whose only completion path is a frame to the original sender.
fn frame_only_kind(
    effect: &crate::server::routes::interpret::effects::ExternalEffect,
) -> Option<IngressEffectKind> {
    use crate::server::routes::interpret::effects::{room::ExternalRoomEffect, ExternalEffect};
    match effect {
        ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage { .. }) => {
            Some(IngressEffectKind::RoomObserver)
        }
        _ => None,
    }
}

async fn freeze(
    uow: &IngressUnitOfWork,
    key: MessageKey,
) -> Result<Option<FrozenRecovery>, IngressUowError> {
    let mut tx = uow
        .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
        .await?;
    if !CanonicalMessageRepository::lock(&mut tx, key).await?
        || CanonicalMessageRepository::is_terminal(&mut tx, key).await?
    {
        tx.commit().await?;
        return Ok(None);
    }
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await?
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    let created_at = CanonicalMessageRepository::created_at(&mut tx, key).await?;
    let recorded = EffectIntentRepository::load(&mut tx, key).await?;
    // One bulk read of receipt identities: a groupchat row can carry one
    // intent per occupant, and a per-intent lookup would spend the whole row
    // deadline before anything recoverable on it could run.
    let receipted = EffectReceiptRepository::keys(&mut tx, key).await?;
    let mut unreceipted = Vec::new();
    for intent in &recorded {
        if !receipted.contains(&super::receipt_key(intent)?) {
            unreceipted.push(intent.clone());
        }
    }
    // A live invitation delivery and its offline fallback are mutually
    // exclusive: one committed receipt proves both, exactly as alias replay.
    super::commit::reconcile_invitation_delivery_receipts(
        &mut tx,
        key,
        &recorded,
        &mut unreceipted,
    )
    .await?;
    // One bulk read of delivery progress for the same reason as the receipts.
    let progress = DeliveryProgressRepository::load_all(&mut tx, key).await?;
    let mut route_progress = Vec::new();
    let mut empty_muc = Vec::new();
    for intent in &unreceipted {
        let Some(mut route) = RouteProgress::from_intent(intent, Some(created_at), Vec::new())?
        else {
            continue;
        };
        route.completed = progress
            .iter()
            .find(|(receipt, _)| receipt == &route.receipt)
            .map(|(_, completed)| completed.clone())
            .unwrap_or_default();
        if !route.is_direct() && route.fanout.is_empty() {
            crate::ingress_uow::settle_recorded(&mut tx, key, &[route.settle_evidence()]).await?;
            empty_muc.push(route.settle_evidence());
        } else {
            route_progress.push(route);
        }
    }
    if !empty_muc.is_empty() {
        unreceipted.retain(|intent| !empty_muc.contains(intent));
        super::execute::terminalize_if_complete_in_transaction(
            &mut tx,
            key,
            DeliveryExecutionContext::MaintenanceRecovery.into(),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Some(FrozenRecovery {
        envelope,
        created_at,
        recorded,
        unreceipted,
        route_progress,
    }))
}

async fn contains(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
    receipt: &EffectReceiptKey,
) -> Result<bool, IngressUowError> {
    EffectReceiptRepository::contains(tx, key, receipt.kind, &receipt.semantic_identity_hash).await
}

/// Pending receipts that are still absent after execution.
async fn missing_receipts(
    uow: &IngressUnitOfWork,
    key: MessageKey,
    pending: &[EffectReceiptKey],
) -> Result<Vec<EffectReceiptKey>, IngressUowError> {
    let mut tx = uow.begin().await?;
    let mut missing = Vec::new();
    for receipt in pending {
        if !contains(&mut tx, key, receipt).await? {
            missing.push(receipt.clone());
        }
    }
    tx.commit().await?;
    Ok(missing)
}

#[cfg(test)]
static ATTEMPTS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<MessageKey, u64>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
fn record_attempt(key: MessageKey) {
    *ATTEMPTS
        .lock()
        .expect("recovery attempts")
        .entry(key)
        .or_default() += 1;
}

#[cfg(test)]
pub(super) fn attempt_count(key: MessageKey) -> u64 {
    ATTEMPTS
        .lock()
        .expect("recovery attempts")
        .get(&key)
        .copied()
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "recovery_executor_tests.rs"]
mod tests;
