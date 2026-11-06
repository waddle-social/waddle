use std::time::Duration;

use jid::FullJid;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::ownership::NodeLeaseIdentity;
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::{SmIngressAppendKey, SmIngressReceiptKind};

use super::types::db;
use super::{
    CommittedAdmission, CommittedSlot, CustodyCapture, CustodyError, CustodyInventoryPage,
    CustodyLimits, CustodyOperation, CustodyRecoveryAuthority, CustodyReplayPage,
    CustodyResolution, CustodyTerminal, PreparedDirectFrame, SealedSession, SessionGeneration,
};
use crate::db::physical_attestation::PhysicalAttestationConfig;
use crate::ingress_uow::{
    ClaimRepository, IngressUnitOfWork, IngressUowTransaction, PrincipalAssertion,
    PrincipalRepository,
};

/// Unselected candidate over the exact shared, physically attested UOW.
/// All operations use epoch -> principal -> exact claim -> gate -> canonical
/// -> attempt -> quota -> journal/slot. No transaction spans transport I/O.
pub struct SmCustodyRepository {
    pub(super) uow: IngressUnitOfWork,
    pub(super) schema: String,
    pub(super) capture: CustodyCapture,
    pub(super) limits: CustodyLimits,
}

impl SmCustodyRepository {
    /// Reconstruct the old captured gate from durable storage under a fresh
    /// successor's exact current claim. No original token, principal, binding,
    /// frame, or operation ID needs to survive the origin process.
    pub async fn recover_session(
        uow: IngressUnitOfWork,
        config: &PhysicalAttestationConfig,
        session: &SmSessionId,
        resource: &FullJid,
        authority: &CustodyRecoveryAuthority,
        limits: CustodyLimits,
    ) -> Result<Self, CustodyError> {
        let schema = uow.custody_candidate_schema(config)?;
        let q = format!("\"{}\"", schema.replace('"', "\"\""));
        let mut tx = uow
            .begin_read_committed_with_timeouts(Duration::from_secs(3), Duration::from_secs(5))
            .await?;
        let _claim =
            ClaimRepository::assert_sm_claim(&mut tx, session, &authority.owner, authority.claim)
                .await?;
        let row = sqlx::query(&format!("SELECT gate_id, token, generation, endpoint, node_id, node_incarnation, claim_epoch FROM {q}.sm_custody_gates WHERE session_id = $1 AND resource = $2 FOR UPDATE"))
            .bind(session.as_str()).bind(resource.to_string()).fetch_optional(pg(&mut tx)?).await.map_err(db)?.ok_or(CustodyError::Rejected)?;
        let endpoint: String = row.try_get(3).map_err(db)?;
        let (principal, binding) =
            serde_json::from_str(&endpoint).map_err(|_| CustodyError::InvalidStored)?;
        let generation: i64 = row.try_get(2).map_err(db)?;
        let capture = CustodyCapture {
            gate_id: row.try_get(0).map_err(db)?,
            token: row.try_get(1).map_err(db)?,
            resource: resource.clone(),
            session: session.clone(),
            principal,
            binding,
            generation: SessionGeneration::new(
                u64::try_from(generation).map_err(|_| CustodyError::InvalidStored)?,
            )?,
            owner: NodeIdentity::new(
                row.try_get::<String, _>(4).map_err(db)?,
                row.try_get::<String, _>(5).map_err(db)?,
            ),
            claim: ClaimEpoch(row.try_get(6).map_err(db)?),
        };
        commit(tx).await?;
        Self::new(uow, config, capture, limits)
    }

    pub fn new(
        uow: IngressUnitOfWork,
        config: &PhysicalAttestationConfig,
        capture: CustodyCapture,
        limits: CustodyLimits,
    ) -> Result<Self, CustodyError> {
        if capture.gate_id.is_nil()
            || capture.token.is_nil()
            || !capture.owner.is_active()
            || capture.principal.bare_jid() != &capture.resource.to_bare()
            || capture.binding.connection_ref().node_lease()
                != &NodeLeaseIdentity::from_node_identity(&capture.owner)
        {
            return Err(CustodyError::Rejected);
        }
        let schema = uow.custody_candidate_schema(config)?;
        Ok(Self {
            uow,
            schema: format!("\"{}\"", schema.replace('"', "\"\"")),
            capture,
            limits,
        })
    }

