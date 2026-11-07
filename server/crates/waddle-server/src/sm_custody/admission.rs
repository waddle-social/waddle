use minidom::Element;
use sqlx::Row;
use waddle_xmpp::ingress::IngressEffectIntent;

use super::repository::{commit, pg};
use super::types::{db, ACK_HALF_RANGE};
use super::{
    CandidateSmMemory, CommittedAdmission, CommittedSlot, CustodyError, HandoffRecovery,
    PreparedDirectFrame, SmCustodyRepository,
};
use crate::ingress_substrate::{EnvelopeVersion, MessageEnvelope};
use crate::ingress_uow::{start_fresh_postgres, IngressUowTransaction, SendLease};

impl SmCustodyRepository {
    /// Positive readback of an exact caller-retained immutable identity.
    /// This cannot infer rollback or mint a capability for an absent operation.
    pub async fn committed_admission(
        &self,
        recovery: &HandoffRecovery,
    ) -> Result<CommittedAdmission, CustodyError> {
        if recovery.capture() != &self.capture {
            return Err(CustodyError::Rejected);
        }
        let mut tx = self
            .guarded_operation(recovery.operation_id(), false, false, None)
            .await?;
        let saved = self.operation(&mut tx, recovery.operation_id()).await?;
        if !recovery.matches_frame(&saved.frame)
            || recovery.ordinal().is_some_and(|expected| {
                saved.slot.as_ref().map(CommittedSlot::ordinal) != Some(expected)
            })
        {
            return Err(CustodyError::Conflict);
        }
        if saved.terminal.is_some() {
            return Err(CustodyError::Rejected);
        }
        commit(tx).await?;
        Ok(CommittedAdmission {
            gate: self.capture.gate_id,
            operation: recovery.operation_id(),
        })
    }

    /// Atomically commit exact full-payload admission and start its real send
    /// attempt. Unknown commit returns no capability and never authorizes enqueue.
    pub async fn admit(
        &self,
        frame: &PreparedDirectFrame,
        lease: &SendLease,
    ) -> Result<CommittedAdmission, CustodyError> {
        let checked = PreparedDirectFrame::for_lease(
            frame.operation_id,
            frame.key.clone(),
            frame.payload.clone(),
            frame.received_at,
            lease,
        )?;
        if &checked != frame
            || lease.owner() != &self.capture.owner
            || frame.key.resource != self.capture.resource
        {
            return Err(CustodyError::Rejected);
        }
        let payload = encode_payload(frame)?;
        let mut tx = self
            .guarded_source(false, true, frame.key.message_key)
            .await?;
        let q = &self.schema;
        let prior: Vec<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(&format!(
            "SELECT operation_id, gate_id FROM {q}.sm_custody_operations WHERE operation_id = $1 OR attempt_token = $2 OR (message_key = $3 AND receipt_kind = $4 AND semantic_identity_hash = $5 AND resource = $6)"
        ))
        .bind(frame.operation_id).bind(frame.attempt).bind(frame.key.message_key.to_storage())
        .bind(frame.key.kind.to_storage()).bind(frame.key.semantic_identity_hash.as_slice())
        .bind(frame.key.resource.to_string()).fetch_all(pg(&mut tx)?).await.map_err(db)?;
        if !prior.is_empty() {
            if prior.len() != 1 || prior[0] != (frame.operation_id, self.capture.gate_id) {
                return Err(CustodyError::Conflict);
            }
            let id = prior[0].0;
            let saved = self.operation(&mut tx, id).await?;
            if saved.frame != *frame || saved.terminal.is_some() {
                return Err(CustodyError::Conflict);
            }
            commit(tx).await?;
            return Ok(CommittedAdmission {
                gate: self.capture.gate_id,
                operation: id,
            });
        }
        self.validate_canonical(&mut tx, frame, true).await?;
        sqlx::query(&format!("SELECT 1 FROM {q}.ingress_send_attempts WHERE message_key = $1 AND kind = $2 AND semantic_identity_hash = $3 AND recipient = $4 FOR UPDATE"))
            .bind(frame.key.message_key.to_storage()).bind(frame.key.kind.to_storage())
            .bind(frame.key.semantic_identity_hash.as_slice()).bind(frame.key.resource.to_string())
            .fetch_optional(pg(&mut tx)?).await.map_err(db)?.ok_or(CustodyError::Rejected)?;
        sqlx::query(&format!(
            "SELECT id FROM {q}.sm_custody_quota WHERE id = 1 FOR UPDATE"
        ))
        .fetch_one(pg(&mut tx)?)
        .await
        .map_err(db)?;
        let (rows, bytes): (i64,i64) = sqlx::query_as(&format!("SELECT count(*), COALESCE(sum(octet_length(payload)),0)::bigint FROM {q}.sm_custody_operations"))
            .fetch_one(pg(&mut tx)?).await.map_err(db)?;
        if rows >= self.limits.rows
            || bytes
                .checked_add(payload.len() as i64)
                .is_none_or(|total| total > self.limits.bytes)
        {
            return Err(CustodyError::Capacity);
        }
        if !start_fresh_postgres(pg(&mut tx)?, &format!("{q}.ingress_send_attempts"), lease)
            .await
            .map_err(db)?
        {
            return Err(CustodyError::Rejected);
        }
        sqlx::query(&format!("INSERT INTO {q}.sm_custody_operations (operation_id, gate_id, message_key, receipt_kind, semantic_identity_hash, resource, attempt_token, payload, received_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)"))
            .bind(frame.operation_id).bind(self.capture.gate_id).bind(frame.key.message_key.to_storage())
            .bind(frame.key.kind.to_storage()).bind(frame.key.semantic_identity_hash.as_slice())
            .bind(frame.key.resource.to_string()).bind(frame.attempt).bind(payload).bind(frame.received_at)
            .execute(pg(&mut tx)?).await.map_err(db)?;
        sqlx::query(&format!(
            "UPDATE {q}.ingress_messages SET retention_eligible_at = NULL WHERE message_key = $1"
        ))
        .bind(frame.key.message_key.to_storage())
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        self.fresh_principal(&mut tx).await?;
        commit(tx).await?;
        Ok(CommittedAdmission {
            gate: self.capture.gate_id,
            operation: frame.operation_id,
        })
    }

