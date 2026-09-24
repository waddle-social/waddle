//! Retain completed remote carbon targets independently of the final fanout ACK.
use super::*;
use crate::ingress_uow::CarbonReceiptRepository;
use waddle_xmpp::ingress::IngressEffectIntent;

/// Stamp the exact recorded carbon obligation onto every resulting socket copy.
/// Its original acceptance time survives recovery and a later detached append.
pub(super) async fn append_context(
    uow: &IngressUnitOfWork,
    message_key: MessageKey,
    effect: &ExternalEffect,
    receipts: &[EffectReceiptKey],
) -> Result<Option<SmIngressAppendContext>, IngressUowError> {
    let intent = match effect {
        ExternalEffect::Delivery(ExternalDeliveryEffect::Carbons {
            owner,
            recipient,
            exclude,
            kind,
            ..
        }) => {
            let excluded_source = exclude
                .iter()
                .find(|source| source.to_bare() == *owner)
                .ok_or(IngressUowError::EffectIntentConflict)?;
            IngressEffectIntent::Carbons {
                carbon_recipients: vec![recipient.clone()],
                excluded_source: excluded_source.clone(),
                kind: *kind,
            }
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
            owner,
            exclude,
            kind,
            ..
        }) => IngressEffectIntent::RelayCarbons {
            owner: owner.clone(),
            exclude: exclude.clone(),
            kind: *kind,
        },
        _ => return Ok(None),
    };
    let receipt = super::super::durable::receipt_key(&intent)?;
    if !receipts.contains(&receipt) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let mut tx = uow.begin().await?;
    let recorded = crate::ingress_uow::EffectIntentRepository::load(&mut tx, message_key).await?;
    if !recorded.contains(&intent) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let received_at =
        crate::ingress_uow::CanonicalMessageRepository::created_at(&mut tx, message_key).await?;
    let archive_positions =
        crate::ingress_uow::ArchiveDispatchRepository::positions(&mut tx, message_key, &receipt)
            .await?;
    tx.commit().await?;
    Ok(Some(SmIngressAppendContext {
        message_key,
        receipt,
        received_at: Some(received_at),
        archive_positions,
        dispatch_stream: None,
    }))
}

fn obligation(effect: &ExternalEffect) -> Result<Option<EffectReceiptKey>, IngressUowError> {
    let ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
        owner,
        exclude,
        kind,
        ..
    }) = effect
    else {
        return Ok(None);
    };
    super::super::durable::receipt_key(&IngressEffectIntent::RelayCarbons {
        owner: owner.clone(),
        exclude: exclude.clone(),
        kind: *kind,
    })
    .map(Some)
}

pub(super) async fn prepare(
    uow: &IngressUnitOfWork,
    message: MessageKey,
    effect: &ExternalEffect,
) -> Result<ExternalEffect, IngressUowError> {
    let mut prepared = effect.clone();
    if let Some(receipt) = obligation(effect)? {
        let completed = CarbonReceiptRepository::load(uow, message, &receipt).await?;
        if let ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons { exclude, .. }) =
            &mut prepared
        {
            for recipient in completed {
                if !exclude.contains(&recipient) {
                    exclude.push(recipient);
                }
            }
        }
    }
    Ok(prepared)
}

pub(super) async fn persist(
    uow: &IngressUnitOfWork,
    message: MessageKey,
    effect: &ExternalEffect,
    result: &EffectOutcome,
) -> Result<(), IngressUowError> {
    if let (Some(receipt), EffectOutcome::CarbonFanout { recipients, .. }) =
        (obligation(effect)?, result)
    {
        CarbonReceiptRepository::record(uow, message, &receipt, recipients).await?;
    }
    Ok(())
}
