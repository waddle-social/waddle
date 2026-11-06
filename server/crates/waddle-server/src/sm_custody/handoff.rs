//! Unselected pre-start source contract and bounded ordered memory owner.
//! No production routing, transport, replay or endpoint publication uses it.

use std::collections::BTreeMap;

use jid::FullJid;
use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::SmIngressAppendKey;

use super::admission::encode_payload;
use super::repository::{commit, pg};
use super::types::db;
use super::{
    CommittedAdmission, CommittedSlot, CustodyCapture, CustodyError, CustodyOperation,
    PreparedDirectFrame, SessionGeneration, SmCustodyRepository,
};
use crate::ingress_uow::{IngressUowTransaction, SendLease};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffStage {
    Admission,
    Slot,
    SourceCompletion,
    Hydration,
}

/// Lookup identity, not proof that an uncertain transaction committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffRecovery {
    capture: CustodyCapture,
    operation: Uuid,
    source: SmIngressAppendKey,
    attempt: Uuid,
    ordinal: Option<u64>,
}

impl HandoffRecovery {
    /// Reconstruct caller-retained lookup evidence. It grants no custody or
    /// send authority; the complete identity must match durable state.
    pub fn new(
        capture: CustodyCapture,
        operation: Uuid,
        source: SmIngressAppendKey,
        attempt: Uuid,
        ordinal: Option<u64>,
    ) -> Result<Self, CustodyError> {
        if operation.is_nil()
            || attempt.is_nil()
            || source.resource != capture.resource
            || ordinal.is_some_and(|ordinal| ordinal == 0 || i64::try_from(ordinal).is_err())
        {
            return Err(CustodyError::Rejected);
        }
        Ok(Self {
            capture,
            operation,
            source,
            attempt,
            ordinal,
        })
    }

    pub fn operation_id(&self) -> Uuid {
        self.operation
    }
    pub fn session(&self) -> &SmSessionId {
        &self.capture.session
    }
    pub fn resource(&self) -> &FullJid {
        &self.capture.resource
    }
    pub fn generation(&self) -> SessionGeneration {
        self.capture.generation
    }
    pub fn capture(&self) -> &CustodyCapture {
        &self.capture
    }
    pub fn source(&self) -> &SmIngressAppendKey {
        &self.source
    }
    pub fn attempt(&self) -> Uuid {
        self.attempt
    }
    /// The absolute ordinal only if binding was positively acknowledged.
    pub fn ordinal(&self) -> Option<u64> {
        self.ordinal
    }

    pub(super) fn matches_frame(&self, frame: &PreparedDirectFrame) -> bool {
        self.operation == frame.operation_id
            && self.source == frame.key
            && self.attempt == frame.attempt
    }

    fn with_slot(&self, slot: &CommittedSlot) -> Self {
        let mut identity = self.clone();
        identity.ordinal = Some(slot.ordinal());
        identity
    }
}

#[derive(Debug, Error)]
#[error("candidate handoff failed; retained custody may require recovery")]
pub struct HandoffFailure {
    recovery: Box<HandoffRecovery>,
    stage: HandoffStage,
    #[source]
    cause: Box<CustodyError>,
}

impl HandoffFailure {
    pub fn recovery(&self) -> &HandoffRecovery {
        &self.recovery
    }
    pub fn stage(&self) -> HandoffStage {
        self.stage
    }
    pub fn cause(&self) -> &CustodyError {
        &self.cause
    }
}

/// Known committed payload, exact slot and source-handoff marker. This never
/// means transport exposure or client ACK. Fields cannot be caller-constructed.
#[derive(Debug, Clone)]
pub struct CommittedPreacceptHandoff {
    capture: CustodyCapture,
    frame: PreparedDirectFrame,
    slot: CommittedSlot,
}