    pub(super) async fn validate_canonical(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        frame: &PreparedDirectFrame,
        require_unresolved: bool,
    ) -> Result<(), CustodyError> {
        let q = &self.schema;
        let row = sqlx::query(&format!("SELECT m.envelope_version::bigint, m.envelope, m.created_at, i.kind, i.payload_version::bigint, i.payload FROM {q}.ingress_messages m JOIN {q}.ingress_effect_intents i ON i.message_key = m.message_key WHERE m.message_key = $1 AND i.kind = $2 AND i.semantic_identity_hash = $3"))
            .bind(frame.key.message_key.to_storage()).bind(frame.key.kind.to_storage())
            .bind(frame.key.semantic_identity_hash.as_slice()).fetch_optional(pg(tx)?).await.map_err(db)?
            .ok_or(CustodyError::Rejected)?;
        let version: Option<i64> = row.try_get(0).map_err(db)?;
        let envelope: Option<Vec<u8>> = row.try_get(1).map_err(db)?;
        let received_at: chrono::DateTime<chrono::Utc> = row.try_get(2).map_err(db)?;
        let envelope = MessageEnvelope::from_pin_lab_storage(
            version.ok_or(CustodyError::Rejected)?,
            envelope.ok_or(CustodyError::Rejected)?,
        )
        .map_err(|_| CustodyError::Rejected)?;
        let intent = IngressEffectIntent::decode_v1(
            row.try_get(3).map_err(db)?,
            &row.try_get::<Vec<u8>, _>(5).map_err(db)?,
        )
        .map_err(|_| CustodyError::Rejected)?;
        let receipt = crate::ingress::receipt_key(&intent).map_err(|_| CustodyError::Rejected)?;
        if received_at != frame.received_at
            || envelope.version() != EnvelopeVersion::V1
            || envelope.message() != &frame.payload
            || row.try_get::<i64, _>(4).map_err(db)? != 1
            || receipt.kind.to_storage() != frame.key.kind.to_storage()
            || receipt.semantic_identity_hash != frame.key.semantic_identity_hash
        {
            return Err(CustodyError::Rejected);
        }
        let IngressEffectIntent::RouteDirect {
            recipient,
            fanout,
            prepared: Some(prepared),
            ..
        } = intent
        else {
            return Err(CustodyError::Rejected);
        };
        if prepared.message() != &frame.payload
            || frame.payload.from.is_none()
            || !frame
                .payload
                .to
                .as_ref()
                .is_some_and(|jid| jid.to_bare() == recipient)
            || frame.key.resource.to_bare() != recipient
            || !fanout.contains(&frame.key.resource)
            || fanout.iter().any(|target| target.to_bare() != recipient)
            || !matches!(
                frame.payload.type_,
                xmpp_parsers::message::MessageType::Chat
                    | xmpp_parsers::message::MessageType::Normal
                    | xmpp_parsers::message::MessageType::Headline
            )
        {
            return Err(CustodyError::Rejected);
        }
        if require_unresolved {
            let resolved: bool = sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM {q}.ingress_effect_receipts WHERE message_key = $1 AND kind = $2 AND semantic_identity_hash = $3) OR EXISTS (SELECT 1 FROM {q}.ingress_delivery_receipts WHERE message_key = $1 AND kind = $2 AND semantic_identity_hash = $3 AND resource = $4) OR EXISTS (SELECT 1 FROM {q}.ingress_carbon_receipts WHERE message_key = $1 AND kind = $2 AND semantic_identity_hash = $3 AND recipient = $4) OR EXISTS (SELECT 1 FROM {q}.sm_ingress_appends WHERE message_key = $1::text AND receipt_kind = $2 AND semantic_identity_hash = $3 AND resource = $4)"))
                .bind(frame.key.message_key.to_storage()).bind(frame.key.kind.to_storage())
                .bind(frame.key.semantic_identity_hash.as_slice()).bind(frame.key.resource.to_string())
                .fetch_one(pg(tx)?).await.map_err(db)?;
            if resolved {
                return Err(CustodyError::Rejected);
            }
        }
        Ok(())
    }

    /// A known committed slot is the first capability a future adapter could
    /// use for memory acceptance. Repeating bind returns the exact same ordinal.
    pub async fn bind(
        &self,
        admission: &CommittedAdmission,
    ) -> Result<CommittedSlot, CustodyError> {
        self.check_admission(admission)?;
        let mut tx = self
            .guarded_operation(admission.operation, false, true, None)
            .await?;
        let saved = self.operation(&mut tx, admission.operation).await?;
        if saved.terminal.is_some() {
            return Err(CustodyError::Rejected);
        }
        if let Some(slot) = saved.slot {
            commit(tx).await?;
            return Ok(slot);
        }
        let q = &self.schema;
        let (appended, acked): (i64, i64) = sqlx::query_as(&format!(
            "SELECT appended, peer_acked FROM {q}.sm_custody_gates WHERE gate_id = $1"
        ))
        .bind(self.capture.gate_id)
        .fetch_one(pg(&mut tx)?)
        .await
        .map_err(db)?;
        if appended - acked >= ACK_HALF_RANGE - 1 {
            return Err(CustodyError::Capacity);
        }
        let ordinal = appended.checked_add(1).ok_or(CustodyError::Overflow)?;
        sqlx::query(&format!(
            "INSERT INTO {q}.sm_custody_slots (operation_id, gate_id, ordinal) VALUES ($1,$2,$3)"
        ))
        .bind(admission.operation)
        .bind(admission.gate)
        .bind(ordinal)
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        let changed = sqlx::query(&format!(
            "UPDATE {q}.sm_custody_gates SET appended = $2 WHERE gate_id = $1 AND $2 - peer_acked < 2147483648"
        ))
        .bind(admission.gate)
        .bind(ordinal)
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?
        .rows_affected();
        if changed != 1 {
            return Err(CustodyError::Capacity);
        }
        self.fresh_principal(&mut tx).await?;
        commit(tx).await?;
        Ok(CommittedSlot {
            admission: admission.clone(),
            ordinal,
        })
    }

    /// Commit only the next hydrated ordinal. This creates no socket send.
    pub async fn expose(
        &self,
        receiver: &CandidateSmMemory,
        slot: &CommittedSlot,
    ) -> Result<(), CustodyError> {
        self.check_admission(&slot.admission)?;
        receiver.check_exposure(&self.capture, slot)?;
        let mut tx = self
            .guarded_operation(slot.admission.operation, false, true, None)
            .await?;
        let saved = self.operation(&mut tx, slot.admission.operation).await?;
        if saved.slot.as_ref() != Some(slot) || saved.terminal.is_some() {
            return Err(CustodyError::Rejected);
        }
        let changed = sqlx::query(&format!("UPDATE {}.sm_custody_gates SET exposed = $2 WHERE gate_id = $1 AND appended >= $2 AND exposed = $2 - 1", self.schema))
            .bind(slot.admission.gate).bind(slot.ordinal).execute(pg(&mut tx)?).await.map_err(db)?.rows_affected();
        if changed != 1 {
            return Err(CustodyError::Rejected);
        }
        self.fresh_principal(&mut tx).await?;
        commit(tx).await
    }
}

pub(super) fn encode_payload(frame: &PreparedDirectFrame) -> Result<String, CustodyError> {
    let element: Element = frame.payload.clone().into();
    let mut bytes = Vec::new();
    element
        .write_to(&mut bytes)
        .map_err(|_| CustodyError::InvalidStored)?;
    if bytes.is_empty() || bytes.len() > 256 * 1024 {
        return Err(CustodyError::Capacity);
    }
    String::from_utf8(bytes).map_err(|_| CustodyError::InvalidStored)
}
