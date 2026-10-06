//! Store a frozen system archive only after its authorizing room mutation.
use crate::ingress::decision::IngressDecision;
use crate::{
    ingress_uow::{
        settle_recorded, CanonicalMessageRepository, EffectIntentRepository, IngressUnitOfWork,
        IngressUowError, MamArchiveRepository,
    },
    server::routes::interpret::effects::{
        room::ExternalRoomEffect, EffectOutcome, SettledCompletion, SettledOutcome,
    },
};
use waddle_xmpp::mam::MamTxStoreOutcome;

pub(super) async fn execute(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalRoomEffect,
) -> EffectOutcome {
    match store(uow, decision, index, effect).await {
        Ok(settled) => EffectOutcome::Settled(settled),
        Err(error) => {
            tracing::warn!(%error, "deferred system archive failed");
            EffectOutcome::Unavailable
        }
    }
}

async fn store(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalRoomEffect,
) -> Result<SettledOutcome, IngressUowError> {
    let ExternalRoomEffect::ArchiveAfterPin {
        room,
        message,
        fence,
        ..
    } = effect
    else {
        return Err(IngressUowError::EffectIntentMessageMissing);
    };
    let mut tx = uow
        .begin_with_timeouts(
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(250),
        )
        .await?;
    let key = decision
        .message_key
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    if !CanonicalMessageRepository::lock(&mut tx, key).await? {
        return Err(IngressUowError::EffectIntentMessageMissing);
    }
    let recorded = EffectIntentRepository::load(&mut tx, key).await?;
    let archive_expectation =
        crate::ingress::archive_authority::expectation(key, &recorded, room, message);
    // Pin replanning can produce different human-readable text (for example
    // after a nickname change). The accepted system broadcast owns the archive
    // payload as well as its live copies; a fresh prototype is not authority.
    let source = recorded.iter().find_map(|intent| match intent {
        waddle_xmpp::ingress::IngressEffectIntent::RouteMucSystemBroadcast {
            room: saved,
            route_identity: waddle_xmpp::ingress::EffectMessageIdentity::StanzaId(id),
            system_message: Some(payload),
            ..
        } if saved == room && id.by == *room && id.id == message.id => Some(payload.message()),
        _ => None,
    });
    let restored = if let Some(source) = source {
        let archived_at = match &archive_expectation {
            waddle_xmpp::mam::ArchiveExpectation::Existing { archived_at, .. } => *archived_at,
            waddle_xmpp::mam::ArchiveExpectation::Fresh => message.timestamp,
        };
        Some(
            crate::server::routes::interpret::project_groupchat_archive(
                room,
                source,
                message.id.clone(),
                archived_at,
                Some(0),
                None,
            )
            .ok_or(IngressUowError::EffectIntentMessageMissing)?,
        )
    } else {
        None
    };
    let message = restored.as_ref().unwrap_or(message.as_ref());
    #[cfg(feature = "clustering")]
    let outcome = match fence {
        crate::server::routes::interpret::effects::room::RoomFenceRequirement::Guarded(context) => {
            if context.entity
                != waddle_xmpp::ownership::Entity::new(
                    waddle_xmpp::ownership::EntityType::RoomActor,
                    room.to_string(),
                )
            {
                return Err(IngressUowError::RoomGenerationStale);
            }
            let proof = crate::ingress_uow::ClaimRepository::assert_room_claim(
                &mut tx,
                room,
                &context.owner,
                context.epoch,
            )
            .await?;
            MamArchiveRepository::store_fenced(
                &mut tx,
                &proof,
                room,
                message,
                archive_expectation.clone(),
            )
            .await?
        }
        crate::server::routes::interpret::effects::room::RoomFenceRequirement::Unfenced => {
            if matches!(
                tx.fencing(),
                crate::ingress_uow::IngressFencing::Clustered(_)
            ) {
                return Err(IngressUowError::ClaimFenceMissing);
            }
            MamArchiveRepository::store(&mut tx, room, message, archive_expectation.clone()).await?
        }
    };
    #[cfg(not(feature = "clustering"))]
    let outcome = {
        let _ = fence;
        MamArchiveRepository::store(&mut tx, room, message, archive_expectation.clone()).await?
    };
    if matches!(
        outcome,
        MamTxStoreOutcome::TombstoneHit(_) | MamTxStoreOutcome::Expired(_)
    ) {
        // Dropping this transaction preserves the unresolved archive obligation.
        return Ok(SettledOutcome {
            refusal: None,
            persisted: Vec::new(),
            completion: SettledCompletion::Incomplete,
            detached: None,
        });
    }
    crate::ingress::archive_authority::finalize(&mut tx, key, &recorded, room, message, &outcome)
        .await?;
    let mut evidence = Vec::new();
    for intent in EffectIntentRepository::load(&mut tx, key).await? {
        if decision.external_receipts[index].contains(&crate::ingress::receipt_key(&intent)?) {
            evidence.push(intent);
        }
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