impl CommittedPreacceptHandoff {
    pub fn recovery(&self) -> HandoffRecovery {
        recovery(&self.capture, &self.frame).with_slot(&self.slot)
    }
    pub fn ordinal(&self) -> u64 {
        self.slot.ordinal()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateHydration {
    Inserted,
    AlreadyHydrated,
}

/// Concrete owned candidate, never a published endpoint. Exclusive mutable
/// access spans hydration; no generic async enqueue closure is accepted.
pub struct CandidateSmMemory {
    capture: CustodyCapture,
    retired: bool,
    frames: BTreeMap<i64, MemoryEntry>,
    bytes: usize,
}

struct MemoryEntry {
    handoff: CommittedPreacceptHandoff,
    bytes: usize,
}

impl CandidateSmMemory {
    pub fn new(capture: CustodyCapture) -> Self {
        Self {
            capture,
            retired: false,
            frames: BTreeMap::new(),
            bytes: 0,
        }
    }
    pub fn retire(&mut self) {
        self.retired = true;
    }
    pub fn queued_count(&self) -> usize {
        self.frames.len()
    }
    pub fn accepted_ordinal(&self) -> Option<u64> {
        self.frames
            .keys()
            .copied()
            .zip(1_i64..)
            .take_while(|(actual, expected)| actual == expected)
            .last()
            .map(|(ordinal, _)| ordinal as u64)
    }
    pub fn queued_frame(&self) -> Option<&PreparedDirectFrame> {
        self.frames
            .values()
            .next()
            .map(|entry| &entry.handoff.frame)
    }
    pub fn frame_at(&self, ordinal: u64) -> Option<&PreparedDirectFrame> {
        i64::try_from(ordinal)
            .ok()
            .and_then(|ordinal| self.frames.get(&ordinal).map(|entry| &entry.handoff.frame))
    }

    pub(super) fn check_exposure(
        &self,
        capture: &CustodyCapture,
        slot: &CommittedSlot,
    ) -> Result<(), CustodyError> {
        if self.retired
            || &self.capture != capture
            || !self
                .frames
                .get(&slot.ordinal)
                .is_some_and(|entry| entry.handoff.slot == *slot)
        {
            return Err(CustodyError::Rejected);
        }
        Ok(())
    }

    fn check(&self, handoff: &CommittedPreacceptHandoff) -> Result<(), CustodyError> {
        if self.retired || self.capture != handoff.capture {
            return Err(CustodyError::Rejected);
        }
        if self.frames.values().any(|entry| {
            let existing = &entry.handoff;
            (existing.slot.ordinal == handoff.slot.ordinal
                || existing.frame.operation_id == handoff.frame.operation_id)
                && (existing.frame != handoff.frame || existing.slot != handoff.slot)
        }) {
            return Err(CustodyError::Conflict);
        }
        Ok(())
    }

    fn insert(
        &mut self,
        handoff: &CommittedPreacceptHandoff,
    ) -> Result<CandidateHydration, CustodyError> {
        self.check(handoff)?;
        if self.frames.contains_key(&handoff.slot.ordinal) {
            return Ok(CandidateHydration::AlreadyHydrated);
        }
        let bytes = encode_payload(&handoff.frame)?.len();
        if self.frames.len() >= 1024
            || self
                .bytes
                .checked_add(bytes)
                .is_none_or(|n| n > 16 * 1024 * 1024)
        {
            return Err(CustodyError::Capacity);
        }
        self.frames.insert(
            handoff.slot.ordinal,
            MemoryEntry {
                handoff: handoff.clone(),
                bytes,
            },
        );
        self.bytes += bytes;
        Ok(CandidateHydration::Inserted)
    }
}

/// A dropped/failed authority transaction must not publish a new memory entry.
/// Exclusive receiver access prevents observing it before this guard confirms.
struct ProvisionalHydration<'a> {
    receiver: &'a mut CandidateSmMemory,
    outcome: CandidateHydration,
    ordinal: i64,
    confirmed: bool,
}

impl<'a> ProvisionalHydration<'a> {
    fn new(
        receiver: &'a mut CandidateSmMemory,
        handoff: &CommittedPreacceptHandoff,
    ) -> Result<Self, CustodyError> {
        let outcome = receiver.insert(handoff)?;
        Ok(Self {
            receiver,
            outcome,
            ordinal: handoff.slot.ordinal,
            confirmed: false,
        })
    }

    fn confirm(mut self) -> CandidateHydration {
        self.confirmed = true;
        self.outcome
    }
}

impl Drop for ProvisionalHydration<'_> {
    fn drop(&mut self) {
        if !self.confirmed && self.outcome == CandidateHydration::Inserted {
            if let Some(entry) = self.receiver.frames.remove(&self.ordinal) {
                self.receiver.bytes -= entry.bytes;
            }
        }
    }
}

impl SmCustodyRepository {
    /// Capture lookup identity before starting a handoff that may be cancelled
    /// or return an unknown COMMIT outcome. This is not proof of admission.
    pub fn handoff_recovery(&self, frame: &PreparedDirectFrame) -> HandoffRecovery {
        recovery(&self.capture, frame)
    }

    /// Dark alternative BEFORE production start. Earlier auth transactions must
    /// already be closed. Each custody transaction ends before the next starts.
    pub async fn preaccept_local_direct(
        &self,
        frame: &PreparedDirectFrame,
        lease: &SendLease,
    ) -> Result<CommittedPreacceptHandoff, HandoffFailure> {
        let recovery = self.handoff_recovery(frame);
        let admission = self
            .admit(frame, lease)
            .await
            .map_err(|cause| handoff_failure(&recovery, HandoffStage::Admission, cause))?;
        let slot = self
            .bind(&admission)
            .await
            .map_err(|cause| handoff_failure(&recovery, HandoffStage::Slot, cause))?;
        self.complete_source_handoff(&admission, &slot)
            .await
            .map_err(|cause| {
                handoff_failure(
                    &recovery.with_slot(&slot),
                    HandoffStage::SourceCompletion,
                    cause,
                )
            })
    }

