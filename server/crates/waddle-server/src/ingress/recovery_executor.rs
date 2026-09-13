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
        recovered: u64,
        unrecoverable: Vec<IngressEffectKind>,
        terminal: bool,
        /// Only a rebuild with neither effects nor delegation can be cached.
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
    let unsupported = rebuilt.decision.external.is_empty() && rebuilt.delegated.is_empty();
    if !rebuilt.decision.external.is_empty() {
        super::execute::execute_effects(
            uow,
            database,
            &rebuilt.decision,
            &ImmediateSink,
            deps,
            deadline.saturating_duration_since(Instant::now()),
        )
        .await;
    }
    for row in &rebuilt.delegated {
        if let Some(state) = deps.web_socket_state {
            crate::server::routes::interpret::reconcile_groupchat_notification_recovery(state, row)
                .await?;
        } else {
            tracing::debug!(?key, "groupchat recovery has no websocket state");
        }
    }
    let (recovered, terminal) = recount(uow, key, &rebuilt.decision.receipts_pending).await?;
    Ok(RowRecovery::Executed {
        recovered,
        unrecoverable: rebuilt.unrecoverable,
        terminal,
        unsupported,
    })
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

async fn recount(
    uow: &IngressUnitOfWork,
    key: MessageKey,
    pending: &[EffectReceiptKey],
) -> Result<(u64, bool), IngressUowError> {
    let mut tx = uow.begin().await?;
    let mut recovered = 0;
    for receipt in pending {
        recovered += u64::from(contains(&mut tx, key, receipt).await?);
    }
    let terminal = CanonicalMessageRepository::is_terminal(&mut tx, key).await?;
    tx.commit().await?;
    Ok((recovered, terminal))
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
