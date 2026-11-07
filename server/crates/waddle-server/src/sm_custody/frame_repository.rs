//! Fixture-only PostgreSQL adapter for the production-compiled retained-frame
//! model. This is not selected by startup and provides no send/settlement API.
//! The disposable role has raw DML: arbitrary multi-statement writers can
//! invert lock order and are outside this experiment's authority boundary.
//! An admitted attempt is not rechecked for current lease token or state on
//! each new occurrence. That recheck is a prerequisite for any serving path.

use sqlx::Row;
use waddle_xmpp::stream_management::persistence::SmReplayPayload;

use super::repository::{commit, pg};
use super::types::db;
use super::{
    CommittedAdmission, CustodyCapture, CustodyError, CustodyRecoveryAuthority, SealedSession,
    SmCustodyRepository,
};
use crate::ingress_uow::IngressUowTransaction;
use crate::sm_frame_occurrence::{FrameCapture, FrameCommitEvidence, RetainedFrameOccurrence};

impl From<&CustodyCapture> for FrameCapture {
    fn from(capture: &CustodyCapture) -> Self {
        Self {
            gate_id: capture.gate_id,
            token: capture.token,
            resource: capture.resource.clone(),
            binding: capture.binding.clone(),
            session: capture.session.clone(),
            generation: capture.generation.get(),
            principal: capture.principal.clone(),
            owner: capture.owner.clone(),
            claim: capture.claim,
        }
    }
}

impl SmCustodyRepository {
    /// Retain one exact occurrence under a previously admitted direct-message
    /// obligation. The ordinal and frame are written together, then read back
    /// in that same transaction. A failed or cancelled call leaves the
    /// caller-owned occurrence unchanged and unknown, including its UUID.
    pub async fn persist_frame_occurrence(
        &self,
        admission: &CommittedAdmission,
        frame: &mut RetainedFrameOccurrence,
    ) -> Result<(), CustodyError> {
        frame.mark_unknown();
        self.check_frame_identity(admission, frame)?;
        let mut tx = self
            .guarded_operation(admission.operation, false, true, None)
            .await?;
        self.check_frame_source(&mut tx, admission, frame, false)
            .await?;
        let ordinal = match self.exact_occurrence(&mut tx, admission, frame).await? {
            Some(ordinal) => ordinal,
            None => {
                // A historical exact retry survives later receipts. A new
                // occurrence still requires an unresolved source.
                self.check_frame_source(&mut tx, admission, frame, true)
                    .await?;
                let q = &self.schema;
                sqlx::query(&format!(
                    "SELECT id FROM {q}.sm_custody_quota WHERE id = 1 FOR UPDATE"
                ))
                .fetch_one(pg(&mut tx)?)
                .await
                .map_err(db)?;
                let (rows, bytes): (i64, i64) = sqlx::query_as(&format!("SELECT count(*), COALESCE(sum(octet_length(payload)), 0)::bigint FROM {q}.sm_custody_frame_occurrences"))
                    .fetch_one(pg(&mut tx)?).await.map_err(db)?;
                if rows >= self.limits.rows
                    || bytes
                        .checked_add(frame.payload().wire().len() as i64)
                        .is_none_or(|total| total > self.limits.bytes)
                {
                    return Err(CustodyError::Capacity);
                }
                // The gate is already locked. Its old frontier supplies only
                // the starting point for this independent experiment.
                sqlx::query(&format!("INSERT INTO {q}.sm_custody_frame_frontiers (gate_id, appended) SELECT gate_id, appended FROM {q}.sm_custody_gates WHERE gate_id = $1 ON CONFLICT DO NOTHING"))
                    .bind(admission.gate).execute(pg(&mut tx)?).await.map_err(db)?;
                let appended: i64 = sqlx::query_scalar(&format!("SELECT appended FROM {q}.sm_custody_frame_frontiers WHERE gate_id = $1 FOR UPDATE"))
                    .bind(admission.gate).fetch_one(pg(&mut tx)?).await.map_err(db)?;
                let next = appended.checked_add(1).ok_or(CustodyError::Overflow)?;
                sqlx::query(&format!(
                    "UPDATE {q}.sm_custody_frame_frontiers SET appended = $2 WHERE gate_id = $1"
                ))
                .bind(admission.gate)
                .bind(next)
                .execute(pg(&mut tx)?)
                .await
                .map_err(db)?;
                sqlx::query(&format!("INSERT INTO {q}.sm_custody_frame_occurrences (occurrence_id, operation_id, gate_id, ordinal, payload, provenance, received_at) VALUES ($1,$2,$3,$4,$5,$6,$7)"))
                    .bind(frame.occurrence_id()).bind(admission.operation).bind(admission.gate)
                    .bind(next).bind(frame.payload().wire().as_bytes()).bind(frame.provenance().encode())
                    .bind(frame.received_at()).execute(pg(&mut tx)?).await.map_err(db)?;
                // Positive comparison is deliberately on the writer's own
                // physical transaction, before its COMMIT can be ambiguous.
                match self.exact_occurrence(&mut tx, admission, frame).await? {
                    Some(observed) if observed == next => observed,
                    _ => return Err(CustodyError::Conflict),
                }
            }
        };
        self.fresh_principal(&mut tx).await?;
        commit(tx).await?;
        let evidence =
            FrameCommitEvidence::verified(frame, ordinal).ok_or(CustodyError::Conflict)?;
        if !frame.confirm(&evidence) {
            return Err(CustodyError::Conflict);
        }
        Ok(())
    }

