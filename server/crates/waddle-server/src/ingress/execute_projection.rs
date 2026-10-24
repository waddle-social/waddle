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

/// Sealed inbox refreshes retain their best-effort policy, but their account
/// audience and committed projection source cannot be rediscovered after h.
pub(super) async fn execute_sealed_inbox_push(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalEffect,
    deps: &crate::server::routes::interpret::Deps<'_>,
) -> Result<Option<EffectOutcome>, IngressUowError> {
    use crate::server::routes::interpret::effects::{Effect, ImmediateSink, PlannedEffect};
    use waddle_xmpp::ingress::{DeliveryKey, EffectMessageIdentity, InboxProjectionMutation};
    let ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
        owner,
        projection,
        receipt,
    }) = effect
    else {
        return Ok(None);
    };
    let key = decision
        .message_key
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    let (envelope, recorded) =
        crate::ingress_uow::CarbonReceiptRepository::load_authority(uow.database(), key).await?;
    let Some(manifest) = envelope.room_manifest() else {
        return Ok(None);
    };
    let Some(ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
        owner: original_owner,
        projection: original_projection,
        ..
    })) = decision.external.get(index)
    else {
        return Err(IngressUowError::EffectIntentConflict);
    };
    if owner != original_owner || projection != original_projection || manifest.message_key() != key
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let intent = receipt
        .as_deref()
        .ok_or(IngressUowError::EffectIntentConflict)?
        .with_encoded_v1(IngressEffectIntent::decode_v1)??;
    let IngressEffectIntent::RouteDirect {
        recipient,
        fanout,
        route_identity: EffectMessageIdentity::InboxPush(_),
        prepared: None,
    } = &intent
    else {
        return Err(IngressUowError::EffectIntentConflict);
    };
    let receipt_key = crate::ingress::receipt_key(&intent)?;
    let expected = decision
        .external_receipts
        .get(index)
        .ok_or(IngressUowError::EffectIntentConflict)?;
    if recipient != owner
        || fanout.iter().any(|target| &target.to_bare() != owner)
        || !recorded.contains(&intent)
        || !manifest.recorded_intents().contains(&intent)
        || !expected.contains(&receipt_key)
        || expected.iter().any(|receipt| receipt != &receipt_key)
        || manifest.child(&receipt_key, None).is_none()
        || manifest
            .children
            .iter()
            .filter(|child| child.receipt == receipt_key && child.target.is_some())
            .count()
            != fanout.len()
        || fanout.iter().any(|target| {
            manifest
                .child(&receipt_key, Some(target))
                .is_none_or(|child| {
                    child.delivery_key != DeliveryKey::resource(key, &intent.semantic_key(), target)
                        || child.disposition != super::super::RoomManifestChildDisposition::Delivery
                })
        })
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let entry = decision
        .applied_durable
        .inbox(*projection)
        .ok_or(IngressUowError::EffectIntentConflict)?;
    if entry.partner != manifest.room || entry.kind != waddle_xmpp::inbox::ConversationKind::MucRoom
        || !recorded.iter().any(|source| matches!(source, IngressEffectIntent::InboxProject { owner: saved_owner, mutation }
            if saved_owner == owner && match mutation {
                InboxProjectionMutation::GroupchatChannel { room, .. } | InboxProjectionMutation::GroupchatChannelRead { room } => room == &entry.partner && entry.thread_id.is_none(),
                InboxProjectionMutation::GroupchatThread { room, thread_id } | InboxProjectionMutation::GroupchatThreadRead { room, thread_id } => room == &entry.partner && entry.thread_id.as_deref() == Some(thread_id.as_str()),
                InboxProjectionMutation::GroupchatChannelAndThread { room, thread_id, .. } => room == &entry.partner && (entry.thread_id.is_none() || entry.thread_id.as_deref() == Some(thread_id.as_str())),
                _ => false,
            }))
        || !recorded.iter().any(|source| super::super::room_canonical::source(&envelope, source).is_ok_and(|message| {
            waddle_xmpp::xep::extract_stanza_ids(message).contains(&manifest.stanza_id)
        }))
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    let thread = match entry.thread_id.as_deref() {
        Some(thread) => Some(
            waddle_xmpp_core::mam::ThreadId::try_new(thread)
                .ok_or(IngressUowError::EffectIntentConflict)?,
        ),
        None => None,
    };
    let binding = DeliveryKey::inbox_projection(key, owner, (&entry.partner, thread.as_ref()));
    let mut tx = uow
        .begin_with_timeouts(
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(250),
        )
        .await?;
    if DeliveryEffectRepository::lookup(&mut tx, binding).await? != Some(key) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    tx.commit().await?;
    // Keep the original Phase-B result and projection index. No inbox reread,
    // room roster lookup, SM allocation, or new durable sink identity occurs.
    Ok(Some(
        ImmediateSink
            .execute_with_applied(
                PlannedEffect::new(Effect::External(ExternalEffect::Direct(
                    ExternalDirectEffect::PushInboxUpdate {
                        owner: owner.clone(),
                        projection: *projection,
                        receipt: Some(Box::new(intent)),
                    },
                ))),
                deps,
                &decision.applied_durable,
            )
            .await,
    ))
}

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
