use super::identity::IngressStreamIdentity;
use crate::ingress_substrate::{FrontierOutcome, MessageWriteOutcome};
use crate::ingress_uow::{
    IngressFencing, IngressUowError, IngressUowTransaction, SmIngressRepository,
    SmIngressStreamRepository,
};
use waddle_xmpp::ingress::{IngressOrdinal, MessageKey, SmIngressId, WireHandledCount};

pub(super) struct StreamAttempt {
    pub id: SmIngressId,
    pub ordinal: IngressOrdinal,
    pub bound: Option<(MessageKey, IngressOrdinal)>,
    wire: WireHandledCount,
    checkpoint: WireHandledCount,
}
pub(super) async fn lock_stream(
    tx: &mut IngressUowTransaction<'_>,
    identity: &IngressStreamIdentity,
) -> Result<Option<StreamAttempt>, IngressUowError> {
    let IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        reserved_wire_position,
        checkpoint_h,
        ..
    } = identity
    else {
        return Ok(None);
    };
    #[cfg(feature = "clustering")]
    let fence = if matches!(tx.fencing(), IngressFencing::Clustered(_)) {
        let IngressStreamIdentity::Resumable {
            owner, claim_epoch, ..
        } = identity
        else {
            unreachable!()
        };
        Some(
            crate::ingress_uow::ClaimRepository::assert_sm_claim(
                tx,
                stream_id,
                owner,
                *claim_epoch,
            )
            .await?,
        )
    } else {
        None
    };
    let locked = match tx.fencing() {
        IngressFencing::SingleNode => {
            SmIngressStreamRepository::lock_single_node(tx, stream_id).await?
        }
        #[cfg(feature = "clustering")]
        IngressFencing::Clustered(_) => {
            SmIngressStreamRepository::lock(
                tx,
                fence.as_ref().ok_or(IngressUowError::ClaimFenceMissing)?,
                stream_id,
            )
            .await?
        }
    };
    let (id, handled) = match locked {
        Some(locked) => locked,
        None => {
            SmIngressStreamRepository::mint_reserved(tx, stream_id, *sm_ingress_id).await?;
            (*sm_ingress_id, 0)
        }
    };
    if id != *sm_ingress_id {
        return Err(IngressUowError::InvalidStoredSmIngressId);
    }
    let bound = SmIngressRepository::lookup_wire_binding(tx, id, *reserved_wire_position).await?;
    let ordinal = match bound {
        Some((_, ordinal)) => ordinal,
        None => IngressOrdinal::from_storage(
            handled
                .checked_add(1)
                .ok_or(IngressUowError::InvalidStoredFrontier)?,
        )
        .map_err(|_| IngressUowError::InvalidStoredFrontier)?,
    };
    Ok(Some(StreamAttempt {
        id,
        ordinal,
        bound,
        wire: *reserved_wire_position,
        checkpoint: *checkpoint_h,
    }))
}
pub(super) async fn finish_stream(
    tx: &mut IngressUowTransaction<'_>,
    stream: Option<&StreamAttempt>,
    key: MessageKey,
) -> Result<(), IngressUowError> {
    let Some(stream) = stream else {
        return Ok(());
    };
    if matches!(
        SmIngressRepository::insert_sm_ref(tx, stream.id, stream.ordinal, stream.wire, key).await?,
        MessageWriteOutcome::MessageVanished
    ) {
        return Err(IngressUowError::EffectIntentMessageMissing);
    }
    if stream.bound.is_some() {
        SmIngressStreamRepository::flush_checkpoint(tx, stream.id, stream.checkpoint).await?;
    } else if matches!(
        SmIngressStreamRepository::advance_frontier(
            tx,
            stream.id,
            stream.ordinal,
            stream.checkpoint
        )
        .await?,
        FrontierOutcome::Stale { .. }
    ) {
        return Err(IngressUowError::IngressFrontierStale);
    }
    Ok(())
}
