use sqlx::Row;
use waddle_xmpp::ingress::MessageKey;

use super::admission::encode_payload;
use super::repository::{commit, pg};
use super::types::db;
use super::{
    CommittedAdmission, CommittedSlot, CustodyError, CustodyRecoveryAuthority, CustodyTerminal,
    SealedSession, SmCustodyRepository,
};
use crate::ingress_substrate::ALIAS_RETENTION;
use crate::ingress_uow::IngressUowTransaction;

impl SmCustodyRepository {
    /// Positive ACK must name this exact, durably exposed slot. The candidate
    /// accepts one frame per captured session, not arbitrary cumulative ranges.
    pub async fn acknowledge(&self, slot: &CommittedSlot, h: u32) -> Result<(), CustodyError> {
        if h != slot.h() {
            return Err(CustodyError::Rejected);
        }
        self.finish(&slot.admission, Some(slot), CustodyTerminal::Ack, None)
            .await
    }

    /// Full payload/time/resource transfer commits atomically with terminal
    /// custody. This candidate sink is not the selected pending_delivery store.
    pub async fn transfer(&self, admission: &CommittedAdmission) -> Result<(), CustodyError> {
        self.finish(admission, None, CustodyTerminal::Transfer, None)
            .await
    }

    /// Recovery may transfer retained payload under a fresh exact claim without
    /// resurrecting the old user principal or authorizing socket exposure.
    pub async fn transfer_recovered(
        &self,
        sealed: &SealedSession,
        authority: &CustodyRecoveryAuthority,
        admission: &CommittedAdmission,
    ) -> Result<(), CustodyError> {
        if sealed.gate != self.capture.gate_id || sealed.token != self.capture.token {
            return Err(CustodyError::Rejected);
        }
        self.finish(admission, None, CustodyTerminal::Transfer, Some(authority))
            .await
    }

    async fn finish(
        &self,
        admission: &CommittedAdmission,
        slot: Option<&CommittedSlot>,
        terminal: CustodyTerminal,
        recovery: Option<&CustodyRecoveryAuthority>,
    ) -> Result<(), CustodyError> {
        self.check_admission(admission)?;
        let mut tx = match recovery {
            Some(authority) => self.recovery_guard(authority, true).await?,
            None => self.guarded(true).await?,
        };
        let saved = self.operation(&mut tx, admission.operation).await?;
        if let Some(existing) = saved.terminal {
            if existing != terminal {
                return Err(CustodyError::Conflict);
            }
            commit(tx).await?;
            return Ok(());
        }
        self.lock_canonical(&mut tx, saved.frame.key.message_key)
            .await?;
        if recovery.is_none() {
            self.fresh_principal(&mut tx).await?;
        }
        let q = &self.schema;
        if terminal == CustodyTerminal::Ack {
            let slot = slot.ok_or(CustodyError::Rejected)?;
            if saved.slot.as_ref() != Some(slot) {
                return Err(CustodyError::Rejected);
            }
            let exposed: i64 = sqlx::query_scalar(&format!(
                "SELECT exposed FROM {q}.sm_custody_gates WHERE gate_id = $1"
            ))
            .bind(admission.gate)
            .fetch_one(pg(&mut tx)?)
            .await
            .map_err(db)?;
            if exposed != slot.ordinal {
                return Err(CustodyError::Rejected);
            }
            sqlx::query(&format!(
                "UPDATE {q}.sm_custody_gates SET peer_acked = $2 WHERE gate_id = $1"
            ))
            .bind(admission.gate)
            .bind(slot.ordinal)
            .execute(pg(&mut tx)?)
            .await
            .map_err(db)?;
        }
        if terminal == CustodyTerminal::Transfer {
            sqlx::query(&format!("INSERT INTO {q}.sm_custody_transfers (operation_id, resource, payload, received_at) VALUES ($1,$2,$3,$4)"))
                .bind(admission.operation).bind(saved.frame.key.resource.to_string())
                .bind(encode_payload(&saved.frame)?).bind(saved.frame.received_at)
                .execute(pg(&mut tx)?).await.map_err(db)?;
        }
        sqlx::query(&format!(
            "INSERT INTO {q}.sm_custody_terminals (operation_id, disposition) VALUES ($1,$2)"
        ))
        .bind(admission.operation)
        .bind(terminal.code())
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        sqlx::query(&format!(
            "UPDATE {q}.sm_custody_gates SET sealed = TRUE WHERE gate_id = $1"
        ))
        .bind(admission.gate)
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        let free = self
            .no_other_custody(&mut tx, saved.frame.key.message_key)
            .await?;
        if free {
            sqlx::query(&format!("UPDATE {q}.ingress_messages SET retention_eligible_at = pg_catalog.clock_timestamp() WHERE message_key = $1"))
                .bind(saved.frame.key.message_key.to_storage()).execute(pg(&mut tx)?).await.map_err(db)?;
        }
        if recovery.is_none() {
            self.fresh_principal(&mut tx).await?;
        }
        commit(tx).await
    }