    pub(super) fn endpoint(&self) -> Result<String, CustodyError> {
        serde_json::to_string(&(&self.capture.principal, &self.capture.binding))
            .map_err(|_| CustodyError::InvalidStored)
    }

    async fn authority(&self) -> Result<IngressUowTransaction<'_>, CustodyError> {
        let mut tx = self
            .uow
            .begin_read_committed_with_timeouts(Duration::from_secs(3), Duration::from_secs(5))
            .await?;
        let expected_schema = self.schema.trim_matches('"').replace("\"\"", "\"");
        if tx.receipt_schema() != Some(expected_schema.as_str()) {
            return Err(CustodyError::Rejected);
        }
        if PrincipalRepository::assert_principal(&mut tx, &self.capture.principal).await?
            != PrincipalAssertion::Asserted
        {
            return Err(CustodyError::Rejected);
        }
        let _claim = ClaimRepository::assert_sm_claim(
            &mut tx,
            &self.capture.session,
            &self.capture.owner,
            self.capture.claim,
        )
        .await?;
        Ok(tx)
    }

    /// Creates an inert captured gate, never a registered or routable endpoint.
    /// It cannot replace a generation or revive an existing sealed gate.
    pub async fn install_captured_session(&self) -> Result<(), CustodyError> {
        let mut tx = self.authority().await?;
        let q = &self.schema;
        sqlx::query(&format!("INSERT INTO {q}.sm_custody_gates (gate_id, token, resource, session_id, generation, endpoint, node_id, node_incarnation, claim_epoch) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)"))
            .bind(self.capture.gate_id).bind(self.capture.token).bind(self.capture.resource.to_string())
            .bind(self.capture.session.as_str()).bind(self.capture.generation.get() as i64)
            .bind(self.endpoint()?).bind(&self.capture.owner.node_id).bind(&self.capture.owner.node_epoch)
            .bind(self.capture.claim.0).execute(pg(&mut tx)?).await.map_err(db)?;
        self.fresh_principal(&mut tx).await?;
        commit(tx).await
    }

    pub(super) async fn guarded(
        &self,
        allow_sealed: bool,
    ) -> Result<IngressUowTransaction<'_>, CustodyError> {
        let mut tx = self.authority().await?;
        let q = &self.schema;
        let row = sqlx::query(&format!("SELECT token, resource, session_id, generation, endpoint, node_id, node_incarnation, claim_epoch, sealed FROM {q}.sm_custody_gates WHERE gate_id = $1 FOR UPDATE"))
            .bind(self.capture.gate_id).fetch_optional(pg(&mut tx)?).await.map_err(db)?
            .ok_or(CustodyError::Rejected)?;
        self.assert_stored_capture(&row)?;
        if !allow_sealed && row.try_get::<bool, _>("sealed").map_err(db)? {
            return Err(CustodyError::Rejected);
        }
        self.fresh_principal(&mut tx).await?;
        Ok(tx)
    }

    // Caller already holds this exact gate row. Comparing the stored capture
    // binds the session whose current claim was asserted to the gate being
    // accessed. Endpoint equality includes the old principal's immutable
    // identity, but deliberately does not require that principal to be live.
    fn assert_stored_capture(&self, row: &PgRow) -> Result<(), CustodyError> {
        if row.try_get::<Uuid, _>("token").map_err(db)? != self.capture.token
            || row.try_get::<String, _>("resource").map_err(db)?
                != self.capture.resource.to_string()
            || row.try_get::<String, _>("session_id").map_err(db)? != self.capture.session.as_str()
            || row.try_get::<i64, _>("generation").map_err(db)?
                != self.capture.generation.get() as i64
            || row.try_get::<String, _>("endpoint").map_err(db)? != self.endpoint()?
            || row.try_get::<String, _>("node_id").map_err(db)? != self.capture.owner.node_id
            || row.try_get::<String, _>("node_incarnation").map_err(db)?
                != self.capture.owner.node_epoch
            || row.try_get::<i64, _>("claim_epoch").map_err(db)? != self.capture.claim.0
        {
            return Err(CustodyError::Rejected);
        }
        Ok(())
    }

    // Recheck database-clock expiry after blocking gate/canonical waits. The
    // principal SHARE lock already prevents concurrent revocation changes.
    pub(super) async fn fresh_principal(
        &self,
        tx: &mut IngressUowTransaction<'_>,
    ) -> Result<(), CustodyError> {
        let q = &self.schema;
        let valid: bool = sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM {q}.sessions WHERE user_jid = $1 AND auth_context_id = $2 AND auth_context_version = $3 AND principal_auth_epoch = $4 AND (expires_at IS NULL OR expires_at::timestamptz > pg_catalog.clock_timestamp()))"))
            .bind(self.capture.principal.bare_jid().to_string())
            .bind(self.capture.principal.auth_context_id().as_uuid().to_string())
            .bind(self.capture.principal.auth_context_version().get() as i64)
            .bind(self.capture.principal.auth_epoch().get() as i64)
            .fetch_one(pg(tx)?).await.map_err(db)?;
        if !valid {
            return Err(CustodyError::Rejected);
        }
        Ok(())
    }

    /// Irreversibly fence every old admission/bind attempt, including attempts
    /// delayed before enrollment. A committed seal is required for negatives.
    pub async fn seal(&self) -> Result<SealedSession, CustodyError> {
        let mut tx = self.guarded(true).await?;
        sqlx::query(&format!(
            "UPDATE {}.sm_custody_gates SET sealed = TRUE WHERE gate_id = $1",
            self.schema
        ))
        .bind(self.capture.gate_id)
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        self.fresh_principal(&mut tx).await?;
        commit(tx).await?;
        Ok(SealedSession {
            gate: self.capture.gate_id,
            token: self.capture.token,
        })
    }

    fn verify_seal(&self, sealed: &SealedSession) -> Result<(), CustodyError> {
        if sealed.gate != self.capture.gate_id || sealed.token != self.capture.token {
            return Err(CustodyError::Rejected);
        }
        Ok(())
    }

    pub(super) async fn recovery_guard(
        &self,
        authority: &CustodyRecoveryAuthority,
        sealed: bool,
    ) -> Result<IngressUowTransaction<'_>, CustodyError> {
        let mut tx = self
            .uow
            .begin_read_committed_with_timeouts(Duration::from_secs(3), Duration::from_secs(5))
            .await?;
        let _claim = ClaimRepository::assert_sm_claim(
            &mut tx,
            &self.capture.session,
            &authority.owner,
            authority.claim,
        )
        .await?;
        let q = &self.schema;
        let row = sqlx::query(&format!("SELECT token, resource, session_id, generation, endpoint, node_id, node_incarnation, claim_epoch, sealed FROM {q}.sm_custody_gates WHERE gate_id = $1 FOR UPDATE"))
        .bind(self.capture.gate_id)
        .fetch_optional(pg(&mut tx)?)
        .await
        .map_err(db)?
        .ok_or(CustodyError::Rejected)?;
        self.assert_stored_capture(&row)?;
        if sealed && !row.try_get::<bool, _>("sealed").map_err(db)? {
            return Err(CustodyError::Rejected);
        }
        Ok(tx)
    }

    /// A successor must first own the exact current claim. Replacing that claim
    /// serializes behind old claim holders; the irreversible gate seal also
    /// rejects original attempts that had not entered their transaction yet.
    pub async fn seal_for_recovery(
        &self,
        authority: &CustodyRecoveryAuthority,
    ) -> Result<SealedSession, CustodyError> {
        let mut tx = self.recovery_guard(authority, false).await?;
        sqlx::query(&format!(
            "UPDATE {}.sm_custody_gates SET sealed = TRUE WHERE gate_id = $1",
            self.schema
        ))
        .bind(self.capture.gate_id)
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        commit(tx).await?;
        Ok(SealedSession {
            gate: self.capture.gate_id,
            token: self.capture.token,
        })
    }

    /// Sealed, cursor-based recovery needs no original payload/attempt. The
    /// authority is the fresh recovery claim, not the captured user's session.
    pub async fn inventory(
        &self,
        sealed: &SealedSession,
        authority: &CustodyRecoveryAuthority,
        after: Option<Uuid>,
    ) -> Result<CustodyInventoryPage, CustodyError> {
        self.verify_seal(sealed)?;
        let mut tx = self.recovery_guard(authority, true).await?;
        let cursor: Option<(bool, i64)> = if let Some(id) = after {
            Some(sqlx::query_as(&format!("SELECT s.ordinal IS NULL, coalesce(s.ordinal,0) FROM {}.sm_custody_operations o LEFT JOIN {}.sm_custody_slots s ON s.operation_id = o.operation_id WHERE o.gate_id = $1 AND o.operation_id = $2", self.schema, self.schema))
                .bind(self.capture.gate_id).bind(id).fetch_optional(pg(&mut tx)?).await.map_err(db)?.ok_or(CustodyError::Rejected)?)
        } else {
            None
        };
        let rows = sqlx::query(&format!("{} WHERE o.gate_id = $1 AND ($2::boolean IS NULL OR (s.ordinal IS NULL, coalesce(s.ordinal,0), o.operation_id) > ($2,$3,$4)) ORDER BY (s.ordinal IS NULL), s.ordinal, o.operation_id LIMIT 17", operation_select(&self.schema)))
            .bind(self.capture.gate_id).bind(cursor.map(|v| v.0)).bind(cursor.map(|v| v.1)).bind(after)
            .fetch_all(pg(&mut tx)?).await.map_err(db)?;
        let operations = rows
            .into_iter()
            .map(decode_operation)
            .collect::<Result<Vec<_>, _>>()?;
        let mut operations = operations;
        let more = operations.len() > 16;
        if more {
            operations.truncate(16);
        }
        let next = if more {
            operations.last().map(|item| item.frame.operation_id)
        } else {
            None
        };
        commit(tx).await?;
        Ok(CustodyInventoryPage { operations, next })
    }

    /// Bounded read-only inventory of pending slots with committed source
    /// handoff. Gaps stay visible through their original ordinals; no new
    /// attempt or send authority exists.
    pub async fn replay_inventory(
        &self,
        sealed: &SealedSession,
        authority: &CustodyRecoveryAuthority,
        after: Option<u64>,
    ) -> Result<CustodyReplayPage, CustodyError> {
        self.verify_seal(sealed)?;
        let after = after
            .map(i64::try_from)
            .transpose()
            .map_err(|_| CustodyError::Rejected)?
            .unwrap_or(0);
        let mut tx = self.recovery_guard(authority, true).await?;
        let rows = sqlx::query(&format!("{} WHERE o.gate_id = $1 AND s.ordinal > $2 AND t.operation_id IS NULL AND EXISTS (SELECT 1 FROM {}.sm_custody_handoffs h WHERE h.operation_id = o.operation_id) ORDER BY s.ordinal LIMIT 17", operation_select(&self.schema), self.schema))
            .bind(self.capture.gate_id).bind(after).fetch_all(pg(&mut tx)?).await.map_err(db)?;
        let mut operations = rows
            .into_iter()
            .map(decode_operation)
            .collect::<Result<Vec<_>, _>>()?;
        let more = operations.len() > 16;
        operations.truncate(16);
        let next = if more {
            operations
                .last()
                .and_then(|item| item.slot.as_ref())
                .map(CommittedSlot::ordinal)
        } else {
            None
        };
        commit(tx).await?;
        Ok(CustodyReplayPage { operations, next })
    }

    pub async fn reconcile(
        &self,
        sealed: &SealedSession,
        authority: &CustodyRecoveryAuthority,
        operation: Uuid,
    ) -> Result<CustodyResolution, CustodyError> {
        self.verify_seal(sealed)?;
        let mut tx = self.recovery_guard(authority, true).await?;
        let row = sqlx::query(&format!(
            "{} WHERE o.gate_id = $1 AND o.operation_id = $2",
            operation_select(&self.schema)
        ))
        .bind(self.capture.gate_id)
        .bind(operation)
        .fetch_optional(pg(&mut tx)?)
        .await
        .map_err(db)?;
        let found = row.map(decode_operation).transpose()?;
        commit(tx).await?;
        Ok(match found {
            Some(item) => CustodyResolution::Committed(Box::new(item)),
            None => CustodyResolution::ProvenAbsent,
        })
    }

    pub(super) async fn operation(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        id: Uuid,
    ) -> Result<CustodyOperation, CustodyError> {
        let row = sqlx::query(&format!(
            "{} WHERE o.operation_id = $1 AND o.gate_id = $2",
            operation_select(&self.schema)
        ))
        .bind(id)
        .bind(self.capture.gate_id)
        .fetch_optional(pg(tx)?)
        .await
        .map_err(db)?
        .ok_or(CustodyError::Rejected)?;
        decode_operation(row)
    }

    pub(super) async fn canonical(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        key: MessageKey,
    ) -> Result<(), CustodyError> {
        self.lock_canonical(tx, key).await?;
        self.fresh_principal(tx).await
    }

    pub(super) async fn lock_canonical(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        key: MessageKey,
    ) -> Result<(), CustodyError> {
        sqlx::query(&format!(
            "SELECT 1 FROM {}.ingress_messages WHERE message_key = $1 FOR UPDATE",
            self.schema
        ))
        .bind(key.to_storage())
        .fetch_optional(pg(tx)?)
        .await
        .map_err(db)?
        .ok_or(CustodyError::SourceMissing)?;
        Ok(())
    }

    pub(super) fn check_admission(
        &self,
        admission: &CommittedAdmission,
    ) -> Result<(), CustodyError> {
        if admission.gate != self.capture.gate_id {
            return Err(CustodyError::Rejected);
        }
        Ok(())
    }
}

