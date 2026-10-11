//! Apply approved local projections and their completion under one canonical lock.
use crate::{
    ingress::decision::IngressDecision,
    ingress_uow::{
        settle_recorded, CanonicalMessageRepository, DeliveryEffectRepository,
        EffectDeliveryBinding, EffectIntentRepository, EffectReceiptRepository, IngressUnitOfWork,
        IngressUowError, ProjectionRepository,
    },
    server::routes::interpret::effects::{
        direct::ExternalDirectEffect, EffectOutcome, ExternalEffect, SettledCompletion,
        SettledOutcome,
    },
};
use waddle_xmpp::ingress::IngressEffectIntent;

#[cfg(test)]
static FAIL_AFTER_UPDATE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<waddle_xmpp::ingress::MessageKey>>,
> = std::sync::LazyLock::new(Default::default);

#[cfg(test)]
fn fail_after_projection_update(key: waddle_xmpp::ingress::MessageKey) {
    FAIL_AFTER_UPDATE
        .lock()
        .expect("projection fault hooks")
        .insert(key);
}

pub(super) async fn execute(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalEffect,
) -> EffectOutcome {
    match store_projection(uow, decision, index, effect).await {
        Ok(settled) => EffectOutcome::Settled(settled),
        Err(error) => {
            tracing::warn!(%error, "local projection settlement failed");
            EffectOutcome::Unavailable
        }
    }
}

async fn store_projection(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalEffect,
) -> Result<SettledOutcome, IngressUowError> {
    let evidence = match effect {
        ExternalEffect::Direct(ExternalDirectEffect::NotificationActivity { owner, mutation }) => {
            vec![IngressEffectIntent::NotificationActivityPreview {
                owner: owner.clone(),
                mutation: mutation.clone(),
            }]
        }
        ExternalEffect::Direct(
            ExternalDirectEffect::LinkPreviewRefs { mutations }
            | ExternalDirectEffect::ClearLinkPreviewRefs { mutations },
        ) => mutations
            .iter()
            .map(|mutation| IngressEffectIntent::LinkPreviewMediaRef {
                mutation: mutation.clone(),
            })
            .collect(),
        _ => return Err(IngressUowError::EffectIntentConflict),
    };
    let key = decision
        .message_key
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    let mut tx = uow
        .begin_with_timeouts(
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(250),
        )
        .await?;
    if !CanonicalMessageRepository::lock(&mut tx, key).await? {
        return Err(IngressUowError::EffectIntentMessageMissing);
    }
    let recorded = EffectIntentRepository::load(&mut tx, key).await?;
    let accepted_at = CanonicalMessageRepository::created_at(&mut tx, key).await?;
    let expected = decision
        .external_receipts
        .get(index)
        .ok_or(IngressUowError::EffectIntentConflict)?;
    if evidence.is_empty()
        || expected.is_empty()
        || expected.iter().any(|receipt| {
            !evidence
                .iter()
                .any(|intent| crate::ingress::receipt_key(intent).as_ref().ok() == Some(receipt))
        })
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    for intent in &evidence {
        let receipt = crate::ingress::receipt_key(intent)?;
        if !recorded.contains(intent) || !expected.contains(&receipt) {
            return Err(IngressUowError::EffectIntentConflict);
        }
        if !matches!(
            DeliveryEffectRepository::bind_effect(&mut tx, key, &intent.semantic_key()).await?,
            EffectDeliveryBinding::Bound(_)
        ) {
            return Err(IngressUowError::EffectIntentConflict);
        }
        if !EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await?
        {
            ProjectionRepository::apply(&mut tx, intent, accepted_at).await?;
        }
    }
    #[cfg(test)]
    if FAIL_AFTER_UPDATE
        .lock()
        .expect("projection fault hooks")
        .remove(&key)
    {
        return Err(IngressUowError::Timeout);
    }
    let persisted = settle_recorded(&mut tx, key, &evidence).await?;
    tx.commit().await?;
    Ok(SettledOutcome {
        refusal: None,
        persisted,
        completion: SettledCompletion::Complete,
        detached: None,
    })
}

#[cfg(test)]
#[path = "execute_projection_tests.rs"]
mod tests;
