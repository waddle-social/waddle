//! PostgreSQL-only, disposable composition experiment. This is not an SM
//! writer, a connection capability, or a production retention implementation.
//! The fixture role can bypass these APIs with SQL. Its GC deliberately models
//! only admission/SM custody and cannot establish the global lock order.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use thiserror::Error;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};

use super::{
    encode_payload, identical_admission, lookup_postgres, quote_identifier, start_postgres,
    DirectAdmission, DirectAdmissionLabError, DirectAdmissionLimits, DirectAdmissionOutcome,
    FixtureConnection, PhysicalAttestationConfig, PostgresLabTables, SendLease, MAX_FRAME_BYTES,
};
use crate::db::physical_attestation::DarkIngressAuthorityError;
use crate::ingress_substrate::ALIAS_RETENTION;
use crate::ingress_uow::{
    ClaimRepository, IngressUnitOfWork, IngressUowError, IngressUowTransaction, PrincipalAssertion,
    PrincipalRepository,
};

const DDL: &str = r#"
CREATE TABLE __SCHEMA__.custody_lab_quota (id INTEGER PRIMARY KEY CHECK (id = 1));
INSERT INTO __SCHEMA__.custody_lab_quota VALUES (1);
CREATE TABLE __SCHEMA__.custody_lab_gate (
    resource TEXT PRIMARY KEY,
    connection_generation UUID NOT NULL,
    sm_session_id TEXT NOT NULL,
    capability UUID NOT NULL,
    previous_capability UUID,
    ordinal BIGINT NOT NULL DEFAULT 0 CHECK (ordinal >= 0),
    sealed BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE TABLE __SCHEMA__.custody_lab_admission (
    admission_id UUID PRIMARY KEY,
    message_key UUID NOT NULL REFERENCES __SCHEMA__.ingress_messages (message_key) ON DELETE CASCADE,
    receipt_kind INTEGER NOT NULL,
    semantic_identity_hash BYTEA NOT NULL CHECK (octet_length(semantic_identity_hash) = 32),
    resource TEXT NOT NULL,
    attempt_token UUID NOT NULL UNIQUE,
    node_id TEXT NOT NULL,
    node_incarnation TEXT NOT NULL,
    connection_generation UUID NOT NULL,
    sm_session_id TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (octet_length(payload) BETWEEN 1 AND 262144),
    received_at TEXT NOT NULL,
    UNIQUE (message_key, receipt_kind, semantic_identity_hash, resource)
);
CREATE TABLE __SCHEMA__.custody_lab_binding (
    admission_id UUID PRIMARY KEY REFERENCES __SCHEMA__.custody_lab_admission (admission_id) ON DELETE CASCADE,
    sm_session_id TEXT NOT NULL,
    connection_generation UUID NOT NULL,
    ordinal BIGINT NOT NULL CHECK (ordinal > 0),
    UNIQUE (sm_session_id, connection_generation, ordinal)
);
CREATE TABLE __SCHEMA__.custody_lab_counted (
    admission_id UUID PRIMARY KEY REFERENCES __SCHEMA__.custody_lab_binding (admission_id) ON DELETE CASCADE
);
CREATE TABLE __SCHEMA__.custody_lab_terminal (
    admission_id UUID PRIMARY KEY REFERENCES __SCHEMA__.custody_lab_binding (admission_id) ON DELETE CASCADE,
    disposition INTEGER NOT NULL CHECK (disposition IN (1, 2, 3))
);
-- A transfer owns its payload after the canonical and admission are reclaimed.
CREATE TABLE __SCHEMA__.custody_lab_transfer (
    admission_id UUID PRIMARY KEY,
    resource TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE FUNCTION __SCHEMA__.custody_lab_immutable() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN RAISE EXCEPTION 'custody laboratory evidence is immutable'; END $$;
CREATE TRIGGER custody_lab_admission_update BEFORE UPDATE ON __SCHEMA__.custody_lab_admission
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.custody_lab_immutable();
CREATE FUNCTION __SCHEMA__.custody_lab_admission_delete() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM __SCHEMA__.custody_lab_terminal WHERE admission_id = OLD.admission_id) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'pending custody laboratory admission cannot be removed';
END $$;
CREATE TRIGGER custody_lab_admission_delete BEFORE DELETE ON __SCHEMA__.custody_lab_admission
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.custody_lab_admission_delete();
CREATE TRIGGER custody_lab_binding_update BEFORE UPDATE ON __SCHEMA__.custody_lab_binding
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.custody_lab_immutable();
CREATE TRIGGER custody_lab_terminal_update BEFORE UPDATE ON __SCHEMA__.custody_lab_terminal
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.custody_lab_immutable();
CREATE TRIGGER custody_lab_transfer_update BEFORE UPDATE OR DELETE ON __SCHEMA__.custody_lab_transfer
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.custody_lab_immutable();
-- This typed link is installed only on the empty disposable fixture table.
-- It closes this fixture's raw late-append race; it is not a migration.
ALTER TABLE __SCHEMA__.sm_ingress_appends
    ADD COLUMN custody_lab_message_key UUID GENERATED ALWAYS AS (message_key::uuid) STORED,
    ADD CONSTRAINT custody_lab_canonical_fk FOREIGN KEY (custody_lab_message_key)
        REFERENCES __SCHEMA__.ingress_messages (message_key) ON DELETE CASCADE;
"#;

/// DDL for a disposable schema only. No runtime grants or migration are installed.
pub fn custody_lab_postgres_ddl(config: &PhysicalAttestationConfig) -> String {
    DDL.replace(
        "__SCHEMA__",
        &quote_identifier(&config.postgres().schema.name.0),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyBinding {
    pub admission_id: uuid::Uuid,
    pub connection: FixtureConnection,
    pub ordinal: u64,
    pub h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustodyDisposition {
    Ack,
    Transfer,
    Tombstone,
}

impl CustodyDisposition {
    fn value(self) -> i32 {
        match self {
            Self::Ack => 1,
            Self::Transfer => 2,
            Self::Tombstone => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustodyRetention {
    Missing,
    Blocked,
    Adopted,
    Retained,
    Deleted,
}

#[derive(Debug, Error)]
pub enum CustodyLabError {
    #[error("the exact principal, claim, fixture gate, or admission was rejected")]
    Rejected,
    #[error("the immutable fixture evidence conflicts")]
    Conflict,
    #[error("the fixture output ordinal is exhausted")]
    Overflow,
    #[error("the canonical parent is missing; reconciliation is unresolved")]
    ReconciliationUnresolved,
    #[error(transparent)]
    Admission(#[from] DirectAdmissionLabError),
    #[error(transparent)]
    Authority(#[from] DarkIngressAuthorityError),
    #[error(transparent)]
    Uow(#[from] IngressUowError),
}

/// Only a permanently sealed gate can prove absence against its old token.
#[derive(Clone)]
pub struct SealedCustodyGate {
    token: uuid::Uuid,
}

/// Synthetic connection witness plus the real principal and SM claim input.
#[derive(Clone)]
pub struct CustodyLabIdentity {
    pub principal: AuthenticatedPrincipalRef,
    pub owner: NodeIdentity,
    pub claim_epoch: ClaimEpoch,
    pub connection: FixtureConnection,
    pub token: uuid::Uuid,
}

/// Closed test-only laboratory using a physically attested PostgreSQL UOW.
/// The gate and terminal observations are synthetic and authorize no I/O.
pub struct CustodyLab {
    uow: IngressUnitOfWork,
    schema: String,
    principal: AuthenticatedPrincipalRef,
    owner: NodeIdentity,
    claim_epoch: ClaimEpoch,
    connection: FixtureConnection,
    token: uuid::Uuid,
    limits: DirectAdmissionLimits,
}

impl CustodyLab {
    pub fn new(
        uow: IngressUnitOfWork,
        config: &PhysicalAttestationConfig,
        identity: CustodyLabIdentity,
        limits: DirectAdmissionLimits,
    ) -> Result<Self, CustodyLabError> {
        let CustodyLabIdentity {
            principal,
            owner,
            claim_epoch,
            connection,
            token,
        } = identity;
        if token.is_nil()
            || connection.generation.is_nil()
            || !owner.is_active()
            || principal.bare_jid() != &connection.resource.to_bare()
        {
            return Err(CustodyLabError::Rejected);
        }
        Ok(Self {
            uow,
            schema: config.postgres().schema.name.0.clone(),
            principal,
            owner,
            claim_epoch,
            connection,
            token,
            limits,
        })
    }

    // Epoch -> principal -> exact SM claim -> synthetic output gate. The
    // transaction owns the real node identity guard through commit.
    async fn guarded(
        &self,
        token: uuid::Uuid,
        sealed: bool,
    ) -> Result<IngressUowTransaction<'_>, CustodyLabError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_secs(3), Duration::from_secs(4))
            .await?;
        if tx.receipt_schema() != Some(self.schema.as_str()) {
            return Err(CustodyLabError::Rejected);
        }
        if PrincipalRepository::assert_principal(&mut tx, &self.principal).await?
            != PrincipalAssertion::Asserted
        {
            return Err(CustodyLabError::Rejected);
        }
        let _claim = ClaimRepository::assert_sm_claim(
            &mut tx,
            &self.connection.sm_session,
            &self.owner,
            self.claim_epoch,
        )
        .await?;
        let row = sqlx::query(&format!("SELECT connection_generation, sm_session_id, capability, sealed FROM {}.custody_lab_gate WHERE resource = $1 FOR UPDATE", quote_identifier(&self.schema)))
            .bind(self.connection.resource.to_string()).fetch_optional(pg(&mut tx)?).await.map_err(db)?
            .ok_or(CustodyLabError::Rejected)?;
        let generation: uuid::Uuid = row.try_get(0).map_err(db)?;
        let session: String = row.try_get(1).map_err(db)?;
        let current: uuid::Uuid = row.try_get(2).map_err(db)?;
        let is_sealed: bool = row.try_get(3).map_err(db)?;
        if generation != self.connection.generation
            || session != self.connection.sm_session.as_str()
            || current != token
            || is_sealed != sealed
        {
            return Err(CustodyLabError::Rejected);
        }
        Ok(tx)
    }

    /// Commit the exact full payload and start the exact attempt atomically.
    pub async fn admit(
        &self,
        admission: &DirectAdmission,
        lease: &SendLease,
    ) -> Result<DirectAdmissionOutcome, CustodyLabError> {
        if admission.connection != self.connection
            || admission.owner != self.owner
            || admission.pin.key.resource != self.connection.resource
            || admission.pin.admission_id.is_nil()
            || admission.attempt_token.is_nil()
            || !lease.never_started()
            || !identical_admission(
                &DirectAdmission::for_lease(admission.pin.clone(), lease, self.connection.clone())?,
                admission,
            )
        {
            return Err(CustodyLabError::Rejected);
        }
        let payload =
            encode_payload(&admission.pin.payload).map_err(|_| CustodyLabError::Rejected)?;
        if payload.len() > MAX_FRAME_BYTES {
            return Err(DirectAdmissionLabError::Capacity.into());
        }
        let mut tx = self.guarded(self.token, false).await?;
        let outcome = start_postgres(
            pg(&mut tx)?,
            &self.schema,
            admission,
            lease,
            &payload,
            self.limits,
            PostgresLabTables::Custody,
        )
        .await?;
        sqlx::query(&format!(
            "UPDATE {}.ingress_messages SET retention_eligible_at = NULL WHERE message_key = $1",
            quote_identifier(&self.schema)
        ))
        .bind(admission.pin.key.message_key.to_storage())
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        tx.commit().await?;
        Ok(outcome)
    }

    /// Seal and rotate a gate before relying on an absent lookup. This waits
    /// for every earlier holder and prevents any later use of the old token.
    pub async fn seal(&self, next: uuid::Uuid) -> Result<SealedCustodyGate, CustodyLabError> {
        if next.is_nil() || next == self.token {
            return Err(CustodyLabError::Rejected);
        }
        let mut tx = match self.guarded(self.token, false).await {
            Ok(tx) => tx,
            Err(CustodyLabError::Rejected) => {
                let mut retry = self.guarded(next, true).await?;
                let previous: Option<uuid::Uuid> = sqlx::query_scalar(&format!(
                    "SELECT previous_capability FROM {}.custody_lab_gate WHERE resource = $1",
                    quote_identifier(&self.schema)
                ))
                .bind(self.connection.resource.to_string())
                .fetch_one(pg(&mut retry)?)
                .await
                .map_err(db)?;
                if previous != Some(self.token) {
                    return Err(CustodyLabError::Rejected);
                }
                retry.commit().await?;
                return Ok(SealedCustodyGate { token: next });
            }
            Err(error) => return Err(error),
        };
        sqlx::query(&format!(
            "UPDATE {}.custody_lab_gate SET previous_capability = capability, capability = $2, sealed = TRUE WHERE resource = $1",
            quote_identifier(&self.schema)
        ))
        .bind(self.connection.resource.to_string())
        .bind(next)
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        tx.commit().await?;
        Ok(SealedCustodyGate { token: next })
    }

    /// Read back the full exact admission after sealing; a missing parent is
    /// unresolved rather than proof that the original transaction rolled back.
    pub async fn lookup_sealed(
        &self,
        sealed: &SealedCustodyGate,
        expected: &DirectAdmission,
    ) -> Result<Option<DirectAdmission>, CustodyLabError> {
        if expected.connection != self.connection
            || expected.owner != self.owner
            || expected.pin.key.resource != self.connection.resource
        {
            return Err(CustodyLabError::Rejected);
        }
        let mut tx = self.guarded(sealed.token, true).await?;
        lock_canonical(pg(&mut tx)?, &self.schema, expected.pin.key.message_key).await?;
        let found = lookup_postgres(
            pg(&mut tx)?,
            &self.schema,
            expected.pin.admission_id,
            PostgresLabTables::Custody,
        )
        .await?;
        if found
            .as_ref()
            .is_some_and(|actual| !identical_admission(actual, expected))
        {
            return Err(CustodyLabError::Conflict);
        }
        if found.is_none() {
            let conflict: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM {}.custody_lab_admission WHERE attempt_token = $1 OR (message_key = $2 AND receipt_kind = $3 AND semantic_identity_hash = $4 AND resource = $5))",
                quote_identifier(&self.schema)
            )).bind(expected.attempt_token).bind(expected.pin.key.message_key.to_storage())
                .bind(expected.pin.key.kind.to_storage()).bind(expected.pin.key.semantic_identity_hash.as_slice())
                .bind(expected.pin.key.resource.to_string()).fetch_one(pg(&mut tx)?).await.map_err(db)?;
            if conflict {
                return Err(CustodyLabError::Conflict);
            }
        }
        tx.commit().await?;
        Ok(found)
    }

    /// Bind before the synthetic counter may advance. The payload remains in
    /// both the immutable admission and the fixture's real SM append row.
    pub async fn bind(
        &self,
        admission: &DirectAdmission,
    ) -> Result<CustodyBinding, CustodyLabError> {
        let mut tx = self.guarded(self.token, false).await?;
        self.require_admission(&mut tx, admission).await?;
        let q = quote_identifier(&self.schema);
        self.require_pending(&mut tx, admission.pin.admission_id)
            .await?;
        let existing = sqlx::query(&format!("SELECT sm_session_id, connection_generation, ordinal FROM {q}.custody_lab_binding WHERE admission_id = $1"))
            .bind(admission.pin.admission_id).fetch_optional(pg(&mut tx)?).await.map_err(db)?;
        let binding = if let Some(row) = existing {
            binding_from_row(admission, row)?
        } else {
            let ordinal: i64 = sqlx::query_scalar(&format!(
                "SELECT ordinal FROM {q}.custody_lab_gate WHERE resource = $1"
            ))
            .bind(self.connection.resource.to_string())
            .fetch_one(pg(&mut tx)?)
            .await
            .map_err(db)?;
            let ordinal = ordinal.checked_add(1).ok_or(CustodyLabError::Overflow)?;
            let h = ordinal as u32;
            let payload =
                encode_payload(&admission.pin.payload).map_err(|_| CustodyLabError::Rejected)?;
            sqlx::query(&format!("INSERT INTO {q}.custody_lab_binding (admission_id, sm_session_id, connection_generation, ordinal) VALUES ($1, $2, $3, $4)"))
                .bind(admission.pin.admission_id).bind(self.connection.sm_session.as_str()).bind(self.connection.generation).bind(ordinal).execute(pg(&mut tx)?).await.map_err(db)?;
            sqlx::query(&format!("INSERT INTO {q}.sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 0)"))
                .bind(admission.pin.key.message_key.to_storage().to_string()).bind(admission.pin.key.kind.to_storage()).bind(admission.pin.key.semantic_identity_hash.as_slice()).bind(self.connection.resource.to_string()).bind(self.connection.sm_session.as_str()).bind(i64::from(h)).bind(Utc::now().timestamp_millis()).bind(&payload).bind(admission.pin.received_at.timestamp_millis()).execute(pg(&mut tx)?).await.map_err(db)?;
            sqlx::query(&format!(
                "UPDATE {q}.custody_lab_gate SET ordinal = $2 WHERE resource = $1"
            ))
            .bind(self.connection.resource.to_string())
            .bind(ordinal)
            .execute(pg(&mut tx)?)
            .await
            .map_err(db)?;
            CustodyBinding {
                admission_id: admission.pin.admission_id,
                connection: self.connection.clone(),
                ordinal: ordinal as u64,
                h,
            }
        };
        tx.commit().await?;
        Ok(binding)
    }

    /// Record only a synthetic count after an exact committed binding.
    pub async fn count(
        &self,
        admission: &DirectAdmission,
        binding: &CustodyBinding,
    ) -> Result<(), CustodyLabError> {
        let mut tx = self.guarded(self.token, false).await?;
        self.require_binding(&mut tx, admission, binding).await?;
        self.require_pending(&mut tx, admission.pin.admission_id)
            .await?;
        sqlx::query(&format!(
            "INSERT INTO {}.custody_lab_counted (admission_id) VALUES ($1) ON CONFLICT DO NOTHING",
            quote_identifier(&self.schema)
        ))
        .bind(binding.admission_id)
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        tx.commit().await?;
        Ok(())
    }

    /// Record synthetic terminal evidence. Transfer copies the full payload in
    /// the same transaction. The last blocker starts a fresh retention tail.
    pub async fn settle(
        &self,
        admission: &DirectAdmission,
        binding: &CustodyBinding,
        disposition: CustodyDisposition,
        now: DateTime<Utc>,
    ) -> Result<(), CustodyLabError> {
        let mut tx = self.guarded(self.token, false).await?;
        self.require_binding(&mut tx, admission, binding).await?;
        let q = quote_identifier(&self.schema);
        if disposition == CustodyDisposition::Ack {
            let counted: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM {q}.custody_lab_counted WHERE admission_id = $1)"
            ))
            .bind(binding.admission_id)
            .fetch_one(pg(&mut tx)?)
            .await
            .map_err(db)?;
            if !counted {
                return Err(CustodyLabError::Rejected);
            }
        }
        let existing: Option<i32> = sqlx::query_scalar(&format!(
            "SELECT disposition FROM {q}.custody_lab_terminal WHERE admission_id = $1"
        ))
        .bind(binding.admission_id)
        .fetch_optional(pg(&mut tx)?)
        .await
        .map_err(db)?;
        if let Some(existing) = existing {
            if existing != disposition.value() {
                return Err(CustodyLabError::Conflict);
            }
            tx.commit().await?;
            return Ok(());
        }
        if disposition == CustodyDisposition::Transfer {
            let payload =
                encode_payload(&admission.pin.payload).map_err(|_| CustodyLabError::Rejected)?;
            sqlx::query(&format!("INSERT INTO {q}.custody_lab_transfer (admission_id, resource, payload) VALUES ($1, $2, $3)"))
                .bind(binding.admission_id).bind(self.connection.resource.to_string()).bind(payload).execute(pg(&mut tx)?).await.map_err(db)?;
        }
        let changed = sqlx::query(&format!("UPDATE {q}.sm_ingress_appends SET disposition = $5 WHERE message_key = $1 AND receipt_kind = $2 AND semantic_identity_hash = $3 AND resource = $4 AND disposition = 0"))
            .bind(admission.pin.key.message_key.to_storage().to_string()).bind(admission.pin.key.kind.to_storage()).bind(admission.pin.key.semantic_identity_hash.as_slice()).bind(self.connection.resource.to_string()).bind(disposition.value()).execute(pg(&mut tx)?).await.map_err(db)?.rows_affected();
        if changed != 1 {
            return Err(CustodyLabError::Conflict);
        }
        sqlx::query(&format!(
            "INSERT INTO {q}.custody_lab_terminal (admission_id, disposition) VALUES ($1, $2)"
        ))
        .bind(binding.admission_id)
        .bind(disposition.value())
        .execute(pg(&mut tx)?)
        .await
        .map_err(db)?;
        let blocked =
            has_custody(pg(&mut tx)?, &self.schema, admission.pin.key.message_key).await?;
        if !blocked {
            sqlx::query(&format!(
                "UPDATE {q}.ingress_messages SET retention_eligible_at = $2 WHERE message_key = $1"
            ))
            .bind(admission.pin.key.message_key.to_storage())
            .bind(now)
            .execute(pg(&mut tx)?)
            .await
            .map_err(db)?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// A narrow synthetic collector. It models no other production blockers.
    pub async fn collect(
        &self,
        message: MessageKey,
        now: DateTime<Utc>,
    ) -> Result<CustodyRetention, CustodyLabError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_secs(3), Duration::from_secs(4))
            .await?;
        if tx.receipt_schema() != Some(self.schema.as_str()) {
            return Err(CustodyLabError::Rejected);
        }
        let q = quote_identifier(&self.schema);
        let row = sqlx::query(&format!("SELECT terminal_at, retention_eligible_at FROM {q}.ingress_messages WHERE message_key = $1 FOR UPDATE"))
            .bind(message.to_storage()).fetch_optional(pg(&mut tx)?).await.map_err(db)?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(CustodyRetention::Missing);
        };
        let terminal: Option<DateTime<Utc>> = row.try_get(0).map_err(db)?;
        let eligible: Option<DateTime<Utc>> = row.try_get(1).map_err(db)?;
        let result = if terminal.is_none()
            || has_custody(pg(&mut tx)?, &self.schema, message).await?
        {
            sqlx::query(&format!("UPDATE {q}.ingress_messages SET retention_eligible_at = NULL WHERE message_key = $1"))
                .bind(message.to_storage()).execute(pg(&mut tx)?).await.map_err(db)?;
            CustodyRetention::Blocked
        } else if eligible.is_none() {
            sqlx::query(&format!(
                "UPDATE {q}.ingress_messages SET retention_eligible_at = $2 WHERE message_key = $1"
            ))
            .bind(message.to_storage())
            .bind(now)
            .execute(pg(&mut tx)?)
            .await
            .map_err(db)?;
            CustodyRetention::Adopted
        } else if eligible.is_some_and(|at| at > now - ALIAS_RETENTION) {
            CustodyRetention::Retained
        } else {
            for table in ["ingress_origin_aliases", "ingress_deliveries"] {
                sqlx::query(&format!("DELETE FROM {q}.{table} WHERE message_key = $1"))
                    .bind(message.to_storage())
                    .execute(pg(&mut tx)?)
                    .await
                    .map_err(db)?;
            }
            sqlx::query(&format!(
                "DELETE FROM {q}.ingress_messages WHERE message_key = $1"
            ))
            .bind(message.to_storage())
            .execute(pg(&mut tx)?)
            .await
            .map_err(db)?;
            CustodyRetention::Deleted
        };
        tx.commit().await?;
        Ok(result)
    }

    async fn require_pending(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        id: uuid::Uuid,
    ) -> Result<(), CustodyLabError> {
        let terminal: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM {}.custody_lab_terminal WHERE admission_id = $1)",
            quote_identifier(&self.schema)
        ))
        .bind(id)
        .fetch_one(pg(tx)?)
        .await
        .map_err(db)?;
        if terminal {
            return Err(CustodyLabError::Rejected);
        }
        Ok(())
    }

    async fn require_admission(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        expected: &DirectAdmission,
    ) -> Result<(), CustodyLabError> {
        if expected.connection != self.connection
            || expected.owner != self.owner
            || expected.pin.key.resource != self.connection.resource
        {
            return Err(CustodyLabError::Rejected);
        }
        lock_canonical(pg(tx)?, &self.schema, expected.pin.key.message_key).await?;
        let actual = lookup_postgres(
            pg(tx)?,
            &self.schema,
            expected.pin.admission_id,
            PostgresLabTables::Custody,
        )
        .await?
        .ok_or(CustodyLabError::Rejected)?;
        if !identical_admission(&actual, expected) {
            return Err(CustodyLabError::Conflict);
        }
        Ok(())
    }

    async fn require_binding(
        &self,
        tx: &mut IngressUowTransaction<'_>,
        expected: &DirectAdmission,
        binding: &CustodyBinding,
    ) -> Result<(), CustodyLabError> {
        self.require_admission(tx, expected).await?;
        let row = sqlx::query(&format!("SELECT sm_session_id, connection_generation, ordinal FROM {}.custody_lab_binding WHERE admission_id = $1", quote_identifier(&self.schema)))
            .bind(expected.pin.admission_id).fetch_optional(pg(tx)?).await.map_err(db)?.ok_or(CustodyLabError::Rejected)?;
        if binding_from_row(expected, row)? != *binding {
            return Err(CustodyLabError::Conflict);
        }
        let payload =
            encode_payload(&expected.pin.payload).map_err(|_| CustodyLabError::Rejected)?;
        let matches_append: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM {}.sm_ingress_appends WHERE message_key = $1 AND receipt_kind = $2 AND semantic_identity_hash = $3 AND resource = $4 AND accepting_stream_id = $5 AND sequence = $6 AND custody_payload = $7 AND original_receipt_at_ms = $8)",
            quote_identifier(&self.schema)
        )).bind(expected.pin.key.message_key.to_storage().to_string())
            .bind(expected.pin.key.kind.to_storage()).bind(expected.pin.key.semantic_identity_hash.as_slice())
            .bind(self.connection.resource.to_string()).bind(self.connection.sm_session.as_str())
            .bind(i64::from(binding.h)).bind(payload).bind(expected.pin.received_at.timestamp_millis())
            .fetch_one(pg(tx)?).await.map_err(db)?;
        if !matches_append {
            return Err(CustodyLabError::Conflict);
        }
        Ok(())
    }
}

