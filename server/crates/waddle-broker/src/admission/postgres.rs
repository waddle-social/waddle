//! PostgreSQL (`PgPool` + `SELECT ... FOR UPDATE`) and in-process `SqliteCompat`
//! (`SqlitePool`) implementations of the 3-fence `AdmissionLedgerStore`,
//! `ReplicaFenceStore`, and `AdmissionQueryStore`.

use async_trait::async_trait;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{PgPool, Postgres, Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;
use waddle_broker_core::admission::{
    AdmissionError, AdmissionLedgerStore, AdmitAttemptRequest, AdmitReceipt, AttemptId,
    AttemptOutcome, CredentialKey, FenceKey, FenceKind, FenceSnapshot, FenceState, RoomScopeKey,
    RunId, StepId, ThreeFenceSnapshot,
};
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::kernels::budget_ledger::{BudgetError, BudgetState};
use waddle_broker_core::kernels::three_fence_guard::{
    close_and_advance, evaluate_admission, reopen_fence, AdmissionCheckInput, FenceGuardError,
    KernelFenceState, KernelThreeFenceSnapshot, KernelThreeFenceState,
};
use waddle_broker_core::placement::FenceGeneration;

use crate::admission::{
    decode_u64, encode_u64, fence_kind_and_scope, parse_fence_kind_and_scope, AdmissionQueryStore,
    AttemptRecord, AttemptStatus, FenceAcknowledgement, ReplicaFenceStore, RunBudgetSnapshot,
};

const PG_SCHEMA: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS admission_fences (
        tenant_id TEXT NOT NULL,
        fence_kind TEXT NOT NULL CHECK (fence_kind IN ('credential', 'room_or_space', 'placement')),
        fence_scope TEXT NOT NULL,
        generation BIGINT NOT NULL CHECK (generation >= 1),
        state TEXT NOT NULL CHECK (state IN ('open', 'closed')),
        updated_at_ms BIGINT NOT NULL,
        PRIMARY KEY (tenant_id, fence_kind, fence_scope)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS admission_runs (
        run_id TEXT PRIMARY KEY,
        tenant_id TEXT NOT NULL,
        max_spend_microcents TEXT NOT NULL,
        spent_microcents TEXT NOT NULL,
        reserved_microcents TEXT NOT NULL,
        max_steps INTEGER NOT NULL,
        steps_used INTEGER NOT NULL,
        max_input_tokens TEXT NOT NULL,
        input_tokens_used TEXT NOT NULL,
        max_output_tokens TEXT NOT NULL,
        output_tokens_used TEXT NOT NULL,
        created_at_ms BIGINT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS admission_attempts (
        attempt_id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL,
        step_id INTEGER NOT NULL,
        tenant_id TEXT NOT NULL,
        credential_scope TEXT NOT NULL,
        room_scope TEXT NOT NULL,
        holder_spiffe_id TEXT NOT NULL,
        holder_incarnation_pubkey BYTEA NOT NULL,
        nonce_bytes BYTEA NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('admitted', 'completed', 'failed_before_send', 'canceled', 'quarantined')),
        reserved_spend_microcents TEXT NOT NULL,
        actual_spend_microcents TEXT NOT NULL,
        input_tokens TEXT NOT NULL,
        output_tokens TEXT NOT NULL,
        admitted_at_ms BIGINT NOT NULL,
        UNIQUE (run_id, step_id),
        UNIQUE (tenant_id, nonce_bytes)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS admission_fence_replicas (
        replica_id TEXT PRIMARY KEY,
        registered_at_ms BIGINT NOT NULL,
        lease_expires_at_ms BIGINT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS admission_fence_acks (
        replica_id TEXT NOT NULL,
        tenant_id TEXT NOT NULL,
        fence_kind TEXT NOT NULL,
        fence_scope TEXT NOT NULL,
        generation BIGINT NOT NULL,
        acknowledged_at_ms BIGINT NOT NULL,
        PRIMARY KEY (replica_id, tenant_id, fence_kind, fence_scope)
    )"#,
];

const SQLITE_SCHEMA: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS admission_fences (
        tenant_id TEXT NOT NULL,
        fence_kind TEXT NOT NULL CHECK (fence_kind IN ('credential', 'room_or_space', 'placement')),
        fence_scope TEXT NOT NULL,
        generation INTEGER NOT NULL CHECK (generation >= 1),
        state TEXT NOT NULL CHECK (state IN ('open', 'closed')),
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (tenant_id, fence_kind, fence_scope)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS admission_runs (
        run_id TEXT PRIMARY KEY,
        tenant_id TEXT NOT NULL,
        max_spend_microcents TEXT NOT NULL,
        spent_microcents TEXT NOT NULL,
        reserved_microcents TEXT NOT NULL,
        max_steps INTEGER NOT NULL,
        steps_used INTEGER NOT NULL,
        max_input_tokens TEXT NOT NULL,
        input_tokens_used TEXT NOT NULL,
        max_output_tokens TEXT NOT NULL,
        output_tokens_used TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS admission_attempts (
        attempt_id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL,
        step_id INTEGER NOT NULL,
        tenant_id TEXT NOT NULL,
        credential_scope TEXT NOT NULL,
        room_scope TEXT NOT NULL,
        holder_spiffe_id TEXT NOT NULL,
        holder_incarnation_pubkey BLOB NOT NULL,
        nonce_bytes BLOB NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('admitted', 'completed', 'failed_before_send', 'canceled', 'quarantined')),
        reserved_spend_microcents TEXT NOT NULL,
        actual_spend_microcents TEXT NOT NULL,
        input_tokens TEXT NOT NULL,
        output_tokens TEXT NOT NULL,
        admitted_at_ms INTEGER NOT NULL,
        UNIQUE (run_id, step_id),
        UNIQUE (tenant_id, nonce_bytes)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS admission_fence_replicas (
        replica_id TEXT PRIMARY KEY,
        registered_at_ms INTEGER NOT NULL,
        lease_expires_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS admission_fence_acks (
        replica_id TEXT NOT NULL,
        tenant_id TEXT NOT NULL,
        fence_kind TEXT NOT NULL,
        fence_scope TEXT NOT NULL,
        generation INTEGER NOT NULL,
        acknowledged_at_ms INTEGER NOT NULL,
        PRIMARY KEY (replica_id, tenant_id, fence_kind, fence_scope)
    )"#,
];

fn to_kernel_fence(snap: FenceSnapshot) -> KernelFenceState {
    KernelFenceState {
        open: snap.state == FenceState::Open,
        generation: snap.generation.get(),
    }
}

fn map_budget_err(err: BudgetError) -> AdmissionError {
    match err {
        BudgetError::CapExceeded | BudgetError::Overflow => AdmissionError::BudgetExceeded,
        BudgetError::ReservedUnderflow | BudgetError::ActualExceedsReserved => {
            AdmissionError::Backend(err.to_string())
        }
    }
}

fn parse_fence_state(raw: &str) -> Result<FenceState, AdmissionError> {
    match raw {
        "open" => Ok(FenceState::Open),
        "closed" => Ok(FenceState::Closed),
        other => Err(AdmissionError::Backend(format!(
            "invalid fence state: {other}"
        ))),
    }
}

// ============================================================================
// PostgreSQL implementation
// ============================================================================

/// Production PostgreSQL 3-fence `AdmissionLedgerStore` using `SELECT ... FOR UPDATE`.
#[derive(Clone)]
pub struct PostgresAdmissionLedgerStore {
    pool: PgPool,
}

impl PostgresAdmissionLedgerStore {
    pub async fn new(pool: PgPool) -> Result<Self, AdmissionError> {
        for stmt in PG_SCHEMA {
            sqlx::query(stmt).execute(&pool).await.map_err(|err| {
                AdmissionError::Backend(format!("PG admission DDL failed: {err}"))
            })?;
        }
        Ok(Self { pool })
    }

    async fn pg_now_ms(tx: &mut Transaction<'_, Postgres>) -> Result<i64, AdmissionError> {
        let row = sqlx::query("SELECT (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT")
            .fetch_one(&mut **tx)
            .await
            .map_err(|err| AdmissionError::Backend(format!("PG clock query failed: {err}")))?;
        row.try_get(0)
            .map_err(|err| AdmissionError::Backend(format!("PG clock decode failed: {err}")))
    }

    async fn pg_load_or_init_fence(
        tx: &mut Transaction<'_, Postgres>,
        tenant: &TenantId,
        kind: &FenceKind,
        now_ms: i64,
    ) -> Result<FenceSnapshot, AdmissionError> {
        let (kind_tag, scope) = fence_kind_and_scope(kind);
        sqlx::query(
            "INSERT INTO admission_fences (tenant_id, fence_kind, fence_scope, generation, state, updated_at_ms) VALUES ($1, $2, $3, 1, 'open', $4) ON CONFLICT (tenant_id, fence_kind, fence_scope) DO NOTHING",
        )
        .bind(tenant.as_str())
        .bind(kind_tag)
        .bind(&scope)
        .bind(now_ms)
        .execute(&mut **tx)
        .await
        .map_err(|err| AdmissionError::Backend(format!("PG fence init failed: {err}")))?;

        let row = sqlx::query(
            "SELECT generation, state FROM admission_fences WHERE tenant_id = $1 AND fence_kind = $2 AND fence_scope = $3 FOR UPDATE",
        )
        .bind(tenant.as_str())
        .bind(kind_tag)
        .bind(&scope)
        .fetch_one(&mut **tx)
        .await
        .map_err(|err| AdmissionError::Backend(format!("PG fence lock failed: {err}")))?;

        let gen_i64: i64 = row
            .try_get(0)
            .map_err(|err| AdmissionError::Backend(format!("PG gen decode failed: {err}")))?;
        let state_str: String = row
            .try_get(1)
            .map_err(|err| AdmissionError::Backend(format!("PG state decode failed: {err}")))?;
        let gen_u64 = u64::try_from(gen_i64)
            .map_err(|_| AdmissionError::Backend("negative PG fence generation".to_owned()))?;
        Ok(FenceSnapshot {
            generation: FenceGeneration::new(gen_u64),
            state: parse_fence_state(&state_str)?,
        })
    }

    async fn pg_load_all_fences(
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<(FenceKey, FenceGeneration)>, AdmissionError> {
        let rows = sqlx::query(
            "SELECT tenant_id, fence_kind, fence_scope, generation FROM admission_fences",
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(|err| AdmissionError::Backend(format!("PG list fences failed: {err}")))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let tenant_str: String = row
                .try_get(0)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            let kind_str: String = row
                .try_get(1)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            let scope_str: String = row
                .try_get(2)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            let gen_i64: i64 = row
                .try_get(3)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            let tenant_id =
                TenantId::parse(&tenant_str).map_err(|e| AdmissionError::Backend(e.to_string()))?;
            let kind = parse_fence_kind_and_scope(&kind_str, &scope_str)?;
            let gen_u64 = u64::try_from(gen_i64)
                .map_err(|_| AdmissionError::Backend("negative generation".to_owned()))?;
            out.push((FenceKey { tenant_id, kind }, FenceGeneration::new(gen_u64)));
        }
        Ok(out)
    }
}

#[async_trait]
impl AdmissionLedgerStore for PostgresAdmissionLedgerStore {
    async fn snapshot_fences(
        &self,
        tenant: &TenantId,
        credential: &CredentialKey,
        room_or_space: &RoomScopeKey,
    ) -> Result<ThreeFenceSnapshot, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| AdmissionError::Backend(format!("PG begin failed: {err}")))?;
        let now_ms = Self::pg_now_ms(&mut tx).await?;
        let cred = Self::pg_load_or_init_fence(
            &mut tx,
            tenant,
            &FenceKind::Credential(credential.clone()),
            now_ms,
        )
        .await?;
        let room = Self::pg_load_or_init_fence(
            &mut tx,
            tenant,
            &FenceKind::RoomOrSpace(room_or_space.clone()),
            now_ms,
        )
        .await?;
        let placement =
            Self::pg_load_or_init_fence(&mut tx, tenant, &FenceKind::Placement, now_ms).await?;
        tx.commit()
            .await
            .map_err(|err| AdmissionError::Backend(format!("PG commit failed: {err}")))?;
        Ok(ThreeFenceSnapshot {
            credential: cred,
            room_or_space: room,
            placement,
        })
    }

    async fn close_and_advance_fence(
        &self,
        key: &FenceKey,
    ) -> Result<FenceGeneration, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| AdmissionError::Backend(format!("PG begin failed: {err}")))?;
        let now_ms = Self::pg_now_ms(&mut tx).await?;
        let current =
            Self::pg_load_or_init_fence(&mut tx, &key.tenant_id, &key.kind, now_ms).await?;
        let next = close_and_advance(to_kernel_fence(current))
            .map_err(|err| AdmissionError::Backend(format!("fence overflow: {err}")))?;
        let next_i64 = i64::try_from(next.generation)
            .map_err(|_| AdmissionError::Backend("generation exceeds BIGINT".to_owned()))?;
        let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
        sqlx::query(
            "UPDATE admission_fences SET generation = $1, state = 'closed', updated_at_ms = $2 WHERE tenant_id = $3 AND fence_kind = $4 AND fence_scope = $5",
        )
        .bind(next_i64)
        .bind(now_ms)
        .bind(key.tenant_id.as_str())
        .bind(kind_tag)
        .bind(&scope)
        .execute(&mut *tx)
        .await
        .map_err(|err| AdmissionError::Backend(format!("PG close update failed: {err}")))?;
        tx.commit()
            .await
            .map_err(|err| AdmissionError::Backend(format!("PG commit failed: {err}")))?;
        Ok(FenceGeneration::new(next.generation))
    }

    async fn reopen_fence(
        &self,
        key: &FenceKey,
        expected_generation: FenceGeneration,
    ) -> Result<(), AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| AdmissionError::Backend(format!("PG begin failed: {err}")))?;
        let now_ms = Self::pg_now_ms(&mut tx).await?;
        let current =
            Self::pg_load_or_init_fence(&mut tx, &key.tenant_id, &key.kind, now_ms).await?;
        let _ =
            reopen_fence(to_kernel_fence(current), expected_generation.get()).map_err(|err| {
                match err {
                    FenceGuardError::GenerationMismatch | FenceGuardError::AlreadyOpen => {
                        AdmissionError::FenceStaleOrClosed(key.kind.clone())
                    }
                    FenceGuardError::GenerationOverflow => AdmissionError::Backend(err.to_string()),
                }
            })?;
        let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
        sqlx::query(
            "UPDATE admission_fences SET state = 'open', updated_at_ms = $1 WHERE tenant_id = $2 AND fence_kind = $3 AND fence_scope = $4",
        )
        .bind(now_ms)
        .bind(key.tenant_id.as_str())
        .bind(kind_tag)
        .bind(&scope)
        .execute(&mut *tx)
        .await
        .map_err(|err| AdmissionError::Backend(format!("PG reopen update failed: {err}")))?;
        tx.commit()
            .await
            .map_err(|err| AdmissionError::Backend(format!("PG commit failed: {err}")))?;
        Ok(())
    }

    async fn admit_attempt(
        &self,
        req: &AdmitAttemptRequest,
    ) -> Result<AdmitReceipt, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| AdmissionError::Backend(format!("PG begin failed: {err}")))?;
        let now_ms = Self::pg_now_ms(&mut tx).await?;
        if now_ms >= req.expires_at_ms {
            return Err(AdmissionError::InvocationExpired);
        }
        if req.holder.spiffe_id.as_str().is_empty() {
            return Err(AdmissionError::HolderUnauthorized);
        }

        let cred_kind = FenceKind::Credential(req.credential.clone());
        let room_kind = FenceKind::RoomOrSpace(req.room_or_space.clone());
        let cred_cur =
            Self::pg_load_or_init_fence(&mut tx, &req.tenant_id, &cred_kind, now_ms).await?;
        let room_cur =
            Self::pg_load_or_init_fence(&mut tx, &req.tenant_id, &room_kind, now_ms).await?;
        let place_cur =
            Self::pg_load_or_init_fence(&mut tx, &req.tenant_id, &FenceKind::Placement, now_ms)
                .await?;

        let check_input = AdmissionCheckInput {
            snapshot: KernelThreeFenceSnapshot {
                cred_gen: req.expected_fences.credential.generation.get(),
                room_gen: req.expected_fences.room_or_space.generation.get(),
                placement_gen: req.expected_fences.placement.generation.get(),
            },
            current: KernelThreeFenceState {
                cred: to_kernel_fence(cred_cur),
                room: to_kernel_fence(room_cur),
                placement: to_kernel_fence(place_cur),
            },
            expected_holder_id: 1,
            actual_holder_id: 1,
            now_ms,
            lease_expiry_ms: req.expires_at_ms,
            perm_allowed: true,
        };
        if !evaluate_admission(&check_input) {
            if cred_cur.state != FenceState::Open
                || cred_cur.generation != req.expected_fences.credential.generation
            {
                return Err(AdmissionError::FenceStaleOrClosed(cred_kind));
            }
            if room_cur.state != FenceState::Open
                || room_cur.generation != req.expected_fences.room_or_space.generation
            {
                return Err(AdmissionError::FenceStaleOrClosed(room_kind));
            }
            return Err(AdmissionError::FenceStaleOrClosed(FenceKind::Placement));
        }

        let attempt_id_str = req.attempt_id.as_uuid().to_string();
        let run_id_str = req.run_id.as_uuid().to_string();
        let step_i32 =
            i32::try_from(req.step_id.get()).map_err(|_| AdmissionError::BudgetExceeded)?;

        if let Some(row) =
            sqlx::query("SELECT status FROM admission_attempts WHERE attempt_id = $1")
                .bind(&attempt_id_str)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| AdmissionError::Backend(e.to_string()))?
        {
            let status_s: String = row
                .try_get(0)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            if status_s == AttemptStatus::Quarantined.as_str() {
                return Err(AdmissionError::Quarantined);
            }
            return Err(AdmissionError::DuplicateAttempt);
        }

        if let Some(row) = sqlx::query(
            "SELECT status FROM admission_attempts WHERE tenant_id = $1 AND nonce_bytes = $2",
        )
        .bind(req.tenant_id.as_str())
        .bind(req.invocation_nonce.as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?
        {
            let status_s: String = row
                .try_get(0)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            if status_s == AttemptStatus::Quarantined.as_str() {
                return Err(AdmissionError::Quarantined);
            }
            return Err(AdmissionError::NonceReplayed);
        }

        if sqlx::query("SELECT 1 FROM admission_attempts WHERE run_id = $1 AND step_id = $2")
            .bind(&run_id_str)
            .bind(step_i32)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?
            .is_some()
        {
            return Err(AdmissionError::DuplicateAttempt);
        }

        sqlx::query(
            "INSERT INTO admission_runs (run_id, tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used, created_at_ms) VALUES ($1, $2, $3, '0', '0', $4, 0, $5, '0', $6, '0', $7) ON CONFLICT (run_id) DO NOTHING",
        )
        .bind(&run_id_str)
        .bind(req.tenant_id.as_str())
        .bind(encode_u64(req.budget.max_spend_microcents))
        .bind(i32::try_from(req.budget.max_steps).unwrap_or(i32::MAX))
        .bind(encode_u64(req.budget.max_input_tokens))
        .bind(encode_u64(req.budget.max_output_tokens))
        .bind(now_ms)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        let run_row = sqlx::query(
            "SELECT tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used FROM admission_runs WHERE run_id = $1 FOR UPDATE",
        )
        .bind(&run_id_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let row_tenant_id: String = run_row
            .try_get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        if row_tenant_id != req.tenant_id.as_str() {
            return Err(AdmissionError::HolderUnauthorized);
        }
        let cap_s: String = run_row
            .try_get(1)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let spent_s: String = run_row
            .try_get(2)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let res_s: String = run_row
            .try_get(3)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_steps: i32 = run_row
            .try_get(4)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let steps_used: i32 = run_row
            .try_get(5)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        if steps_used >= max_steps {
            return Err(AdmissionError::BudgetExceeded);
        }
        let mut budget = BudgetState {
            signed_cap: decode_u64(&cap_s)?,
            spent: decode_u64(&spent_s)?,
            reserved: decode_u64(&res_s)?,
        };
        budget
            .reserve(req.reserved_spend_microcents)
            .map_err(map_budget_err)?;
        let next_steps = steps_used
            .checked_add(1)
            .ok_or(AdmissionError::BudgetExceeded)?;

        sqlx::query(
            "UPDATE admission_runs SET reserved_microcents = $1, steps_used = $2 WHERE run_id = $3",
        )
        .bind(encode_u64(budget.reserved))
        .bind(next_steps)
        .bind(&run_id_str)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        let (_, cred_scope_str) = fence_kind_and_scope(&cred_kind);
        sqlx::query(
            "INSERT INTO admission_attempts (attempt_id, run_id, step_id, tenant_id, credential_scope, room_scope, holder_spiffe_id, holder_incarnation_pubkey, nonce_bytes, status, reserved_spend_microcents, actual_spend_microcents, input_tokens, output_tokens, admitted_at_ms) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'admitted', $10, '0', '0', '0', $11)",
        )
        .bind(&attempt_id_str)
        .bind(&run_id_str)
        .bind(step_i32)
        .bind(req.tenant_id.as_str())
        .bind(&cred_scope_str)
        .bind(req.room_or_space.scope_id.as_str())
        .bind(req.holder.spiffe_id.as_str())
        .bind(req.holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(req.invocation_nonce.as_slice())
        .bind(encode_u64(req.reserved_spend_microcents))
        .bind(now_ms)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|err| AdmissionError::Backend(format!("PG commit failed: {err}")))?;
        Ok(AdmitReceipt {
            attempt_id: req.attempt_id,
            run_id: req.run_id,
            step_id: req.step_id,
            admitted_at_ms: now_ms,
            reserved_spend_microcents: req.reserved_spend_microcents,
        })
    }

    async fn reconcile_attempt(
        &self,
        attempt_id: AttemptId,
        outcome: AttemptOutcome,
    ) -> Result<(), AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| AdmissionError::Backend(format!("PG begin failed: {err}")))?;
        let attempt_id_str = attempt_id.as_uuid().to_string();
        let row = sqlx::query(
            "SELECT run_id, status, reserved_spend_microcents FROM admission_attempts WHERE attempt_id = $1 FOR UPDATE",
        )
        .bind(&attempt_id_str)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?
        .ok_or(AdmissionError::AttemptNotFound)?;
        let run_id_str: String = row
            .try_get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let status_s: String = row
            .try_get(1)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let reserved_s: String = row
            .try_get(2)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let reserved_spend = decode_u64(&reserved_s)?;

        let current_status = AttemptStatus::parse(&status_s)?;
        if current_status == AttemptStatus::Quarantined {
            return Err(AdmissionError::Quarantined);
        }
        if current_status != AttemptStatus::Admitted {
            return Err(AdmissionError::Backend(format!(
                "attempt already reconciled in status {}",
                current_status.as_str()
            )));
        }

        let run_row = sqlx::query(
            "SELECT max_spend_microcents, spent_microcents, reserved_microcents, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used FROM admission_runs WHERE run_id = $1 FOR UPDATE",
        )
        .bind(&run_id_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let cap_s: String = run_row
            .try_get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let spent_s: String = run_row
            .try_get(1)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let res_s: String = run_row
            .try_get(2)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_in_s: String = run_row
            .try_get(3)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let in_s: String = run_row
            .try_get(4)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_out_s: String = run_row
            .try_get(5)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let out_s: String = run_row
            .try_get(6)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        let mut budget = BudgetState {
            signed_cap: decode_u64(&cap_s)?,
            spent: decode_u64(&spent_s)?,
            reserved: decode_u64(&res_s)?,
        };
        let max_in = decode_u64(&max_in_s)?;
        let in_used = decode_u64(&in_s)?;
        let max_out = decode_u64(&max_out_s)?;
        let out_used = decode_u64(&out_s)?;

        let (new_status, actual_spend, new_in, new_out, attempt_in, attempt_out) = match outcome {
            AttemptOutcome::Completed {
                input_tokens,
                output_tokens,
                actual_spend_microcents,
            } => {
                let next_in = in_used
                    .checked_add(input_tokens)
                    .ok_or(AdmissionError::BudgetExceeded)?;
                let next_out = out_used
                    .checked_add(output_tokens)
                    .ok_or(AdmissionError::BudgetExceeded)?;
                if next_in > max_in || next_out > max_out {
                    return Err(AdmissionError::BudgetExceeded);
                }
                budget
                    .commit(reserved_spend, actual_spend_microcents)
                    .map_err(map_budget_err)?;
                (
                    AttemptStatus::Completed,
                    actual_spend_microcents,
                    next_in,
                    next_out,
                    input_tokens,
                    output_tokens,
                )
            }
            AttemptOutcome::FailedBeforeSend => {
                budget.release(reserved_spend).map_err(map_budget_err)?;
                (AttemptStatus::FailedBeforeSend, 0, in_used, out_used, 0, 0)
            }
            AttemptOutcome::Canceled {
                actual_spend_microcents,
            } => {
                budget
                    .commit(reserved_spend, actual_spend_microcents)
                    .map_err(map_budget_err)?;
                (
                    AttemptStatus::Canceled,
                    actual_spend_microcents,
                    in_used,
                    out_used,
                    0,
                    0,
                )
            }
            AttemptOutcome::UnknownAfterSend => {
                budget
                    .quarantine_conservative(reserved_spend)
                    .map_err(map_budget_err)?;
                (
                    AttemptStatus::Quarantined,
                    reserved_spend,
                    in_used,
                    out_used,
                    0,
                    0,
                )
            }
        };

        sqlx::query(
            "UPDATE admission_runs SET spent_microcents = $1, reserved_microcents = $2, input_tokens_used = $3, output_tokens_used = $4 WHERE run_id = $5",
        )
        .bind(encode_u64(budget.spent))
        .bind(encode_u64(budget.reserved))
        .bind(encode_u64(new_in))
        .bind(encode_u64(new_out))
        .bind(&run_id_str)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        sqlx::query(
            "UPDATE admission_attempts SET status = $1, actual_spend_microcents = $2, input_tokens = $3, output_tokens = $4 WHERE attempt_id = $5",
        )
        .bind(new_status.as_str())
        .bind(encode_u64(actual_spend))
        .bind(encode_u64(attempt_in))
        .bind(encode_u64(attempt_out))
        .bind(&attempt_id_str)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|err| AdmissionError::Backend(format!("PG commit failed: {err}")))?;
        Ok(())
    }
}

