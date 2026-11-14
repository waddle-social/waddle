//! Pre-accept custody of outbound frames and acknowledgement settlement.
//!
//! A countable outbound stanza is admitted here before any memory queue may
//! accept it. Admission stores the full payload under the session's next
//! ordinal in the transaction that locks the session and compares the
//! owner's fence, claim and principal.

use chrono::{DateTime, Utc};
use thiserror::Error;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ownership::{SessionGeneration, SmSessionKey};
use waddle_xmpp::stream_management::durable::{
    check_owner_fence, AckKind, AppendError, DurableSmState, Exposure, FenceError, HandledDecision,
    HandledError, OutboundWindow, OwnerFence, ReplayRange, SmOperationId, SmOrdinal,
};
use waddle_xmpp::stream_management::persistence::{
    PersistedUnackedStanza, SmClaimFence, SmReplayPayload,
};
use waddle_xmpp::stream_management::{SmFrameProvenance, SmIngressFrameReceipt};

use super::error::DurableSmError;
use super::operations::{self, OperationKind, OperationOutcome};
use super::record::{self, DurableSessionRecord};
use super::store::DurableSmStore;
use crate::db::Transaction;
use crate::ingress_substrate::EnteredTransaction;
use crate::sm_persistence::durable_rows;

/// One countable outbound stanza offered for custody.
#[derive(Debug, Clone)]
pub struct AdmissionFrame {
    /// Caller-minted identity. Offering the same operation again returns the
    /// ordinal it was first admitted under.
    pub operation: SmOperationId,
    /// The stanza and the exact bytes that will be written and replayed.
    pub payload: SmReplayPayload,
    pub original_receipt_at: DateTime<Utc>,
    pub ingress_receipts: Vec<SmIngressFrameReceipt>,
    pub provenance: SmFrameProvenance,
}

/// On whose authority a frame is admitted.
#[derive(Debug, Clone, Copy)]
pub enum AdmissionAuthority<'a> {
    /// The binding that owns the current generation, with the principal its
    /// connection is authenticated as.
    Owner {
        fence: &'a OwnerFence,
        principal: &'a AuthenticatedPrincipalRef,
    },
    /// A delivery to a session no connection writes to: one that is
    /// `Detached`, or one whose resume committed but has not completed
    /// (`TransferHydrating`). The generation must be the one read with the
    /// row, so a delivery routed before a resume never lands after it.
    Detached {
        session: SmSessionKey,
        generation: SessionGeneration,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct AdmissionRequest<'a> {
    pub authority: AdmissionAuthority<'a>,
    /// This node's claim on the session; required on a clustered store.
    pub claim: Option<&'a SmClaimFence>,
    pub now: DateTime<Utc>,
}

/// Proof that a frame's payload and ordinal committed. A memory queue
/// accepts a frame only against this value.
#[must_use = "the frame is in durable custody; the witness carries its ordinal and exposure"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedObligation {
    session: SmSessionKey,
    generation: SessionGeneration,
    operation: SmOperationId,
    ordinal: SmOrdinal,
    exposure: Exposure,
}

impl CommittedObligation {
    pub fn session(&self) -> SmSessionKey {
        self.session
    }

    /// The generation the frame was admitted under.
    pub fn generation(&self) -> SessionGeneration {
        self.generation
    }

    pub fn operation(&self) -> SmOperationId {
        self.operation
    }

    pub fn ordinal(&self) -> SmOrdinal {
        self.ordinal
    }

    /// The XEP-0198 outbound counter value of this frame.
    pub fn sequence(&self) -> u32 {
        self.ordinal.wire()
    }

    /// `Exposed` frames may be written to the owner's transport now.
    /// `Retained` frames wait for a resume to replay them.
    pub fn exposure(&self) -> Exposure {
        self.exposure
    }
}