    /// Narrow candidate collector. Existing production blockers are rechecked
    /// under the canonical lock. It does not establish old-writer quiescence or
    /// replace production GC. Journals/transfers remain for exact reconciliation.
    pub async fn collect_terminal(
        &self,
        sealed: &SealedSession,
        key: MessageKey,
    ) -> Result<bool, CustodyError> {
        if sealed.gate != self.capture.gate_id || sealed.token != self.capture.token {
            return Err(CustodyError::Rejected);
        }
        let mut tx = self.guarded(true).await?;
        self.canonical(&mut tx, key).await?;
        let q = &self.schema;
        let own: bool = sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM {q}.sm_custody_operations o JOIN {q}.sm_custody_terminals t ON t.operation_id = o.operation_id WHERE o.gate_id = $1 AND o.message_key = $2)"))
            .bind(self.capture.gate_id).bind(key.to_storage()).fetch_one(pg(&mut tx)?).await.map_err(db)?;
        if !own {
            return Err(CustodyError::Rejected);
        }
        let row = sqlx::query(&format!("SELECT terminal_at IS NOT NULL, retention_eligible_at IS NOT NULL AND retention_eligible_at + $2 * interval '1 second' <= pg_catalog.clock_timestamp() FROM {q}.ingress_messages WHERE message_key = $1"))
            .bind(key.to_storage()).bind(ALIAS_RETENTION.num_seconds() as f64).fetch_one(pg(&mut tx)?).await.map_err(db)?;
        if !row.try_get::<bool, _>(0).map_err(db)?
            || !row.try_get::<bool, _>(1).map_err(db)?
            || !self.no_other_custody(&mut tx, key).await?
        {
            commit(tx).await?;
            return Ok(false);
        }
        for table in ["ingress_origin_aliases", "ingress_deliveries"] {
            sqlx::query(&format!("DELETE FROM {q}.{table} WHERE message_key = $1"))
                .bind(key.to_storage())
                .execute(pg(&mut tx)?)
                .await
                .map_err(db)?;
        }
        sqlx::query(&format!(
            "DELETE FROM {q}.ingress_messages WHERE message_key = $1"
        ))
        .bind(key.to_storage())
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        commit(tx).await?;
        Ok(true)
    }

    async fn no_other_custody(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        key: MessageKey,
    ) -> Result<bool, CustodyError> {
        let q = &self.schema;
        // This deliberately repeats the candidate's narrow blocker boundary;
        // production GC integration and all existing SM writers remain deferred.
        sqlx::query_scalar(&format!(
            "SELECT NOT EXISTS (SELECT 1 FROM {q}.sm_custody_operations o WHERE o.message_key = $1
               AND NOT EXISTS (SELECT 1 FROM {q}.sm_custody_terminals t WHERE t.operation_id = o.operation_id))
             AND NOT EXISTS (SELECT 1 FROM {q}.ingress_sm_refs WHERE message_key = $1)
             AND NOT EXISTS (SELECT 1 FROM {q}.ingress_effect_descendants WHERE message_key = $1 AND settled_at IS NULL)
             AND NOT EXISTS (SELECT 1 FROM {q}.ingress_effect_intents i WHERE i.message_key = $1
               AND NOT EXISTS (SELECT 1 FROM {q}.ingress_effect_receipts r WHERE r.message_key = i.message_key
                 AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash))
             AND NOT EXISTS (SELECT 1 FROM {q}.sm_ingress_appends WHERE message_key = $1::text AND disposition = 0)
             AND NOT EXISTS (SELECT 1 FROM {q}.ingress_send_attempts WHERE message_key = $1 AND state <> 2)
             AND NOT EXISTS (SELECT 1 FROM {q}.ingress_archive_dispatch d JOIN {q}.pending_delivery p
               ON p.row_id = d.pending_row_id WHERE d.message_key = $1 AND d.pending_row_id <> '')
             AND NOT EXISTS (SELECT 1 FROM {q}.ingress_owner_rejections WHERE source_message_key = $1 OR child_message_key = $1)
             AND NOT EXISTS (SELECT 1 FROM {q}.ingress_messages WHERE message_key = $1
               AND (source_binding_version IS NOT NULL OR owner_decision IS NOT NULL))"
        ))
            .bind(key.to_storage()).fetch_one(pg(tx)?).await.map_err(db)
    }
}
