use super::submission::IngressSubmission;
use crate::ingress_uow::{IngressUowError, IngressUowTransaction};

#[cfg(feature = "clustering")]
pub(super) type RoomProof<'a> = Option<crate::ingress_uow::RoomClaimFence<'a>>;
#[cfg(not(feature = "clustering"))]
pub(super) type RoomProof<'a> = std::marker::PhantomData<&'a ()>;

pub(super) async fn assert_room<'a>(
    tx: &mut IngressUowTransaction<'a>,
    submission: &IngressSubmission,
    validate_local: bool,
) -> Result<RoomProof<'a>, IngressUowError> {
    #[cfg(feature = "clustering")]
    {
        use super::identity::IngressStreamIdentity;
        use crate::server::routes::interpret::effects::{
            room::RoomFenceRequirement, RoomExecutionPath,
        };
        let mut proof = None;
        if let IngressStreamIdentity::Relayed {
            room, room_fence, ..
        } = &submission.identity
        {
            validate_room_context(room, room_fence)?;
            proof = Some(
                crate::ingress_uow::ClaimRepository::assert_room_claim(
                    tx,
                    room,
                    &room_fence.owner,
                    room_fence.epoch,
                )
                .await?,
            );
        }
        if !validate_local {
            return Ok(proof);
        }
        if let RoomExecutionPath::Local { room, fence, .. } = &submission.plan.room_execution {
            if matches!(&submission.identity, IngressStreamIdentity::Relayed { room: expected, .. } if expected != room)
            {
                return Err(IngressUowError::ClaimFenceMissing);
            }
            match fence {
                RoomFenceRequirement::Guarded(context) => {
                    validate_room_context(room, context)?;
                    proof = Some(
                        crate::ingress_uow::ClaimRepository::assert_room_claim(
                            tx,
                            room,
                            &context.owner,
                            context.epoch,
                        )
                        .await?,
                    );
                }
                RoomFenceRequirement::Unfenced
                    if matches!(
                        tx.fencing(),
                        crate::ingress_uow::IngressFencing::Clustered(_)
                    ) =>
                {
                    return Err(IngressUowError::ClaimFenceMissing)
                }
                RoomFenceRequirement::Unfenced => {}
            }
        }
        // Admission revisions currently exist only inside the actor. A claim
        // proves ownership, but cannot attest that snapshot's admission policy.
        if validate_local
            && matches!(
                submission.plan.room_execution,
                crate::server::routes::interpret::effects::RoomExecutionPath::Local { .. }
            )
        {
            return Err(IngressUowError::RoomGenerationStale);
        }
        Ok(proof)
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = tx;
        if validate_local
            && matches!(
                submission.plan.room_execution,
                crate::server::routes::interpret::effects::RoomExecutionPath::Local { .. }
            )
        {
            return Err(IngressUowError::RoomGenerationStale);
        }
        Ok(std::marker::PhantomData)
    }
}
#[cfg(feature = "clustering")]
fn validate_room_context(
    room: &jid::BareJid,
    context: &waddle_xmpp::muc::RoomClaimFenceContext,
) -> Result<(), IngressUowError> {
    if context.entity
        != waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::RoomActor,
            room.to_string(),
        )
    {
        return Err(IngressUowError::RoomGenerationStale);
    }
    Ok(())
}
