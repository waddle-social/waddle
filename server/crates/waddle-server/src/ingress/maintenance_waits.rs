//! Cache only rows whose unfinished frozen audience is entirely lease-blocked.

use std::time::Duration;

use tokio::time::Instant;
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey};

use crate::{
    ingress::{receipt_key, RouteProgress},
    ingress_substrate::RecoveryEvidence,
    ingress_uow::{
        CanonicalMessageRepository, DeliveryProgressRepository, EffectIntentRepository,
        EffectReceiptRepository, IngressUnitOfWork, IngressUowError, SendAttemptRepository,
        SendAttemptStatus, SendObligation,
    },
};

pub(super) async fn known_wait(
    uow: &IngressUnitOfWork,
    key: MessageKey,
) -> Result<Option<(RecoveryEvidence, Instant)>, IngressUowError> {
    // Measure before the transaction: DB latency must not prolong its deadline.
    let observed_at = Instant::now();
    let mut tx = uow
        .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
        .await?;
    if !CanonicalMessageRepository::lock(&mut tx, key).await?
        || CanonicalMessageRepository::is_terminal(&mut tx, key).await?
    {
        return Ok(None);
    }
    let intents = EffectIntentRepository::load(&mut tx, key).await?;
    let receipts = EffectReceiptRepository::keys(&mut tx, key).await?;
    let progress = DeliveryProgressRepository::load_all(&mut tx, key).await?;
    let (Ok(intent_count), Ok(receipt_count), Ok(progress_count)) = (
        u32::try_from(intents.len()),
        u32::try_from(receipts.len()),
        u32::try_from(
            progress
                .iter()
                .map(|(_, resources)| resources.len())
                .sum::<usize>(),
        ),
    ) else {
        return Ok(None);
    };
    let evidence = RecoveryEvidence {
        intents: intent_count,
        receipts: receipt_count,
        progress: progress_count,
    };
    let mut waiting = false;
    for intent in &intents {
        let receipt = receipt_key(intent)?;
        if receipts.contains(&receipt) {
            continue;
        }
        let audience = match RouteProgress::from_intent(intent, None, Vec::new())? {
            Some(route) => route.fanout,
            None => match intent {
                IngressEffectIntent::Carbons {
                    carbon_recipients, ..
                } => carbon_recipients.clone(),
                // Dynamic audiences and independently runnable siblings must
                // retain their normal retry path, even if another copy waits.
                _ => return Ok(None),
            },
        };
        if audience.is_empty() {
            return Ok(None);
        }
        let completed = progress.iter().find(|(stored, _)| *stored == receipt);
        let mut intent_waiting = false;
        for recipient in audience {
            if completed.is_some_and(|(_, recipients)| recipients.contains(&recipient)) {
                continue;
            }
            let obligation = SendObligation {
                message: key,
                receipt: receipt.clone(),
                recipient,
            };
            if SendAttemptRepository::has_custody(&mut tx, &obligation).await?
                || SendAttemptRepository::has_resource_receipt(&mut tx, &obligation).await?
            {
                // Proof exists: recovery can repair aggregate receipts now.
                return Ok(None);
            }
            if !matches!(
                SendAttemptRepository::status(&mut tx, &obligation).await?,
                Some(SendAttemptStatus::Leased | SendAttemptStatus::Started)
            ) {
                return Ok(None);
            }
            waiting = true;
            intent_waiting = true;
        }
        if !intent_waiting {
            // All copies settled; the missing aggregate receipt can be repaired.
            return Ok(None);
        }
    }
    let delay = SendAttemptRepository::next_retry_delay(&mut tx, key).await?;
    tx.commit().await?;
    Ok(delay
        .filter(|delay| waiting && !delay.is_zero())
        .and_then(|delay| observed_at.checked_add(delay))
        .filter(|until| *until > Instant::now())
        .map(|until| (evidence, until)))
}
