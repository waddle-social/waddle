//! Resolve a prepared no-store copy only after proving its exact target is gone.
use super::{
    recorded::{ProgressObligation, RouteProgress},
    recovery_reachability::{self, ResourceReachability},
};
use crate::{
    ingress_uow::{
        CanonicalMessageRepository, DeliveryProgressRepository, EffectIntentRepository,
        EffectReceiptRepository, IngressUnitOfWork, IngressUowError, PolicyDiscardReason,
        SendAttemptRepository, SendAttemptStatus, SendObligation,
    },
    server::routes::interpret::{DeliveryExecutionContext, Deps},
};
use jid::FullJid;
use waddle_xmpp::{
    ingress::{IngressEffectIntent, MessageKey},
    xep::xep0334::{has_hint, Hint},
};

#[cfg(all(test, feature = "clustering"))]
type WriteWindow = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
#[cfg(all(test, feature = "clustering"))]
static WRITE_WINDOWS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<MessageKey, WriteWindow>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
#[cfg(all(test, feature = "clustering"))]
pub(crate) fn before_next_write(
    key: MessageKey,
    future: impl std::future::Future<Output = ()> + Send + 'static,
) {
    WRITE_WINDOWS
        .lock()
        .expect("policy write windows")
        .insert(key, Box::pin(future));
}

pub(super) fn forbids_offline_handoff(progress: &RouteProgress) -> bool {
    protected_target(progress).is_some()
}

fn protected_target(progress: &RouteProgress) -> Option<&FullJid> {
    let ProgressObligation::Direct {
        prepared: Some(prepared),
        ..
    } = &progress.obligation
    else {
        return None;
    };
    let message = prepared.message();
    let target = message.to.as_ref()?.try_as_full().ok()?;
    (progress.fanout.as_slice() == std::slice::from_ref(target)
        && message.from.as_ref()?.to_bare() != target.to_bare()
        && has_hint(message, Hint::NoStore)
        && !has_hint(message, Hint::Store))
    .then_some(target)
}

pub(super) async fn settle_unavailable(
    uow: &IngressUnitOfWork,
    deps: &Deps<'_>,
    key: MessageKey,
    progress: &RouteProgress,
    resource: &FullJid,
) -> Result<Option<Vec<IngressEffectIntent>>, IngressUowError> {
    if deps.delivery_execution_context != DeliveryExecutionContext::MaintenanceRecovery
        || protected_target(progress) != Some(resource)
    {
        return Ok(None);
    }
    let Some(owner) = crate::ingress_uow::capture_local_policy_owner(uow, resource).await? else {
        return Ok(None);
    };
    // Absence of a local socket alone is insufficient: an existing resumable
    // stream or a peer-held socket still owns delivery, even after claim moves.
    if recovery_reachability::locally_reachable(deps, resource).await
        || recovery_reachability::reachable_elsewhere(
            deps,
            resource,
            recovery_reachability::SETTLEMENT_FANOUT_BUDGET,
        )
        .await
            != ResourceReachability::AbsentEverywhere
    {
        return Ok(None);
    }
    #[cfg(all(test, feature = "clustering"))]
    {
        let window = WRITE_WINDOWS
            .lock()
            .expect("policy write windows")
            .remove(&key);
        if let Some(window) = window {
            window.await;
        }
    }
    // Peer probes can take the full budget. Recheck both sockets and resumable
    // custody, including durable sessions, before taking any write locks.
    if recovery_reachability::locally_reachable(deps, resource).await {
        return Ok(None);
    }
    let mut tx = uow
        .begin_with_timeouts(
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(250),
        )
        .await?;
    // Match ownership lock order: epoch -> local claim -> canonical row.
    if !crate::ingress_uow::assert_local_policy_owner(&mut tx, resource, &owner).await? {
        return Ok(None);
    }
    let intents = EffectIntentRepository::load(&mut tx, key).await?;
    let intent = progress
        .settle_evidence()
        .with_encoded_v1(IngressEffectIntent::decode_v1)??;
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await?
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    if !intents.contains(&intent)
        || super::recorded::prepared_direct_message(&envelope, &intent).is_none()
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    if EffectReceiptRepository::contains(
        &mut tx,
        key,
        progress.receipt.kind,
        &progress.receipt.semantic_identity_hash,
    )
    .await?
    {
        tx.commit().await?;
        return Ok(Some(vec![intent]));
    }
    let obligation = SendObligation {
        message: key,
        receipt: progress.receipt.clone(),
        recipient: resource.clone(),
    };
    if DeliveryProgressRepository::load(&mut tx, key, &progress.receipt)
        .await?
        .contains(resource)
    {
        // A direct resource proof appearing after the reachability snapshot
        // remains ordinary completion, never a policy discard.
        crate::ingress_uow::settle_recorded(&mut tx, key, std::slice::from_ref(&intent)).await?;
        super::execute::terminalize_if_complete_in_transaction(
            &mut tx,
            key,
            DeliveryExecutionContext::MaintenanceRecovery.into(),
        )
        .await?;
        tx.commit().await?;
        return Ok(Some(vec![intent]));
    }
    if SendAttemptRepository::has_custody(&mut tx, &obligation).await?
        || matches!(
            SendAttemptRepository::status(&mut tx, &obligation).await?,
            Some(
                SendAttemptStatus::Leased
                    | SendAttemptStatus::Started
                    | SendAttemptStatus::Completed
            )
        )
    {
        return Ok(None);
    }
    SendAttemptRepository::retire_expired_attempt(&mut tx, &obligation).await?;
    EffectReceiptRepository::record_policy_discard(
        &mut tx,
        key,
        &progress.receipt,
        PolicyDiscardReason::StorageHintForbidsHandoff,
    )
    .await?;
    super::execute::terminalize_if_complete_in_transaction(
        &mut tx,
        key,
        DeliveryExecutionContext::MaintenanceRecovery.into(),
    )
    .await?;
    tx.commit().await?;
    Ok(Some(vec![intent]))
}
