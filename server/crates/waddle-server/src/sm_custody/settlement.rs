use sqlx::Row;
use waddle_xmpp::ingress::MessageKey;

use super::repository::{commit, pg};
use super::types::{db, ACK_HALF_RANGE};
use super::{
    CommittedAdmission, CustodyError, CustodyRecoveryAuthority, CustodyTerminal, SealedSession,
    SmCustodyRepository,
};
use crate::ingress_substrate::ALIAS_RETENTION;
use crate::ingress_uow::IngressUowTransaction;

impl SmCustodyRepository {
    /// Validate and settle one contiguous exposed prefix in a single commit.
    /// The gate stays open. No socket or XMPP response is produced here.
    pub async fn acknowledge(&self, h: u32) -> Result<(), CustodyError> {
        // A changed provisional prefix releases every lock and starts a fresh
        // transaction. Four attempts bound contention; exhaustion is Conflict.
        for _ in 0..4 {
            let mut tx = self.authority().await?;
            let q = &self.schema;
            let frontier: (i64, i64) = sqlx::query_as(&format!(
                "SELECT peer_acked, exposed FROM {q}.sm_custody_gates WHERE gate_id = $1"
            ))
            .bind(self.capture.gate_id)
            .fetch_optional(pg(&mut tx)?)
            .await
            .map_err(db)?
            .ok_or(CustodyError::Rejected)?;
            let (acked, exposed) = frontier;
            let window = exposed - acked;
            let advance = i64::from(h.wrapping_sub(acked as u32));
            let in_window = window < ACK_HALF_RANGE && advance <= window;
            let excessive = advance > 1024;
            let through = if in_window && !excessive {
                Some(acked.checked_add(advance).ok_or(CustodyError::Overflow)?)
            } else {
                None
            };
            let slots = match through {
                Some(through) if advance > 0 => self.ack_prefix(&mut tx, acked, through).await?,
                _ => Vec::new(),
            };
            if slots
                .iter()
                .any(|row| row.3 != self.capture.resource.to_string())
            {
                return Err(CustodyError::Conflict);
            }
            let mut parents: Vec<_> = slots.iter().map(|row| row.2).collect();
            parents.sort_unstable();
            parents.dedup();
            match self.lock_sources_and_routes(&mut tx, &parents, false).await {
                Ok(()) => {}
                Err(CustodyError::SourceMissing) => continue,
                Err(error) => return Err(error),
            }
            self.lock_gate(&mut tx, Some(false)).await?;
            self.fresh_principal(&mut tx).await?;
            let current: (i64, i64) = sqlx::query_as(&format!(
                "SELECT peer_acked, exposed FROM {q}.sm_custody_gates WHERE gate_id = $1"
            ))
            .bind(self.capture.gate_id)
            .fetch_one(pg(&mut tx)?)
            .await
            .map_err(db)?;
            if current != frontier {
                continue;
            }
            if let Some(through) = through.filter(|_| advance > 0) {
                if self.ack_prefix(&mut tx, acked, through).await? != slots {
                    continue;
                }
            }
            if window >= ACK_HALF_RANGE || (in_window && excessive) {
                return Err(CustodyError::Conflict);
            }
            let through = through.ok_or(CustodyError::Rejected)?;
            if slots.len() as i64 != advance
                || slots
                    .iter()
                    .enumerate()
                    .any(|(i, row)| row.1 != acked + i as i64 + 1 || row.4.is_some())
            {
                return Err(CustodyError::Conflict);
            }
            for (operation, _, _, _, _) in slots {
                sqlx::query(&format!(
                    "INSERT INTO {q}.sm_custody_terminals (operation_id, disposition) VALUES ($1,$2)"
                ))
                .bind(operation)
                .bind(CustodyTerminal::Ack.code())
                .execute(pg(&mut tx)?)
                .await
                .map_err(db)?;
            }
            if advance > 0 {
                sqlx::query(&format!(
                    "UPDATE {q}.sm_custody_gates SET peer_acked = $2 WHERE gate_id = $1"
                ))
                .bind(self.capture.gate_id)
                .bind(through)
                .execute(pg(&mut tx)?)
                .await
                .map_err(db)?;
            }
            for key in parents {
                let key = MessageKey::from_storage(key);
                if self.no_other_custody(&mut tx, key).await? {
                    sqlx::query(&format!("UPDATE {q}.ingress_messages SET retention_eligible_at = pg_catalog.clock_timestamp() WHERE message_key = $1"))
                        .bind(key.to_storage()).execute(pg(&mut tx)?).await.map_err(db)?;
                }
            }
            self.fresh_principal(&mut tx).await?;
            return commit(tx).await;
        }
        Err(CustodyError::Conflict)
    }

    async fn ack_prefix(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        acked: i64,
        through: i64,
    ) -> Result<Vec<(uuid::Uuid, i64, uuid::Uuid, String, Option<i32>)>, CustodyError> {
        let q = &self.schema;
        // The candidate's global admission quota is 1024 rows. Limit the read
        // even if a corrupt frontier claims a much larger exposed prefix.
        sqlx::query_as(&format!(
            "SELECT s.operation_id, s.ordinal, o.message_key, o.resource, t.disposition FROM {q}.sm_custody_slots s JOIN {q}.sm_custody_operations o ON (o.operation_id,o.gate_id) = (s.operation_id,s.gate_id) LEFT JOIN {q}.sm_custody_terminals t ON t.operation_id = s.operation_id WHERE s.gate_id = $1 AND s.ordinal > $2 AND s.ordinal <= $3 ORDER BY s.ordinal LIMIT 1025"
        ))
        .bind(self.capture.gate_id).bind(acked).bind(through)
        .fetch_all(pg(tx)?).await.map_err(db)
    }

