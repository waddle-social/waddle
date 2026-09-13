//! Freeze recorded authority, release the lock, then replay existing effect arms.

use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::time::Instant;
use waddle_xmpp::ingress::{IngressEffectIntent, IngressEffectKind, MessageKey};

use crate::{
    db::Database,
    ingress_substrate::MessageEnvelope,
    ingress_uow::{
        CanonicalMessageRepository, DeliveryProgressRepository, EffectIntentRepository,
        EffectReceiptRepository, IngressUnitOfWork, IngressUowError, IngressUowTransaction,
    },
};

use super::{recovery_rebuild, Deps, EffectReceiptKey, ImmediateSink, RouteProgress};

pub(super) enum RowRecovery {
    Vanished,
    NothingPending,
    Executed {
        unrecoverable: Vec<IngressEffectKind>,
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
    let Some(frozen) = freeze(uow, key).await? else {
        return Ok(RowRecovery::Vanished);
    };
    if frozen.unreceipted.is_empty() {
        return Ok(RowRecovery::NothingPending);
    }
    #[cfg(test)]
    super::execute::test_hooks::after_recovery_freeze(key).await;
    let rebuilt = recovery_rebuild::rebuild(recovery_rebuild::RecoveryInput {
        key,
        envelope: &frozen.envelope,
        created_at: frozen.created_at,
        recorded: &frozen.recorded,
        unreceipted: &frozen.unreceipted,
        route_progress: frozen.route_progress,
    })?;
    let mut unsupported = rebuilt.decision.external.is_empty() && rebuilt.delegated.is_empty();
    let mut unrecoverable = rebuilt.unrecoverable;
    // Receipts that only a frame to the vanished sender or an unrebuildable
    // obligation could still settle. Non-empty once a frame-only effect ran.
    let mut settled_here: Vec<&EffectReceiptKey> = Vec::new();
    if !rebuilt.decision.external.is_empty() {
        let report = super::execute::execute_effects(
            uow,
            database,
            &rebuilt.decision,
            &ImmediateSink,
            deps,
            deadline.saturating_duration_since(Instant::now()),
        )
        .await;
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
            tracing::debug!(?key, "groupchat recovery has no websocket state");
            continue;
        };
        crate::server::routes::interpret::reconcile_groupchat_notification_recovery(state, row)
            .await?;
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
    Ok(RowRecovery::Executed {
        unrecoverable,
        unsupported,
    })
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
    let mut unreceipted = Vec::new();
    for intent in &recorded {
        if !contains(&mut tx, key, &super::receipt_key(intent)?).await? {
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
    let mut route_progress = Vec::new();
    for intent in &unreceipted {
        let IngressEffectIntent::RouteDirect {
            recipient,
            fanout,
            route_identity,
        } = intent
        else {
            continue;
        };
        let receipt = super::receipt_key(intent)?;
        let completed = DeliveryProgressRepository::load(&mut tx, key, &receipt).await?;
        route_progress.push(RouteProgress {
            receipt,
            recipient: recipient.clone(),
            fanout: fanout.clone(),
            route_identity: route_identity.clone(),
            completed,
        });
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