    /// Exact-ID retry under the original current capture. Admission/slot proof
    /// is required; a Completed attempt alone cannot mint a handoff. Successors
    /// use sealed inventory/transfer, never old-binding memory hydration.
    pub async fn resume_preaccept(
        &self,
        recovery: &HandoffRecovery,
    ) -> Result<CommittedPreacceptHandoff, HandoffFailure> {
        let admission = self
            .committed_admission(recovery)
            .await
            .map_err(|cause| handoff_failure(recovery, HandoffStage::Admission, cause))?;
        let slot = self
            .bind(&admission)
            .await
            .map_err(|cause| handoff_failure(recovery, HandoffStage::Slot, cause))?;
        self.complete_source_handoff(&admission, &slot)
            .await
            .map_err(|cause| {
                handoff_failure(
                    &recovery.with_slot(&slot),
                    HandoffStage::SourceCompletion,
                    cause,
                )
            })
    }

    async fn complete_source_handoff(
        &self,
        admission: &CommittedAdmission,
        slot: &CommittedSlot,
    ) -> Result<CommittedPreacceptHandoff, CustodyError> {
        self.check_admission(admission)?;
        let mut tx = self
            .guarded_operation(admission.operation, false, true, None)
            .await?;
        let saved = self.operation(&mut tx, admission.operation).await?;
        if saved.terminal.is_some()
            || saved.slot.as_ref() != Some(slot)
            || &slot.admission != admission
        {
            return Err(CustodyError::Rejected);
        }
        if !self
            .has_source_handoff(&mut tx, admission.operation)
            .await?
        {
            let q = &self.schema;
            // Bind the transition to the original immutable attempt identity.
            // It shares the marker's transaction and never completes a successor.
            let changed = sqlx::query(&format!("UPDATE {q}.ingress_send_attempts SET state = 2 WHERE message_key = $1 AND kind = $2 AND semantic_identity_hash = $3 AND recipient = $4 AND node_id = $5 AND node_incarnation = $6 AND lease_token = $7 AND state = 1"))
                .bind(saved.frame.key.message_key.to_storage()).bind(saved.frame.key.kind.to_storage())
                .bind(saved.frame.key.semantic_identity_hash.as_slice()).bind(saved.frame.key.resource.to_string())
                .bind(&self.capture.owner.node_id).bind(&self.capture.owner.node_epoch).bind(saved.frame.attempt.to_string())
                .execute(pg(&mut tx)?).await.map_err(db)?.rows_affected();
            if changed != 1 {
                return Err(CustodyError::Conflict);
            }
            sqlx::query(&format!(
                "INSERT INTO {q}.sm_custody_handoffs (operation_id) VALUES ($1)"
            ))
            .bind(admission.operation)
            .execute(pg(&mut tx)?)
            .await
            .map_err(db)?;
        }
        self.fresh_principal(&mut tx).await?;
        commit(tx).await?;
        Ok(CommittedPreacceptHandoff {
            capture: self.capture.clone(),
            frame: saved.frame,
            slot: slot.clone(),
        })
    }

    pub async fn hydrate_preaccepted(
        &self,
        receiver: &mut CandidateSmMemory,
        handoff: &CommittedPreacceptHandoff,
    ) -> Result<CandidateHydration, HandoffFailure> {
        let operation = handoff.frame.operation_id;
        let hydrate = async {
            receiver.check(handoff)?;
            if handoff.capture != self.capture {
                return Err(CustodyError::Rejected);
            }
            let mut tx = self.guarded_operation(operation, false, true, None).await?;
            let saved = self.operation(&mut tx, operation).await?;
            self.check_handoff(&saved, handoff)?;
            if !self.has_source_handoff(&mut tx, operation).await? {
                return Err(CustodyError::Rejected);
            }
            self.fresh_principal(&mut tx).await?;
            // Admission, slot and source marker already committed. This concrete
            // mutation assigns their ordinal rather than counting it again.
            let pending = ProvisionalHydration::new(receiver, handoff)?;
            commit(tx).await?;
            Ok(pending.confirm())
        };
        hydrate.await.map_err(|cause| HandoffFailure {
            recovery: Box::new(handoff.recovery()),
            stage: HandoffStage::Hydration,
            cause: Box::new(cause),
        })
    }

    fn check_handoff(
        &self,
        saved: &CustodyOperation,
        handoff: &CommittedPreacceptHandoff,
    ) -> Result<(), CustodyError> {
        if saved.terminal.is_some()
            || saved.frame != handoff.frame
            || saved.slot.as_ref() != Some(&handoff.slot)
        {
            return Err(CustodyError::Rejected);
        }
        Ok(())
    }

    pub(super) async fn has_source_handoff(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        operation: Uuid,
    ) -> Result<bool, CustodyError> {
        sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM {}.sm_custody_handoffs WHERE operation_id = $1)",
            self.schema
        ))
        .bind(operation)
        .fetch_one(pg(tx)?)
        .await
        .map_err(db)
    }
}

fn handoff_failure(
    recovery: &HandoffRecovery,
    stage: HandoffStage,
    cause: CustodyError,
) -> HandoffFailure {
    HandoffFailure {
        recovery: Box::new(recovery.clone()),
        stage,
        cause: Box::new(cause),
    }
}

fn recovery(capture: &CustodyCapture, frame: &PreparedDirectFrame) -> HandoffRecovery {
    HandoffRecovery {
        capture: capture.clone(),
        operation: frame.operation_id,
        source: frame.key.clone(),
        attempt: frame.attempt,
        ordinal: None,
    }
}
