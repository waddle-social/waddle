//! Retain completed remote carbon targets independently of the final fanout ACK.
use super::*;
use crate::ingress_uow::CarbonReceiptRepository;
use waddle_xmpp::ingress::IngressEffectIntent;

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