/// A known outcome: the frame is not in custody.
#[derive(Debug, Error)]
pub enum AdmissionRejection {
    #[error("no such durable SM session")]
    UnknownSession,
    #[error(transparent)]
    Fence(#[from] FenceError),
    #[error("a session in {0:?} does not admit frames on this authority")]
    NotAdmitting(DurableSmState),
    #[error("the connection's principal is not the session's stored principal")]
    PrincipalMismatch,
    #[error(transparent)]
    Bound(#[from] AppendError),
    #[error("this node does not hold the session claim it named")]
    ClaimFenceMissing,
    #[error("the operation id already belongs to another operation or session")]
    OperationReused,
    #[error(transparent)]
    Store(#[from] DurableSmError),
}

#[derive(Debug, Error)]
pub enum AdmissionError {
    #[error(transparent)]
    Rejected(#[from] AdmissionRejection),
    /// The commit outcome is unknown. Offer the same frame again: admission
    /// is idempotent under its operation id.
    #[error("the admission commit outcome is unknown")]
    Indeterminate(#[source] DurableSmError),
}

/// The settled outbound window after an acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckSettlement {
    pub decision: HandledDecision,
    pub window: OutboundWindow,
}

/// A known outcome: the acknowledgement changed nothing.
#[derive(Debug, Error)]
pub enum OwnerRejection {
    #[error("no such durable SM session")]
    UnknownSession,
    #[error(transparent)]
    Fence(#[from] FenceError),
    #[error("a session in {0:?} has no owner")]
    NotOwned(DurableSmState),
    #[error("this node does not hold the session claim it named")]
    ClaimFenceMissing,
    /// The peer's handled count is invalid for what was exposed to it. The
    /// stream is in error; nothing was released.
    #[error(transparent)]
    Handled(#[from] HandledError),
    #[error(transparent)]
    Store(#[from] DurableSmError),
}

#[derive(Debug, Error)]
pub enum OwnerError {
    #[error(transparent)]
    Rejected(#[from] OwnerRejection),
    /// The commit outcome is unknown. The operation is idempotent under the
    /// same fence and may be repeated.
    #[error("the commit outcome is unknown")]
    Indeterminate(#[source] DurableSmError),
}

/// A retained frame with its absolute position.
#[derive(Debug, Clone)]
pub struct RetainedFrame {
    pub ordinal: SmOrdinal,
    pub frame: PersistedUnackedStanza,
}

impl DurableSmStore {
    /// Commit `frame` into the session's custody and return its ordinal.
    ///
    /// The session row is locked, then the authority is compared: an owner
    /// names its exact generation and binding and the stored principal; a
    /// detached delivery names the generation it was routed against. A
    /// `Live` owner's frame raises the exposure ceiling in the same commit,
    /// so the ceiling is durable before the frame can reach a transport.
    pub async fn admit(
        &self,
        request: AdmissionRequest<'_>,
        frame: &AdmissionFrame,
    ) -> Result<CommittedObligation, AdmissionError> {
        let session = match request.authority {
            AdmissionAuthority::Owner { fence, .. } => fence.session,
            AdmissionAuthority::Detached { session, .. } => session,
        };
        let claim = self
            .write_claim(request.claim)
            .map_err(AdmissionRejection::from)?;
        let mut tx = self.begin_write().await.map_err(AdmissionRejection::from)?;
        if let Some(claim) = claim {
            if !self
                .claim_is_held(&mut tx, session, claim)
                .await
                .map_err(AdmissionRejection::from)?
            {
                return Err(AdmissionRejection::ClaimFenceMissing.into());
            }
        }
        let mut stored = record::load_for_update(&mut tx, session)
            .await
            .map_err(AdmissionRejection::from)?
            .ok_or(AdmissionRejection::UnknownSession)?;
        let exposure = admission_exposure(&stored, request.authority)?;

        if let Some(existing) = operations::find(&mut tx, frame.operation)
            .await
            .map_err(AdmissionRejection::from)?
        {
            let committed = match (existing.kind, existing.outcome, existing.ordinal) {
                (OperationKind::Admission, OperationOutcome::Committed, Some(ordinal))
                    if existing.session == session =>
                {
                    CommittedObligation {
                        session,
                        generation: existing.generation,
                        operation: frame.operation,
                        ordinal,
                        exposure: if ordinal <= stored.window.exposure_ceiling() {
                            Exposure::Exposed
                        } else {
                            Exposure::Retained
                        },
                    }
                }
                _ => return Err(AdmissionRejection::OperationReused.into()),
            };
            tx.rollback()
                .await
                .map_err(|error| AdmissionRejection::Store(error.into()))?;
            return Ok(committed);
        }

        let frame_bytes = frame.payload.wire().len() as u64;
        let (window, ordinal) = stored
            .window
            .append(exposure, self.bounds, stored.unacked_bytes, frame_bytes)
            .map_err(AdmissionRejection::from)?;
        durable_rows::append_unacked(
            &mut tx,
            &PersistedUnackedStanza {
                provenance: frame.provenance.clone(),
                ingress_receipts: frame.ingress_receipts.clone(),
                stream_id: record::stream_id(session),
                sequence: ordinal.wire(),
                stanza: frame.payload.clone(),
                original_receipt_at: frame.original_receipt_at,
            },
        )
        .await
        .map_err(|error| AdmissionRejection::Store(error.into()))?;
        stored.window = window;
        stored.unacked_bytes += frame_bytes;
        stored.last_operation = frame.operation;
        record::update(&mut tx, &stored, request.now)
            .await
            .map_err(AdmissionRejection::from)?;
        operations::record_admission(
            &mut tx,
            frame.operation,
            session,
            stored.generation,
            ordinal,
            request.now,
        )
        .await
        .map_err(AdmissionRejection::from)?;
        tx.commit()
            .await
            .map_err(|error| AdmissionError::Indeterminate(error.into()))?;
        Ok(CommittedObligation {
            session,
            generation: stored.generation,
            operation: frame.operation,
            ordinal,
            exposure,
        })
    }

    /// Settle a live `<a h=…/>` from the owner's transport.
    ///
    /// The count is validated against the durable exposure ceiling, so a
    /// peer cannot acknowledge a frame that was never exposed to it. An
    /// advance deletes exactly the acknowledged frames and their admission
    /// outcomes. Any rejection leaves the session unchanged.
    pub async fn settle_ack(
        &self,
        fence: &OwnerFence,
        claim: Option<&SmClaimFence>,
        h: u32,
        now: DateTime<Utc>,
    ) -> Result<AckSettlement, OwnerError> {
        let (mut tx, mut stored) = self.lock_owned(fence, claim).await?;
        let (window, decision) = stored
            .window
            .acknowledge(h, AckKind::Live)
            .map_err(OwnerRejection::from)?;
        let HandledDecision::Advanced { through, .. } = decision else {
            tx.rollback()
                .await
                .map_err(|error| OwnerRejection::Store(error.into()))?;
            return Ok(AckSettlement { decision, window });
        };
        release_acknowledged(&mut tx, &mut stored, window, through)
            .await
            .map_err(OwnerRejection::from)?;
        record::update(&mut tx, &stored, now)
            .await
            .map_err(OwnerRejection::from)?;
        tx.commit()
            .await
            .map_err(|error| OwnerError::Indeterminate(error.into()))?;
        Ok(AckSettlement { decision, window })
    }

    /// Record the owner's handled count of client stanzas. The stored value
    /// never decreases.
    pub async fn record_inbound_handled(
        &self,
        fence: &OwnerFence,
        claim: Option<&SmClaimFence>,
        handled: u64,
        now: DateTime<Utc>,
    ) -> Result<(), OwnerError> {
        let (mut tx, mut stored) = self.lock_owned(fence, claim).await?;
        if handled <= stored.inbound_handled {
            tx.rollback()
                .await
                .map_err(|error| OwnerRejection::Store(error.into()))?;
            return Ok(());
        }
        stored.inbound_handled = handled;
        record::update(&mut tx, &stored, now)
            .await
            .map_err(OwnerRejection::from)?;
        tx.commit()
            .await
            .map_err(|error| OwnerError::Indeterminate(error.into()))
    }

    /// Every retained frame of a session in ordinal order: the order a
    /// replay writes them, ahead of any frame admitted later.
    pub async fn retained_frames(
        &self,
        session: SmSessionKey,
    ) -> Result<Vec<RetainedFrame>, DurableSmError> {
        let mut tx = self.db.begin().await?;
        let frames = match record::load(&mut tx, session).await? {
            Some(stored) => load_frames(&mut tx, session, stored.window.replay()).await?,
            None => Vec::new(),
        };
        tx.rollback().await?;
        Ok(frames)
    }

    /// Begin a write transaction, assert the claim, lock the session and
    /// compare the owner fence. Returns the open transaction and the record.
    pub(super) async fn lock_owned(
        &self,
        fence: &OwnerFence,
        claim: Option<&SmClaimFence>,
    ) -> Result<(EnteredTransaction<'_>, DurableSessionRecord), OwnerRejection> {
        let claim = self.write_claim(claim)?;
        let mut tx = self.begin_write().await?;
        if let Some(claim) = claim {
            if !self.claim_is_held(&mut tx, fence.session, claim).await? {
                return Err(OwnerRejection::ClaimFenceMissing);
            }
        }
        let stored = record::load_for_update(&mut tx, fence.session)
            .await?
            .ok_or(OwnerRejection::UnknownSession)?;
        check_owner_fence(&stored.fence(), fence)?;
        if !stored.state.has_owner() {
            return Err(OwnerRejection::NotOwned(stored.state));
        }
        Ok((tx, stored))
    }
}

fn admission_exposure(
    stored: &DurableSessionRecord,
    authority: AdmissionAuthority<'_>,
) -> Result<Exposure, AdmissionRejection> {
    match authority {
        AdmissionAuthority::Owner { fence, principal } => {
            check_owner_fence(&stored.fence(), fence)?;
            if *principal != stored.principal {
                return Err(AdmissionRejection::PrincipalMismatch);
            }
            match stored.state {
                DurableSmState::Live => Ok(Exposure::Exposed),
                // The replay has not been written yet; a frame exposed now
                // would overtake it.
                DurableSmState::TransferHydrating => Ok(Exposure::Retained),
                state => Err(AdmissionRejection::NotAdmitting(state)),
            }
        }
        AdmissionAuthority::Detached { generation, .. } => {
            if generation < stored.generation {
                return Err(FenceError::StaleGeneration {
                    stored: stored.generation,
                    presented: generation,
                }
                .into());
            }
            if generation > stored.generation {
                return Err(FenceError::UnknownGeneration {
                    stored: stored.generation,
                    presented: generation,
                }
                .into());
            }
            match stored.state {
                // A hydrating session has written nothing since it detached:
                // completing its resume returns this frame in the tail that
                // is written after the replay and before any live frame.
                DurableSmState::Detached | DurableSmState::TransferHydrating => {
                    Ok(Exposure::Retained)
                }
                state => Err(AdmissionRejection::NotAdmitting(state)),
            }
        }
    }
}

/// Delete the frames a handled count released and move the record's window.
pub(super) async fn release_acknowledged(
    tx: &mut Transaction<'_>,
    stored: &mut DurableSessionRecord,
    window: OutboundWindow,
    through: SmOrdinal,
) -> Result<(), DurableSmError> {
    let stream = record::stream_id(stored.session);
    durable_rows::delete_unacked_through(tx, &stream, stored.window.acked().wire(), through.wire())
        .await?;
    operations::prune_admissions_through(tx, stored.session, through).await?;
    stored.window = window;
    stored.unacked_bytes = durable_rows::retained_payload_bytes(tx, &stream).await?;
    Ok(())
}

/// Load the frames of `range` in ordinal order, requiring one frame for
/// every ordinal: a missing or foreign row means the custody is damaged.
pub(super) async fn load_frames(
    tx: &mut Transaction<'_>,
    session: SmSessionKey,
    range: ReplayRange,
) -> Result<Vec<RetainedFrame>, DurableSmError> {
    let stored = durable_rows::load_unacked(tx, &record::stream_id(session)).await?;
    let found = stored.len() as u64;
    let mut frames = stored
        .into_iter()
        .filter_map(|frame| {
            range
                .ordinal_for_wire(frame.sequence)
                .map(|ordinal| RetainedFrame { ordinal, frame })
        })
        .collect::<Vec<_>>();
    frames.sort_by_key(|frame| frame.ordinal);
    let dense = frames
        .iter()
        .map(|frame| frame.ordinal)
        .eq(range.ordinals());
    if !dense || found != range.len() {
        return Err(DurableSmError::ReplayGap {
            expected: range.len(),
            found,
        });
    }
    Ok(frames)
}