    /// A source-marked slot's full payload/time/resource transfer commits
    /// atomically with terminal custody. This candidate sink is not the
    /// selected pending_delivery store.
    pub async fn transfer(&self, admission: &CommittedAdmission) -> Result<(), CustodyError> {
        self.finish_transfer(admission, None).await
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
        self.finish_transfer(admission, Some(authority)).await
    }

    async fn finish_transfer(
        &self,
        admission: &CommittedAdmission,
        recovery: Option<&CustodyRecoveryAuthority>,
    ) -> Result<(), CustodyError> {
        self.check_admission(admission)?;
        let mut tx = match self
            .guarded_operation(admission.operation, true, false, recovery)
            .await
        {
            Ok(tx) => tx,
            Err(CustodyError::SourceMissing) => {
                return self.collected_transfer_readback(admission, recovery).await;
            }
            Err(error) => return Err(error),
        };
        let saved = self.operation(&mut tx, admission.operation).await?;
        let q = &self.schema;
        if saved.terminal == Some(CustodyTerminal::Ack) {
            return Err(CustodyError::Conflict);
        }
        // An admitted payload, or even a bound slot, does not prove the exact
        // source attempt handed ownership to this candidate. In particular,
        // never seal the gate on a refusal: the source may still finish its
        // handoff, and a successor must be able to inventory the payload.
        if saved.slot.is_none()
            || !self
                .has_source_handoff(&mut tx, admission.operation)
                .await?
        {
            return Err(CustodyError::Rejected);
        }
        // The first transfer seals the entire generation, even when later
        // transfers have not yet committed. Recovery can inventory the rest.
        sqlx::query(&format!(
            "UPDATE {q}.sm_custody_gates SET sealed = TRUE WHERE gate_id = $1"
        ))
        .bind(admission.gate)
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        if let Some(existing) = saved.terminal {
            if existing != CustodyTerminal::Transfer {
                return Err(CustodyError::Conflict);
            }
            let exact: bool = sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM {q}.sm_custody_transfers t JOIN {q}.sm_custody_operations o USING (operation_id) WHERE o.operation_id = $1 AND (t.resource,t.payload,t.received_at) = (o.resource,o.payload,o.received_at))"))
                .bind(admission.operation).fetch_one(pg(&mut tx)?).await.map_err(db)?;
            if !exact {
                return Err(CustodyError::Conflict);
            }
            if recovery.is_none() {
                self.fresh_principal(&mut tx).await?;
            }
            commit(tx).await?;
            return Ok(());
        }
        sqlx::query(&format!("INSERT INTO {q}.sm_custody_transfers (operation_id, resource, payload, received_at) SELECT operation_id,resource,payload,received_at FROM {q}.sm_custody_operations WHERE operation_id = $1"))
            .bind(admission.operation).execute(pg(&mut tx)?).await.map_err(db)?;
        sqlx::query(&format!(
            "INSERT INTO {q}.sm_custody_terminals (operation_id, disposition) VALUES ($1,$2)"
        ))
        .bind(admission.operation)
        .bind(CustodyTerminal::Transfer.code())
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

    /// A collected source cannot be locked. An already sealed, exact transfer
    /// can still be confirmed from immutable candidate evidence under the gate.
    /// This path performs no source-bearing write or later source lock.
    async fn collected_transfer_readback(
        &self,
        admission: &CommittedAdmission,
        recovery: Option<&CustodyRecoveryAuthority>,
    ) -> Result<(), CustodyError> {
        let mut tx = match recovery {
            Some(authority) => self.recovery_guard(authority, true).await?,
            None => self.guarded(true).await?,
        };
        let saved = self.operation(&mut tx, admission.operation).await?;
        if saved.terminal == Some(CustodyTerminal::Ack) {
            return Err(CustodyError::Conflict);
        }
        if saved.slot.is_none()
            || !self
                .has_source_handoff(&mut tx, admission.operation)
                .await?
        {
            return Err(CustodyError::Rejected);
        }
        if saved.terminal != Some(CustodyTerminal::Transfer) {
            return Err(CustodyError::SourceMissing);
        }
        let q = &self.schema;
        let exact: bool = sqlx::query_scalar(&format!("SELECT g.sealed AND EXISTS (SELECT 1 FROM {q}.sm_custody_transfers t JOIN {q}.sm_custody_operations o USING (operation_id) WHERE o.operation_id=$1 AND o.gate_id=g.gate_id AND (t.resource,t.payload,t.received_at)=(o.resource,o.payload,o.received_at)) FROM {q}.sm_custody_gates g WHERE g.gate_id=$2"))
            .bind(admission.operation).bind(self.capture.gate_id).fetch_one(pg(&mut tx)?).await.map_err(db)?;
        if !exact {
            return Err(CustodyError::Conflict);
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
        let mut tx = self.guarded_source(true, false, key).await?;
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
