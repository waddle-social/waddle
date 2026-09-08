//! Store a frozen system archive only after its authorizing room mutation.
use crate::{
    ingress_uow::{IngressUnitOfWork, IngressUowError, MamArchiveRepository},
    server::routes::interpret::effects::{room::ExternalRoomEffect, EffectOutcome},
};
use waddle_xmpp::mam::{MamTxStoreOutcome, StoreOutcome};

pub(super) async fn execute(uow: &IngressUnitOfWork, effect: &ExternalRoomEffect) -> EffectOutcome {
    match store(uow, effect).await {
        Ok(
            MamTxStoreOutcome::Inserted(id)
            | MamTxStoreOutcome::Existing(id)
            | MamTxStoreOutcome::Repaired(id),
        ) => EffectOutcome::Archive(Ok(StoreOutcome::Stored(id.id))),
        Ok(MamTxStoreOutcome::TombstoneHit(_) | MamTxStoreOutcome::Expired(_)) => {
            EffectOutcome::Unavailable
        }
        Err(error) => {
            tracing::warn!(%error, "deferred system archive failed");
            EffectOutcome::Unavailable
        }
    }
}

async fn store(
    uow: &IngressUnitOfWork,
    effect: &ExternalRoomEffect,
) -> Result<MamTxStoreOutcome, IngressUowError> {
    let ExternalRoomEffect::ArchiveAfterPin {
        room,
        message,
        fence,
        archive_expectation,
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
    tx.commit().await?;
    Ok(outcome)
}