#[async_trait]
impl ReplicaFenceStore for PostgresAdmissionLedgerStore {
    async fn register_replica(
        &self,
        replica_id: Uuid,
        ttl_ms: i64,
    ) -> Result<Vec<(FenceKey, FenceGeneration)>, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::pg_now_ms(&mut tx).await?;
        let expires_at = now_ms.saturating_add(ttl_ms);
        sqlx::query(
            "INSERT INTO admission_fence_replicas (replica_id, registered_at_ms, lease_expires_at_ms) VALUES ($1, $2, $3) ON CONFLICT (replica_id) DO UPDATE SET lease_expires_at_ms = EXCLUDED.lease_expires_at_ms",
        )
        .bind(replica_id.to_string())
        .bind(now_ms)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let fences = Self::pg_load_all_fences(&mut tx).await?;
        tx.commit()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(fences)
    }

    async fn renew_replica(
        &self,
        replica_id: Uuid,
        ttl_ms: i64,
    ) -> Result<Option<Vec<(FenceKey, FenceGeneration)>>, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::pg_now_ms(&mut tx).await?;
        let expires_at = now_ms.saturating_add(ttl_ms);
        let res = sqlx::query(
            "UPDATE admission_fence_replicas SET lease_expires_at_ms = $1 WHERE replica_id = $2",
        )
        .bind(expires_at)
        .bind(replica_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        if res.rows_affected() == 0 {
            return Ok(None);
        }
        let fences = Self::pg_load_all_fences(&mut tx).await?;
        tx.commit()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(Some(fences))
    }

    async fn ack_replica_fences(
        &self,
        replica_id: Uuid,
        acks: &[(FenceKey, FenceGeneration)],
    ) -> Result<(), AdmissionError> {
        if acks.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::pg_now_ms(&mut tx).await?;
        let id_str = replica_id.to_string();
        for (key, gen) in acks {
            let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
            let gen_i64 = i64::try_from(gen.get())
                .map_err(|_| AdmissionError::Backend("generation exceeds BIGINT".to_owned()))?;
            sqlx::query(
                "INSERT INTO admission_fence_acks (replica_id, tenant_id, fence_kind, fence_scope, generation, acknowledged_at_ms) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (replica_id, tenant_id, fence_kind, fence_scope) DO UPDATE SET generation = GREATEST(admission_fence_acks.generation, EXCLUDED.generation), acknowledged_at_ms = EXCLUDED.acknowledged_at_ms",
            )
            .bind(&id_str)
            .bind(key.tenant_id.as_str())
            .bind(kind_tag)
            .bind(&scope)
            .bind(gen_i64)
            .bind(now_ms)
            .execute(&mut *tx)
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn release_replica(&self, replica_id: Uuid) -> Result<(), AdmissionError> {
        let id_str = replica_id.to_string();
        sqlx::query("DELETE FROM admission_fence_acks WHERE replica_id = $1")
            .bind(&id_str)
            .execute(&self.pool)
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        sqlx::query("DELETE FROM admission_fence_replicas WHERE replica_id = $1")
            .bind(&id_str)
            .execute(&self.pool)
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn check_revocation_ack(
        &self,
        key: &FenceKey,
        target_gen: FenceGeneration,
        expiry_grace_ms: i64,
    ) -> Result<FenceAcknowledgement, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::pg_now_ms(&mut tx).await?;
        let cutoff = now_ms.saturating_sub(expiry_grace_ms);
        let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
        let target_i64 = i64::try_from(target_gen.get())
            .map_err(|_| AdmissionError::Backend("target_gen exceeds BIGINT".to_owned()))?;
        let row = sqlx::query(
            "SELECT COUNT(*) FROM admission_fence_replicas r WHERE r.lease_expires_at_ms > $1 AND NOT EXISTS (SELECT 1 FROM admission_fence_acks a WHERE a.replica_id = r.replica_id AND a.tenant_id = $2 AND a.fence_kind = $3 AND a.fence_scope = $4 AND a.generation >= $5)",
        )
        .bind(cutoff)
        .bind(key.tenant_id.as_str())
        .bind(kind_tag)
        .bind(&scope)
        .bind(target_i64)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let count: i64 = row
            .try_get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        if count <= 0 {
            Ok(FenceAcknowledgement::Acknowledged)
        } else {
            Ok(FenceAcknowledgement::Pending {
                unacked_replicas: usize::try_from(count).unwrap_or(1),
            })
        }
    }
}

// ============================================================================
// SqliteCompat implementation (in-process SQLite parity for Postgres contract)
// ============================================================================

/// In-process `sqlx::SqlitePool` implementation mirroring `PostgresAdmissionLedgerStore`
/// for deterministic dual-backend parity testing without an external PostgreSQL daemon.
#[derive(Clone)]
pub struct SqliteCompatAdmissionLedgerStore {
    pool: SqlitePool,
}

impl SqliteCompatAdmissionLedgerStore {
    pub async fn in_memory() -> Result<Self, AdmissionError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(|err| AdmissionError::Backend(format!("sqlite connect failed: {err}")))?;
        for stmt in SQLITE_SCHEMA {
            sqlx::query(stmt)
                .execute(&pool)
                .await
                .map_err(|err| AdmissionError::Backend(format!("sqlite DDL failed: {err}")))?;
        }
        Ok(Self { pool })
    }

    async fn sqlite_now_ms(tx: &mut Transaction<'_, Sqlite>) -> Result<i64, AdmissionError> {
        let row = sqlx::query("SELECT CAST(unixepoch('subsec') * 1000 AS INTEGER)")
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        row.try_get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))
    }

    async fn sqlite_load_or_init_fence(
        tx: &mut Transaction<'_, Sqlite>,
        tenant: &TenantId,
        kind: &FenceKind,
        now_ms: i64,
    ) -> Result<FenceSnapshot, AdmissionError> {
        let (kind_tag, scope) = fence_kind_and_scope(kind);
        sqlx::query(
            "INSERT OR IGNORE INTO admission_fences (tenant_id, fence_kind, fence_scope, generation, state, updated_at_ms) VALUES (?, ?, ?, 1, 'open', ?)",
        )
        .bind(tenant.as_str())
        .bind(kind_tag)
        .bind(&scope)
        .bind(now_ms)
        .execute(&mut **tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        let row = sqlx::query(
            "SELECT generation, state FROM admission_fences WHERE tenant_id = ? AND fence_kind = ? AND fence_scope = ?",
        )
        .bind(tenant.as_str())
        .bind(kind_tag)
        .bind(&scope)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let gen_i64: i64 = row
            .try_get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let state_str: String = row
            .try_get(1)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let gen_u64 = u64::try_from(gen_i64)
            .map_err(|_| AdmissionError::Backend("negative fence generation".to_owned()))?;
        Ok(FenceSnapshot {
            generation: FenceGeneration::new(gen_u64),
            state: parse_fence_state(&state_str)?,
        })
    }

    async fn sqlite_load_all_fences(
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<Vec<(FenceKey, FenceGeneration)>, AdmissionError> {
        let rows = sqlx::query(
            "SELECT tenant_id, fence_kind, fence_scope, generation FROM admission_fences",
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let tenant_str: String = row
                .try_get(0)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            let kind_str: String = row
                .try_get(1)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            let scope_str: String = row
                .try_get(2)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            let gen_i64: i64 = row
                .try_get(3)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            let tenant_id =
                TenantId::parse(&tenant_str).map_err(|e| AdmissionError::Backend(e.to_string()))?;
            let kind = parse_fence_kind_and_scope(&kind_str, &scope_str)?;
            let gen_u64 = u64::try_from(gen_i64)
                .map_err(|_| AdmissionError::Backend("negative generation".to_owned()))?;
            out.push((FenceKey { tenant_id, kind }, FenceGeneration::new(gen_u64)));
        }
        Ok(out)
    }
}

#[async_trait]
impl AdmissionLedgerStore for SqliteCompatAdmissionLedgerStore {
    async fn snapshot_fences(
        &self,
        tenant: &TenantId,
        credential: &CredentialKey,
        room_or_space: &RoomScopeKey,
    ) -> Result<ThreeFenceSnapshot, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::sqlite_now_ms(&mut tx).await?;
        let cred = Self::sqlite_load_or_init_fence(
            &mut tx,
            tenant,
            &FenceKind::Credential(credential.clone()),
            now_ms,
        )
        .await?;
        let room = Self::sqlite_load_or_init_fence(
            &mut tx,
            tenant,
            &FenceKind::RoomOrSpace(room_or_space.clone()),
            now_ms,
        )
        .await?;
        let placement =
            Self::sqlite_load_or_init_fence(&mut tx, tenant, &FenceKind::Placement, now_ms).await?;
        tx.commit()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(ThreeFenceSnapshot {
            credential: cred,
            room_or_space: room,
            placement,
        })
    }

    async fn close_and_advance_fence(
        &self,
        key: &FenceKey,
    ) -> Result<FenceGeneration, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::sqlite_now_ms(&mut tx).await?;
        let current =
            Self::sqlite_load_or_init_fence(&mut tx, &key.tenant_id, &key.kind, now_ms).await?;
        let next = close_and_advance(to_kernel_fence(current))
            .map_err(|err| AdmissionError::Backend(format!("fence overflow: {err}")))?;
        let next_i64 = i64::try_from(next.generation)
            .map_err(|_| AdmissionError::Backend("generation exceeds i64".to_owned()))?;
        let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
        sqlx::query(
            "UPDATE admission_fences SET generation = ?, state = 'closed', updated_at_ms = ? WHERE tenant_id = ? AND fence_kind = ? AND fence_scope = ?",
        )
        .bind(next_i64)
        .bind(now_ms)
        .bind(key.tenant_id.as_str())
        .bind(kind_tag)
        .bind(&scope)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(FenceGeneration::new(next.generation))
    }

    async fn reopen_fence(
        &self,
        key: &FenceKey,
        expected_generation: FenceGeneration,
    ) -> Result<(), AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::sqlite_now_ms(&mut tx).await?;
        let current =
            Self::sqlite_load_or_init_fence(&mut tx, &key.tenant_id, &key.kind, now_ms).await?;
        let _ =
            reopen_fence(to_kernel_fence(current), expected_generation.get()).map_err(|err| {
                match err {
                    FenceGuardError::GenerationMismatch | FenceGuardError::AlreadyOpen => {
                        AdmissionError::FenceStaleOrClosed(key.kind.clone())
                    }
                    FenceGuardError::GenerationOverflow => AdmissionError::Backend(err.to_string()),
                }
            })?;
        let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
        sqlx::query(
            "UPDATE admission_fences SET state = 'open', updated_at_ms = ? WHERE tenant_id = ? AND fence_kind = ? AND fence_scope = ?",
        )
        .bind(now_ms)
        .bind(key.tenant_id.as_str())
        .bind(kind_tag)
        .bind(&scope)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn admit_attempt(
        &self,
        req: &AdmitAttemptRequest,
    ) -> Result<AdmitReceipt, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::sqlite_now_ms(&mut tx).await?;
        if now_ms >= req.expires_at_ms {
            return Err(AdmissionError::InvocationExpired);
        }
        if req.holder.spiffe_id.as_str().is_empty() {
            return Err(AdmissionError::HolderUnauthorized);
        }

        let cred_kind = FenceKind::Credential(req.credential.clone());
        let room_kind = FenceKind::RoomOrSpace(req.room_or_space.clone());
        let cred_cur =
            Self::sqlite_load_or_init_fence(&mut tx, &req.tenant_id, &cred_kind, now_ms).await?;
        let room_cur =
            Self::sqlite_load_or_init_fence(&mut tx, &req.tenant_id, &room_kind, now_ms).await?;
        let place_cur =
            Self::sqlite_load_or_init_fence(&mut tx, &req.tenant_id, &FenceKind::Placement, now_ms)
                .await?;

        let check_input = AdmissionCheckInput {
            snapshot: KernelThreeFenceSnapshot {
                cred_gen: req.expected_fences.credential.generation.get(),
                room_gen: req.expected_fences.room_or_space.generation.get(),
                placement_gen: req.expected_fences.placement.generation.get(),
            },
            current: KernelThreeFenceState {
                cred: to_kernel_fence(cred_cur),
                room: to_kernel_fence(room_cur),
                placement: to_kernel_fence(place_cur),
            },
            expected_holder_id: 1,
            actual_holder_id: 1,
            now_ms,
            lease_expiry_ms: req.expires_at_ms,
            perm_allowed: true,
        };
        if !evaluate_admission(&check_input) {
            if cred_cur.state != FenceState::Open
                || cred_cur.generation != req.expected_fences.credential.generation
            {
                return Err(AdmissionError::FenceStaleOrClosed(cred_kind));
            }
            if room_cur.state != FenceState::Open
                || room_cur.generation != req.expected_fences.room_or_space.generation
            {
                return Err(AdmissionError::FenceStaleOrClosed(room_kind));
            }
            return Err(AdmissionError::FenceStaleOrClosed(FenceKind::Placement));
        }

        let attempt_id_str = req.attempt_id.as_uuid().to_string();
        let run_id_str = req.run_id.as_uuid().to_string();
        let step_i64 = i64::from(req.step_id.get());

        if let Some(row) = sqlx::query("SELECT status FROM admission_attempts WHERE attempt_id = ?")
            .bind(&attempt_id_str)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?
        {
            let status_s: String = row
                .try_get(0)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            if status_s == AttemptStatus::Quarantined.as_str() {
                return Err(AdmissionError::Quarantined);
            }
            return Err(AdmissionError::DuplicateAttempt);
        }

        if let Some(row) = sqlx::query(
            "SELECT status FROM admission_attempts WHERE tenant_id = ? AND nonce_bytes = ?",
        )
        .bind(req.tenant_id.as_str())
        .bind(req.invocation_nonce.as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?
        {
            let status_s: String = row
                .try_get(0)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?;
            if status_s == AttemptStatus::Quarantined.as_str() {
                return Err(AdmissionError::Quarantined);
            }
            return Err(AdmissionError::NonceReplayed);
        }

        if sqlx::query("SELECT 1 FROM admission_attempts WHERE run_id = ? AND step_id = ?")
            .bind(&run_id_str)
            .bind(step_i64)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?
            .is_some()
        {
            return Err(AdmissionError::DuplicateAttempt);
        }

        sqlx::query(
            "INSERT OR IGNORE INTO admission_runs (run_id, tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used, created_at_ms) VALUES (?, ?, ?, '0', '0', ?, 0, ?, '0', ?, '0', ?)",
        )
        .bind(&run_id_str)
        .bind(req.tenant_id.as_str())
        .bind(encode_u64(req.budget.max_spend_microcents))
        .bind(i64::from(req.budget.max_steps))
        .bind(encode_u64(req.budget.max_input_tokens))
        .bind(encode_u64(req.budget.max_output_tokens))
        .bind(now_ms)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        let run_row = sqlx::query(
            "SELECT tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used FROM admission_runs WHERE run_id = ?",
        )
        .bind(&run_id_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let row_tenant_id: String = run_row
            .try_get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        if row_tenant_id != req.tenant_id.as_str() {
            return Err(AdmissionError::HolderUnauthorized);
        }
        let cap_s: String = run_row
            .try_get(1)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let spent_s: String = run_row
            .try_get(2)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let res_s: String = run_row
            .try_get(3)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_steps: i64 = run_row
            .try_get(4)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let steps_used: i64 = run_row
            .try_get(5)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        if steps_used >= max_steps {
            return Err(AdmissionError::BudgetExceeded);
        }
        let mut budget = BudgetState {
            signed_cap: decode_u64(&cap_s)?,
            spent: decode_u64(&spent_s)?,
            reserved: decode_u64(&res_s)?,
        };
        budget
            .reserve(req.reserved_spend_microcents)
            .map_err(map_budget_err)?;
        let next_steps = steps_used
            .checked_add(1)
            .ok_or(AdmissionError::BudgetExceeded)?;

        sqlx::query(
            "UPDATE admission_runs SET reserved_microcents = ?, steps_used = ? WHERE run_id = ?",
        )
        .bind(encode_u64(budget.reserved))
        .bind(next_steps)
        .bind(&run_id_str)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        let (_, cred_scope_str) = fence_kind_and_scope(&cred_kind);
        sqlx::query(
            "INSERT INTO admission_attempts (attempt_id, run_id, step_id, tenant_id, credential_scope, room_scope, holder_spiffe_id, holder_incarnation_pubkey, nonce_bytes, status, reserved_spend_microcents, actual_spend_microcents, input_tokens, output_tokens, admitted_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'admitted', ?, '0', '0', '0', ?)",
        )
        .bind(&attempt_id_str)
        .bind(&run_id_str)
        .bind(step_i64)
        .bind(req.tenant_id.as_str())
        .bind(&cred_scope_str)
        .bind(req.room_or_space.scope_id.as_str())
        .bind(req.holder.spiffe_id.as_str())
        .bind(req.holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(req.invocation_nonce.as_slice())
        .bind(encode_u64(req.reserved_spend_microcents))
        .bind(now_ms)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(AdmitReceipt {
            attempt_id: req.attempt_id,
            run_id: req.run_id,
            step_id: req.step_id,
            admitted_at_ms: now_ms,
            reserved_spend_microcents: req.reserved_spend_microcents,
        })
    }

    async fn reconcile_attempt(
        &self,
        attempt_id: AttemptId,
        outcome: AttemptOutcome,
    ) -> Result<(), AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let attempt_id_str = attempt_id.as_uuid().to_string();
        let row = sqlx::query(
            "SELECT run_id, status, reserved_spend_microcents FROM admission_attempts WHERE attempt_id = ?",
        )
        .bind(&attempt_id_str)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?
        .ok_or(AdmissionError::AttemptNotFound)?;
        let run_id_str: String = row
            .try_get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let status_s: String = row
            .try_get(1)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let reserved_s: String = row
            .try_get(2)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let reserved_spend = decode_u64(&reserved_s)?;

        let current_status = AttemptStatus::parse(&status_s)?;
        if current_status == AttemptStatus::Quarantined {
            return Err(AdmissionError::Quarantined);
        }
        if current_status != AttemptStatus::Admitted {
            return Err(AdmissionError::Backend(format!(
                "attempt already reconciled in status {}",
                current_status.as_str()
            )));
        }

        let run_row = sqlx::query(
            "SELECT max_spend_microcents, spent_microcents, reserved_microcents, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used FROM admission_runs WHERE run_id = ?",
        )
        .bind(&run_id_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let cap_s: String = run_row
            .try_get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let spent_s: String = run_row
            .try_get(1)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let res_s: String = run_row
            .try_get(2)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_in_s: String = run_row
            .try_get(3)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let in_s: String = run_row
            .try_get(4)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_out_s: String = run_row
            .try_get(5)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let out_s: String = run_row
            .try_get(6)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        let mut budget = BudgetState {
            signed_cap: decode_u64(&cap_s)?,
            spent: decode_u64(&spent_s)?,
            reserved: decode_u64(&res_s)?,
        };
        let max_in = decode_u64(&max_in_s)?;
        let in_used = decode_u64(&in_s)?;
        let max_out = decode_u64(&max_out_s)?;
        let out_used = decode_u64(&out_s)?;

        let (new_status, actual_spend, new_in, new_out, attempt_in, attempt_out) = match outcome {
            AttemptOutcome::Completed {
                input_tokens,
                output_tokens,
                actual_spend_microcents,
            } => {
                let next_in = in_used
                    .checked_add(input_tokens)
                    .ok_or(AdmissionError::BudgetExceeded)?;
                let next_out = out_used
                    .checked_add(output_tokens)
                    .ok_or(AdmissionError::BudgetExceeded)?;
                if next_in > max_in || next_out > max_out {
                    return Err(AdmissionError::BudgetExceeded);
                }
                budget
                    .commit(reserved_spend, actual_spend_microcents)
                    .map_err(map_budget_err)?;
                (
                    AttemptStatus::Completed,
                    actual_spend_microcents,
                    next_in,
                    next_out,
                    input_tokens,
                    output_tokens,
                )
            }
            AttemptOutcome::FailedBeforeSend => {
                budget.release(reserved_spend).map_err(map_budget_err)?;
                (AttemptStatus::FailedBeforeSend, 0, in_used, out_used, 0, 0)
            }
            AttemptOutcome::Canceled {
                actual_spend_microcents,
            } => {
                budget
                    .commit(reserved_spend, actual_spend_microcents)
                    .map_err(map_budget_err)?;
                (
                    AttemptStatus::Canceled,
                    actual_spend_microcents,
                    in_used,
                    out_used,
                    0,
                    0,
                )
            }
            AttemptOutcome::UnknownAfterSend => {
                budget
                    .quarantine_conservative(reserved_spend)
                    .map_err(map_budget_err)?;
                (
                    AttemptStatus::Quarantined,
                    reserved_spend,
                    in_used,
                    out_used,
                    0,
                    0,
                )
            }
        };

        sqlx::query(
            "UPDATE admission_runs SET spent_microcents = ?, reserved_microcents = ?, input_tokens_used = ?, output_tokens_used = ? WHERE run_id = ?",
        )
        .bind(encode_u64(budget.spent))
        .bind(encode_u64(budget.reserved))
        .bind(encode_u64(new_in))
        .bind(encode_u64(new_out))
        .bind(&run_id_str)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        sqlx::query(
            "UPDATE admission_attempts SET status = ?, actual_spend_microcents = ?, input_tokens = ?, output_tokens = ? WHERE attempt_id = ?",
        )
        .bind(new_status.as_str())
        .bind(encode_u64(actual_spend))
        .bind(encode_u64(attempt_in))
        .bind(encode_u64(attempt_out))
        .bind(&attempt_id_str)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl ReplicaFenceStore for SqliteCompatAdmissionLedgerStore {
    async fn register_replica(
        &self,
        replica_id: Uuid,
        ttl_ms: i64,
    ) -> Result<Vec<(FenceKey, FenceGeneration)>, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::sqlite_now_ms(&mut tx).await?;
        let expires_at = now_ms.saturating_add(ttl_ms);
        sqlx::query(
            "INSERT INTO admission_fence_replicas (replica_id, registered_at_ms, lease_expires_at_ms) VALUES (?, ?, ?) ON CONFLICT(replica_id) DO UPDATE SET lease_expires_at_ms = excluded.lease_expires_at_ms",
        )
        .bind(replica_id.to_string())
        .bind(now_ms)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let fences = Self::sqlite_load_all_fences(&mut tx).await?;
        tx.commit()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(fences)
    }

    async fn renew_replica(
        &self,
        replica_id: Uuid,
        ttl_ms: i64,
    ) -> Result<Option<Vec<(FenceKey, FenceGeneration)>>, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::sqlite_now_ms(&mut tx).await?;
        let expires_at = now_ms.saturating_add(ttl_ms);
        let res = sqlx::query(
            "UPDATE admission_fence_replicas SET lease_expires_at_ms = ? WHERE replica_id = ?",
        )
        .bind(expires_at)
        .bind(replica_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        if res.rows_affected() == 0 {
            return Ok(None);
        }
        let fences = Self::sqlite_load_all_fences(&mut tx).await?;
        tx.commit()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(Some(fences))
    }

    async fn ack_replica_fences(
        &self,
        replica_id: Uuid,
        acks: &[(FenceKey, FenceGeneration)],
    ) -> Result<(), AdmissionError> {
        if acks.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::sqlite_now_ms(&mut tx).await?;
        let id_str = replica_id.to_string();
        for (key, gen) in acks {
            let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
            let gen_i64 = i64::try_from(gen.get())
                .map_err(|_| AdmissionError::Backend("generation exceeds i64".to_owned()))?;
            sqlx::query(
                "INSERT INTO admission_fence_acks (replica_id, tenant_id, fence_kind, fence_scope, generation, acknowledged_at_ms) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(replica_id, tenant_id, fence_kind, fence_scope) DO UPDATE SET generation = MAX(admission_fence_acks.generation, excluded.generation), acknowledged_at_ms = excluded.acknowledged_at_ms",
            )
            .bind(&id_str)
            .bind(key.tenant_id.as_str())
            .bind(kind_tag)
            .bind(&scope)
            .bind(gen_i64)
            .bind(now_ms)
            .execute(&mut *tx)
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn release_replica(&self, replica_id: Uuid) -> Result<(), AdmissionError> {
        let id_str = replica_id.to_string();
        sqlx::query("DELETE FROM admission_fence_acks WHERE replica_id = ?")
            .bind(&id_str)
            .execute(&self.pool)
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        sqlx::query("DELETE FROM admission_fence_replicas WHERE replica_id = ?")
            .bind(&id_str)
            .execute(&self.pool)
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn check_revocation_ack(
        &self,
        key: &FenceKey,
        target_gen: FenceGeneration,
        expiry_grace_ms: i64,
    ) -> Result<FenceAcknowledgement, AdmissionError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let now_ms = Self::sqlite_now_ms(&mut tx).await?;
        let cutoff = now_ms.saturating_sub(expiry_grace_ms);
        let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
        let target_i64 = i64::try_from(target_gen.get())
            .map_err(|_| AdmissionError::Backend("target_gen exceeds i64".to_owned()))?;
        let row = sqlx::query(
            "SELECT COUNT(*) FROM admission_fence_replicas r WHERE r.lease_expires_at_ms > ? AND NOT EXISTS (SELECT 1 FROM admission_fence_acks a WHERE a.replica_id = r.replica_id AND a.tenant_id = ? AND a.fence_kind = ? AND a.fence_scope = ? AND a.generation >= ?)",
        )
        .bind(cutoff)
        .bind(key.tenant_id.as_str())
        .bind(kind_tag)
        .bind(&scope)
        .bind(target_i64)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let count: i64 = row
            .try_get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        if count <= 0 {
            Ok(FenceAcknowledgement::Acknowledged)
        } else {
            Ok(FenceAcknowledgement::Pending {
                unacked_replicas: usize::try_from(count).unwrap_or(1),
            })
        }
    }
}

#[async_trait]
impl AdmissionQueryStore for SqliteCompatAdmissionLedgerStore {
    async fn get_attempt(
        &self,
        attempt_id: AttemptId,
    ) -> Result<Option<AttemptRecord>, AdmissionError> {
        let row = sqlx::query(
            "SELECT attempt_id, run_id, step_id, tenant_id, status, reserved_spend_microcents, actual_spend_microcents, input_tokens, output_tokens, admitted_at_ms FROM admission_attempts WHERE attempt_id = ?",
        )
        .bind(attempt_id.as_uuid().to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        row.map(decode_sqlite_attempt).transpose()
    }

    async fn get_attempt_by_nonce(
        &self,
        tenant_id: &TenantId,
        nonce: &[u8; 32],
    ) -> Result<Option<AttemptRecord>, AdmissionError> {
        let row = sqlx::query(
            "SELECT attempt_id, run_id, step_id, tenant_id, status, reserved_spend_microcents, actual_spend_microcents, input_tokens, output_tokens, admitted_at_ms FROM admission_attempts WHERE tenant_id = ? AND nonce_bytes = ?",
        )
        .bind(tenant_id.as_str())
        .bind(nonce.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        row.map(decode_sqlite_attempt).transpose()
    }

    async fn get_run_budget(
        &self,
        run_id: RunId,
    ) -> Result<Option<RunBudgetSnapshot>, AdmissionError> {
        let Some(row) = sqlx::query(
            "SELECT run_id, tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used FROM admission_runs WHERE run_id = ?",
        )
        .bind(run_id.as_uuid().to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AdmissionError::Backend(e.to_string()))?
        else {
            return Ok(None);
        };
        let run_s: String = row
            .try_get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let tenant_s: String = row
            .try_get(1)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let cap_s: String = row
            .try_get(2)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let spent_s: String = row
            .try_get(3)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let res_s: String = row
            .try_get(4)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_steps: i64 = row
            .try_get(5)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let steps_used: i64 = row
            .try_get(6)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_in: String = row
            .try_get(7)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let in_used: String = row
            .try_get(8)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_out: String = row
            .try_get(9)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let out_used: String = row
            .try_get(10)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        Ok(Some(RunBudgetSnapshot {
            run_id: RunId::from_uuid(
                Uuid::parse_str(&run_s).map_err(|e| AdmissionError::Backend(e.to_string()))?,
            ),
            tenant_id: TenantId::parse(&tenant_s)
                .map_err(|e| AdmissionError::Backend(e.to_string()))?,
            signed_cap_microcents: decode_u64(&cap_s)?,
            spent_microcents: decode_u64(&spent_s)?,
            reserved_microcents: decode_u64(&res_s)?,
            max_steps: u32::try_from(max_steps).unwrap_or(0),
            steps_used: u32::try_from(steps_used).unwrap_or(0),
            max_input_tokens: decode_u64(&max_in)?,
            input_tokens_used: decode_u64(&in_used)?,
            max_output_tokens: decode_u64(&max_out)?,
            output_tokens_used: decode_u64(&out_used)?,
        }))
    }
}

fn decode_sqlite_attempt(row: sqlx::sqlite::SqliteRow) -> Result<AttemptRecord, AdmissionError> {
    let attempt_s: String = row
        .try_get(0)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let run_s: String = row
        .try_get(1)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let step_i64: i64 = row
        .try_get(2)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let tenant_s: String = row
        .try_get(3)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let status_s: String = row
        .try_get(4)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let reserved_s: String = row
        .try_get(5)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let actual_s: String = row
        .try_get(6)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let in_s: String = row
        .try_get(7)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let out_s: String = row
        .try_get(8)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let admitted_at_ms: i64 = row
        .try_get(9)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    Ok(AttemptRecord {
        attempt_id: AttemptId::from_uuid(
            Uuid::parse_str(&attempt_s).map_err(|e| AdmissionError::Backend(e.to_string()))?,
        ),
        run_id: RunId::from_uuid(
            Uuid::parse_str(&run_s).map_err(|e| AdmissionError::Backend(e.to_string()))?,
        ),
        step_id: StepId::new(u32::try_from(step_i64).unwrap_or(0)),
        tenant_id: TenantId::parse(&tenant_s)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?,
        status: AttemptStatus::parse(&status_s)?,
        reserved_spend_microcents: decode_u64(&reserved_s)?,
        actual_spend_microcents: decode_u64(&actual_s)?,
        input_tokens: decode_u64(&in_s)?,
        output_tokens: decode_u64(&out_s)?,
        admitted_at_ms,
    })
}