fn pg<'a>(tx: &'a mut IngressUowTransaction<'_>) -> Result<&'a mut PgConnection, CustodyLabError> {
    tx.transaction_mut()
        .postgres_connection()
        .ok_or(CustodyLabError::Rejected)
}

fn db(error: sqlx::Error) -> CustodyLabError {
    super::super::super::postgres_error(error).into()
}

async fn lock_canonical(
    tx: &mut PgConnection,
    schema: &str,
    message: MessageKey,
) -> Result<(), CustodyLabError> {
    sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_messages WHERE message_key = $1 FOR UPDATE",
        quote_identifier(schema)
    ))
    .bind(message.to_storage())
    .fetch_optional(tx)
    .await
    .map_err(db)?
    .ok_or(CustodyLabError::ReconciliationUnresolved)?;
    Ok(())
}

async fn has_custody(
    tx: &mut PgConnection,
    schema: &str,
    message: MessageKey,
) -> Result<bool, CustodyLabError> {
    let q = quote_identifier(schema);
    sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM {q}.custody_lab_admission a WHERE a.message_key = $1 AND NOT EXISTS (SELECT 1 FROM {q}.custody_lab_terminal t WHERE t.admission_id = a.admission_id)) OR EXISTS (SELECT 1 FROM {q}.sm_ingress_appends s WHERE s.custody_lab_message_key = $1 AND s.disposition = 0)"))
        .bind(message.to_storage()).fetch_one(tx).await.map_err(db)
}

fn binding_from_row(
    admission: &DirectAdmission,
    row: sqlx::postgres::PgRow,
) -> Result<CustodyBinding, CustodyLabError> {
    let session: String = row.try_get(0).map_err(db)?;
    let generation: uuid::Uuid = row.try_get(1).map_err(db)?;
    let ordinal: i64 = row.try_get(2).map_err(db)?;
    if session != admission.connection.sm_session.as_str()
        || generation != admission.connection.generation
        || ordinal <= 0
    {
        return Err(CustodyLabError::Conflict);
    }
    Ok(CustodyBinding {
        admission_id: admission.pin.admission_id,
        connection: admission.connection.clone(),
        ordinal: ordinal as u64,
        h: ordinal as u32,
    })
}