fn operation_select(q: &str) -> String {
    format!("SELECT o.operation_id, o.gate_id, o.message_key, o.receipt_kind, o.semantic_identity_hash, o.resource, o.attempt_token, o.payload, o.received_at, s.ordinal, t.disposition, g.token FROM {q}.sm_custody_operations o JOIN {q}.sm_custody_gates g ON g.gate_id = o.gate_id LEFT JOIN {q}.sm_custody_slots s ON s.operation_id = o.operation_id LEFT JOIN {q}.sm_custody_terminals t ON t.operation_id = o.operation_id")
}

fn decode_operation(row: PgRow) -> Result<CustodyOperation, CustodyError> {
    let id = row.try_get(0).map_err(db)?;
    let gate = row.try_get(1).map_err(db)?;
    let hash: Vec<u8> = row.try_get(4).map_err(db)?;
    let payload: String = row.try_get(7).map_err(db)?;
    Ok(CustodyOperation {
        gate,
        token: row.try_get(11).map_err(db)?,
        frame: PreparedDirectFrame {
            operation_id: id,
            key: SmIngressAppendKey {
                message_key: MessageKey::from_storage(row.try_get(2).map_err(db)?),
                kind: SmIngressReceiptKind::from_storage(row.try_get(3).map_err(db)?),
                semantic_identity_hash: hash.try_into().map_err(|_| CustodyError::InvalidStored)?,
                resource: row
                    .try_get::<String, _>(5)
                    .map_err(db)?
                    .parse()
                    .map_err(|_| CustodyError::InvalidStored)?,
            },
            attempt: row.try_get(6).map_err(db)?,
            payload: waddle_xmpp::parser::message_from_string(&payload)
                .map_err(|_| CustodyError::InvalidStored)?,
            received_at: row.try_get(8).map_err(db)?,
        },
        slot: row
            .try_get::<Option<i64>, _>(9)
            .map_err(db)?
            .map(|ordinal| CommittedSlot {
                admission: CommittedAdmission {
                    gate,
                    operation: id,
                },
                ordinal,
            }),
        terminal: row
            .try_get::<Option<i32>, _>(10)
            .map_err(db)?
            .map(CustodyTerminal::decode)
            .transpose()?,
    })
}

pub(super) fn pg<'a>(
    tx: &'a mut IngressUowTransaction<'_>,
) -> Result<&'a mut PgConnection, CustodyError> {
    tx.transaction_mut()
        .postgres_connection()
        .ok_or(CustodyError::Rejected)
}

pub(super) async fn commit(tx: IngressUowTransaction<'_>) -> Result<(), CustodyError> {
    // Even an error can be COMMIT-unknown. Never hand out a success capability.
    tx.commit().await.map_err(|_| CustodyError::CommitUnknown)
}