    /// Only exact positive readback resolves an uncertain historical commit.
    /// Absence, an unavailable database or stale authority leaves it unknown.
    pub async fn reconcile_frame_occurrence(
        &self,
        admission: &CommittedAdmission,
        frame: &mut RetainedFrameOccurrence,
    ) -> Result<(), CustodyError> {
        frame.mark_unknown();
        self.check_frame_identity(admission, frame)?;
        let mut tx = self
            .guarded_operation(admission.operation, true, false, None)
            .await?;
        self.check_frame_source(&mut tx, admission, frame, false)
            .await?;
        let ordinal = self
            .exact_occurrence(&mut tx, admission, frame)
            .await?
            .ok_or(CustodyError::CommitUnknown)?;
        self.fresh_principal(&mut tx).await?;
        commit(tx).await?;
        let evidence =
            FrameCommitEvidence::verified(frame, ordinal).ok_or(CustodyError::Conflict)?;
        if !frame.confirm(&evidence) {
            return Err(CustodyError::Conflict);
        }
        Ok(())
    }

    /// Resolve caller-retained evidence under a successor's exact current
    /// claim and an irreversibly sealed old gate. This does not revive the old
    /// principal or grant a send, settlement, or absence capability.
    pub async fn reconcile_frame_occurrence_recovered(
        &self,
        sealed: &SealedSession,
        authority: &CustodyRecoveryAuthority,
        admission: &CommittedAdmission,
        frame: &mut RetainedFrameOccurrence,
    ) -> Result<(), CustodyError> {
        frame.mark_unknown();
        self.check_frame_identity(admission, frame)?;
        if sealed.gate != self.capture.gate_id || sealed.token != self.capture.token {
            return Err(CustodyError::Rejected);
        }
        let mut tx = self
            .guarded_operation(admission.operation, true, false, Some(authority))
            .await?;
        self.check_frame_source(&mut tx, admission, frame, false)
            .await?;
        let ordinal = self
            .exact_occurrence(&mut tx, admission, frame)
            .await?
            .ok_or(CustodyError::CommitUnknown)?;
        commit(tx).await?;
        let evidence =
            FrameCommitEvidence::verified(frame, ordinal).ok_or(CustodyError::Conflict)?;
        if !frame.confirm(&evidence) {
            return Err(CustodyError::Conflict);
        }
        Ok(())
    }

    fn check_frame_identity(
        &self,
        admission: &CommittedAdmission,
        frame: &RetainedFrameOccurrence,
    ) -> Result<(), CustodyError> {
        self.check_admission(admission)?;
        if frame.capture() != &FrameCapture::from(&self.capture)
            || frame.obligation_id() != admission.operation
            || frame.occurrence_id().is_nil()
            || frame.payload().wire().is_empty()
            || frame.payload().wire().len() > 1024 * 1024
            || !frame
                .received_at()
                .timestamp_subsec_nanos()
                .is_multiple_of(1000)
        {
            return Err(CustodyError::Rejected);
        }
        Ok(())
    }

    async fn check_frame_source(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        admission: &CommittedAdmission,
        frame: &RetainedFrameOccurrence,
        require_unresolved: bool,
    ) -> Result<(), CustodyError> {
        let saved = self.operation(tx, admission.operation).await?;
        self.validate_canonical(tx, &saved.frame, require_unresolved)
            .await?;
        if saved.terminal.is_some()
            || saved.slot.is_some()
            || frame.provenance().claims() != Some(std::slice::from_ref(&saved.frame.key))
            || frame.received_at() != saved.frame.received_at
            || SmReplayPayload::from_wire_matching(
                frame.payload().wire().to_owned(),
                &waddle_xmpp::Stanza::Message(saved.frame.payload.clone()),
            )
            .is_err()
        {
            return Err(CustodyError::Rejected);
        }
        Ok(())
    }

    async fn exact_occurrence(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        admission: &CommittedAdmission,
        frame: &RetainedFrameOccurrence,
    ) -> Result<Option<i64>, CustodyError> {
        let row = sqlx::query(&format!("SELECT operation_id, gate_id, ordinal, payload, provenance, received_at FROM {}.sm_custody_frame_occurrences WHERE occurrence_id = $1", self.schema))
            .bind(frame.occurrence_id()).fetch_optional(pg(tx)?).await.map_err(db)?;
        let Some(row) = row else { return Ok(None) };
        let ordinal: i64 = row.try_get("ordinal").map_err(db)?;
        if row.try_get::<uuid::Uuid, _>("operation_id").map_err(db)? != admission.operation
            || row.try_get::<uuid::Uuid, _>("gate_id").map_err(db)? != admission.gate
            || row.try_get::<Vec<u8>, _>("payload").map_err(db)?
                != frame.payload().wire().as_bytes()
            || row.try_get::<Vec<u8>, _>("provenance").map_err(db)? != frame.provenance().encode()
            || row
                .try_get::<chrono::DateTime<chrono::Utc>, _>("received_at")
                .map_err(db)?
                != frame.received_at()
            || ordinal <= 0
        {
            return Err(CustodyError::Conflict);
        }
        Ok(Some(ordinal))
    }
}
