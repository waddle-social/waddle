//! PostgreSQL (`PgPool` + `SELECT ... FOR UPDATE`) and in-process `SqliteCompat`
//! (`SqlitePool`) implementations of the 3-fence `AdmissionLedgerStore`,
//! `ReplicaFenceStore`, and `AdmissionQueryStore`.

use std::borrow::Cow;

use async_trait::async_trait;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{
    ColumnIndex, Database, Decode, Encode, Executor, IntoArguments, PgPool, Pool, Postgres, Row,
    Sqlite, Transaction, Type,
};
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

fn build_admission_schema(int_type: &str, blob_type: &str) -> Vec<String> {
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS admission_fences (
                tenant_id TEXT NOT NULL,
                fence_kind TEXT NOT NULL CHECK (fence_kind IN ('credential', 'room_or_space', 'placement')),
                fence_scope TEXT NOT NULL,
                generation {int_type} NOT NULL CHECK (generation >= 1),
                state TEXT NOT NULL CHECK (state IN ('open', 'closed')),
                updated_at_ms {int_type} NOT NULL,
                PRIMARY KEY (tenant_id, fence_kind, fence_scope)
            )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS admission_runs (
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
                created_at_ms {int_type} NOT NULL
            )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS admission_attempts (
                attempt_id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                step_id INTEGER NOT NULL,
                tenant_id TEXT NOT NULL,
                credential_scope TEXT NOT NULL,
                room_scope TEXT NOT NULL,
                holder_spiffe_id TEXT NOT NULL,
                holder_incarnation_pubkey {blob_type} NOT NULL,
                nonce_bytes {blob_type} NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('admitted', 'completed', 'failed_before_send', 'canceled', 'quarantined')),
                reserved_spend_microcents TEXT NOT NULL,
                actual_spend_microcents TEXT NOT NULL,
                input_tokens TEXT NOT NULL,
                output_tokens TEXT NOT NULL,
                admitted_at_ms {int_type} NOT NULL,
                UNIQUE (run_id, step_id),
                UNIQUE (tenant_id, nonce_bytes)
            )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS admission_fence_replicas (
                replica_id TEXT PRIMARY KEY,
                registered_at_ms {int_type} NOT NULL,
                lease_expires_at_ms {int_type} NOT NULL
            )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS admission_fence_acks (
                replica_id TEXT NOT NULL,
                tenant_id TEXT NOT NULL,
                fence_kind TEXT NOT NULL,
                fence_scope TEXT NOT NULL,
                generation {int_type} NOT NULL,
                acknowledged_at_ms {int_type} NOT NULL,
                PRIMARY KEY (replica_id, tenant_id, fence_kind, fence_scope)
            )"
        ),
    ]
}

const SQL_SELECT_FENCE_LOCK: &str = "SELECT generation, state FROM admission_fences WHERE tenant_id = $1 AND fence_kind = $2 AND fence_scope = $3 FOR UPDATE";
const SQL_SELECT_ALL_FENCES: &str =
    "SELECT tenant_id, fence_kind, fence_scope, generation FROM admission_fences";
const SQL_UPDATE_FENCE_CLOSED: &str = "UPDATE admission_fences SET generation = $1, state = 'closed', updated_at_ms = $2 WHERE tenant_id = $3 AND fence_kind = $4 AND fence_scope = $5";
const SQL_UPDATE_FENCE_OPEN: &str = "UPDATE admission_fences SET state = 'open', updated_at_ms = $1 WHERE tenant_id = $2 AND fence_kind = $3 AND fence_scope = $4";
const SQL_SELECT_ATTEMPT_STATUS_BY_ID: &str =
    "SELECT status FROM admission_attempts WHERE attempt_id = $1";
const SQL_SELECT_ATTEMPT_STATUS_BY_NONCE: &str =
    "SELECT status FROM admission_attempts WHERE tenant_id = $1 AND nonce_bytes = $2";
const SQL_SELECT_ATTEMPT_BY_RUN_STEP: &str =
    "SELECT 1 FROM admission_attempts WHERE run_id = $1 AND step_id = $2";
const SQL_SELECT_RUN_FOR_ADMIT: &str = "SELECT tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used FROM admission_runs WHERE run_id = $1 FOR UPDATE";
const SQL_UPDATE_RUN_RESERVE: &str =
    "UPDATE admission_runs SET reserved_microcents = $1, steps_used = $2 WHERE run_id = $3";
const SQL_INSERT_ATTEMPT: &str = "INSERT INTO admission_attempts (attempt_id, run_id, step_id, tenant_id, credential_scope, room_scope, holder_spiffe_id, holder_incarnation_pubkey, nonce_bytes, status, reserved_spend_microcents, actual_spend_microcents, input_tokens, output_tokens, admitted_at_ms) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'admitted', $10, '0', '0', '0', $11)";
const SQL_SELECT_ATTEMPT_FOR_RECONCILE: &str = "SELECT run_id, status, reserved_spend_microcents FROM admission_attempts WHERE attempt_id = $1 FOR UPDATE";
const SQL_SELECT_RUN_FOR_RECONCILE: &str = "SELECT max_spend_microcents, spent_microcents, reserved_microcents, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used FROM admission_runs WHERE run_id = $1 FOR UPDATE";
const SQL_UPDATE_RUN_RECONCILE: &str = "UPDATE admission_runs SET spent_microcents = $1, reserved_microcents = $2, input_tokens_used = $3, output_tokens_used = $4 WHERE run_id = $5";
const SQL_UPDATE_ATTEMPT_RECONCILE: &str = "UPDATE admission_attempts SET status = $1, actual_spend_microcents = $2, input_tokens = $3, output_tokens = $4 WHERE attempt_id = $5";
const SQL_UPDATE_REPLICA_LEASE: &str =
    "UPDATE admission_fence_replicas SET lease_expires_at_ms = $1 WHERE replica_id = $2";
const SQL_DELETE_REPLICA_ACKS: &str = "DELETE FROM admission_fence_acks WHERE replica_id = $1";
const SQL_DELETE_REPLICA: &str = "DELETE FROM admission_fence_replicas WHERE replica_id = $1";
const SQL_COUNT_UNACKED_REPLICAS: &str = "SELECT COUNT(*) FROM admission_fence_replicas r WHERE r.lease_expires_at_ms > $1 AND NOT EXISTS (SELECT 1 FROM admission_fence_acks a WHERE a.replica_id = r.replica_id AND a.tenant_id = $2 AND a.fence_kind = $3 AND a.fence_scope = $4 AND a.generation >= $5)";
const SQL_SELECT_ATTEMPT_BY_ID: &str = "SELECT attempt_id, run_id, step_id, tenant_id, status, reserved_spend_microcents, actual_spend_microcents, input_tokens, output_tokens, admitted_at_ms FROM admission_attempts WHERE attempt_id = $1";
const SQL_SELECT_ATTEMPT_BY_NONCE: &str = "SELECT attempt_id, run_id, step_id, tenant_id, status, reserved_spend_microcents, actual_spend_microcents, input_tokens, output_tokens, admitted_at_ms FROM admission_attempts WHERE tenant_id = $1 AND nonce_bytes = $2";
const SQL_SELECT_RUN_BUDGET: &str = "SELECT run_id, tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used FROM admission_runs WHERE run_id = $1";

fn pg_sql_to_sqlite(sql: &str) -> String {
    let trimmed = sql.strip_suffix(" FOR UPDATE").unwrap_or(sql);
    let mut out = String::with_capacity(trimmed.len());
    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek().is_some_and(char::is_ascii_digit) {
            out.push('?');
            while chars.peek().is_some_and(char::is_ascii_digit) {
                let _ = chars.next();
            }
        } else {
            out.push(ch);
        }
    }
    out
}

trait AdmissionDialect: Database
where
    Self: Sized,
    for<'c> &'c mut Self::Connection: Executor<'c, Database = Self>,
    for<'q> Self::Arguments<'q>: IntoArguments<'q, Self>,
    usize: ColumnIndex<Self::Row>,
    i64: Type<Self> + for<'e> Encode<'e, Self> + for<'r> Decode<'r, Self>,
    String: Type<Self> + for<'e> Encode<'e, Self> + for<'r> Decode<'r, Self>,
    for<'a> &'a str: Type<Self> + Encode<'a, Self>,
    for<'a> &'a [u8]: Type<Self> + Encode<'a, Self>,
{
    type StepInt: Copy
        + Ord
        + Send
        + Sync
        + Type<Self>
        + for<'e> Encode<'e, Self>
        + for<'r> Decode<'r, Self>;

    const NOW_MS_SQL: &'static str;
    const INSERT_FENCE_IGNORE_SQL: &'static str;
    const INSERT_RUN_IGNORE_SQL: &'static str;
    const UPSERT_REPLICA_SQL: &'static str;
    const UPSERT_FENCE_ACK_SQL: &'static str;

    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str>;
    fn step_from_u32(value: u32) -> Result<Self::StepInt, AdmissionError>;
    fn max_steps_from_u32(value: u32) -> Self::StepInt;
    fn step_checked_add_one(value: Self::StepInt) -> Option<Self::StepInt>;
    fn step_to_u32(value: Self::StepInt) -> u32;
    fn rows_affected(result: &Self::QueryResult) -> u64;
}

impl AdmissionDialect for Postgres {
    type StepInt = i32;
    const NOW_MS_SQL: &'static str =
        "SELECT (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT";
    const INSERT_FENCE_IGNORE_SQL: &'static str = "INSERT INTO admission_fences (tenant_id, fence_kind, fence_scope, generation, state, updated_at_ms) VALUES ($1, $2, $3, 1, 'open', $4) ON CONFLICT (tenant_id, fence_kind, fence_scope) DO NOTHING";
    const INSERT_RUN_IGNORE_SQL: &'static str = "INSERT INTO admission_runs (run_id, tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used, created_at_ms) VALUES ($1, $2, $3, '0', '0', $4, 0, $5, '0', $6, '0', $7) ON CONFLICT (run_id) DO NOTHING";
    const UPSERT_REPLICA_SQL: &'static str = "INSERT INTO admission_fence_replicas (replica_id, registered_at_ms, lease_expires_at_ms) VALUES ($1, $2, $3) ON CONFLICT (replica_id) DO UPDATE SET lease_expires_at_ms = EXCLUDED.lease_expires_at_ms";
    const UPSERT_FENCE_ACK_SQL: &'static str = "INSERT INTO admission_fence_acks (replica_id, tenant_id, fence_kind, fence_scope, generation, acknowledged_at_ms) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (replica_id, tenant_id, fence_kind, fence_scope) DO UPDATE SET generation = GREATEST(admission_fence_acks.generation, EXCLUDED.generation), acknowledged_at_ms = EXCLUDED.acknowledged_at_ms";

    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str> {
        Cow::Borrowed(pg_sql)
    }

    fn step_from_u32(value: u32) -> Result<Self::StepInt, AdmissionError> {
        i32::try_from(value).map_err(|_| AdmissionError::BudgetExceeded)
    }

    fn max_steps_from_u32(value: u32) -> Self::StepInt {
        i32::try_from(value).unwrap_or(i32::MAX)
    }

    fn step_checked_add_one(value: Self::StepInt) -> Option<Self::StepInt> {
        value.checked_add(1)
    }

    fn step_to_u32(value: Self::StepInt) -> u32 {
        u32::try_from(value).unwrap_or(0)
    }

    fn rows_affected(result: &Self::QueryResult) -> u64 {
        result.rows_affected()
    }
}

impl AdmissionDialect for Sqlite {
    type StepInt = i64;
    const NOW_MS_SQL: &'static str = "SELECT CAST(unixepoch('subsec') * 1000 AS INTEGER)";
    const INSERT_FENCE_IGNORE_SQL: &'static str = "INSERT OR IGNORE INTO admission_fences (tenant_id, fence_kind, fence_scope, generation, state, updated_at_ms) VALUES (?, ?, ?, 1, 'open', ?)";
    const INSERT_RUN_IGNORE_SQL: &'static str = "INSERT OR IGNORE INTO admission_runs (run_id, tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used, created_at_ms) VALUES (?, ?, ?, '0', '0', ?, 0, ?, '0', ?, '0', ?)";
    const UPSERT_REPLICA_SQL: &'static str = "INSERT INTO admission_fence_replicas (replica_id, registered_at_ms, lease_expires_at_ms) VALUES (?, ?, ?) ON CONFLICT(replica_id) DO UPDATE SET lease_expires_at_ms = excluded.lease_expires_at_ms";
    const UPSERT_FENCE_ACK_SQL: &'static str = "INSERT INTO admission_fence_acks (replica_id, tenant_id, fence_kind, fence_scope, generation, acknowledged_at_ms) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(replica_id, tenant_id, fence_kind, fence_scope) DO UPDATE SET generation = MAX(admission_fence_acks.generation, excluded.generation), acknowledged_at_ms = excluded.acknowledged_at_ms";

    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str> {
        Cow::Owned(pg_sql_to_sqlite(pg_sql))
    }

    fn step_from_u32(value: u32) -> Result<Self::StepInt, AdmissionError> {
        Ok(i64::from(value))
    }

    fn max_steps_from_u32(value: u32) -> Self::StepInt {
        i64::from(value)
    }

    fn step_checked_add_one(value: Self::StepInt) -> Option<Self::StepInt> {
        value.checked_add(1)
    }

    fn step_to_u32(value: Self::StepInt) -> u32 {
        u32::try_from(value).unwrap_or(0)
    }

    fn rows_affected(result: &Self::QueryResult) -> u64 {
        result.rows_affected()
    }
}

fn backend_err(err: impl std::fmt::Display) -> AdmissionError {
    AdmissionError::Backend(err.to_string())
}

fn col<'r, D, T>(row: &'r D::Row, index: usize) -> Result<T, AdmissionError>
where
    D: Database,
    usize: ColumnIndex<D::Row>,
    T: Decode<'r, D> + Type<D>,
{
    row.try_get(index).map_err(backend_err)
}

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

fn validate_admit_fences(
    req: &AdmitAttemptRequest,
    now_ms: i64,
    cred_cur: FenceSnapshot,
    room_cur: FenceSnapshot,
    place_cur: FenceSnapshot,
    cred_kind: &FenceKind,
    room_kind: &FenceKind,
) -> Result<(), AdmissionError> {
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
    if evaluate_admission(&check_input) {
        return Ok(());
    }
    if cred_cur.state != FenceState::Open
        || cred_cur.generation != req.expected_fences.credential.generation
    {
        return Err(AdmissionError::FenceStaleOrClosed(cred_kind.clone()));
    }
    if room_cur.state != FenceState::Open
        || room_cur.generation != req.expected_fences.room_or_space.generation
    {
        return Err(AdmissionError::FenceStaleOrClosed(room_kind.clone()));
    }
    Err(AdmissionError::FenceStaleOrClosed(FenceKind::Placement))
}

struct ReconciledTotals {
    status: AttemptStatus,
    actual_spend: u64,
    new_in_tokens: u64,
    new_out_tokens: u64,
    attempt_in_tokens: u64,
    attempt_out_tokens: u64,
}

fn apply_attempt_outcome(
    outcome: AttemptOutcome,
    reserved_spend: u64,
    budget: &mut BudgetState,
    max_in: u64,
    in_used: u64,
    max_out: u64,
    out_used: u64,
) -> Result<ReconciledTotals, AdmissionError> {
    match outcome {
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
            Ok(ReconciledTotals {
                status: AttemptStatus::Completed,
                actual_spend: actual_spend_microcents,
                new_in_tokens: next_in,
                new_out_tokens: next_out,
                attempt_in_tokens: input_tokens,
                attempt_out_tokens: output_tokens,
            })
        }
        AttemptOutcome::FailedBeforeSend => {
            budget.release(reserved_spend).map_err(map_budget_err)?;
            Ok(ReconciledTotals {
                status: AttemptStatus::FailedBeforeSend,
                actual_spend: 0,
                new_in_tokens: in_used,
                new_out_tokens: out_used,
                attempt_in_tokens: 0,
                attempt_out_tokens: 0,
            })
        }
        AttemptOutcome::Canceled {
            actual_spend_microcents,
        } => {
            budget
                .commit(reserved_spend, actual_spend_microcents)
                .map_err(map_budget_err)?;
            Ok(ReconciledTotals {
                status: AttemptStatus::Canceled,
                actual_spend: actual_spend_microcents,
                new_in_tokens: in_used,
                new_out_tokens: out_used,
                attempt_in_tokens: 0,
                attempt_out_tokens: 0,
            })
        }
        AttemptOutcome::UnknownAfterSend => {
            budget
                .quarantine_conservative(reserved_spend)
                .map_err(map_budget_err)?;
            Ok(ReconciledTotals {
                status: AttemptStatus::Quarantined,
                actual_spend: reserved_spend,
                new_in_tokens: in_used,
                new_out_tokens: out_used,
                attempt_in_tokens: 0,
                attempt_out_tokens: 0,
            })
        }
    }
}

struct SqlAdmissionStore<D: Database> {
    pool: Pool<D>,
}

impl<D: Database> Clone for SqlAdmissionStore<D> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl<D> SqlAdmissionStore<D>
where
    D: AdmissionDialect,
    for<'c> &'c mut D::Connection: Executor<'c, Database = D>,
    for<'q> D::Arguments<'q>: IntoArguments<'q, D>,
    usize: ColumnIndex<D::Row>,
    i64: Type<D> + for<'e> Encode<'e, D> + for<'r> Decode<'r, D>,
    String: Type<D> + for<'e> Encode<'e, D> + for<'r> Decode<'r, D>,
    for<'a> &'a str: Type<D> + Encode<'a, D>,
    for<'a> &'a [u8]: Type<D> + Encode<'a, D>,
{
    fn new(pool: Pool<D>) -> Self {
        Self { pool }
    }

    async fn now_ms(tx: &mut Transaction<'_, D>) -> Result<i64, AdmissionError> {
        let row = sqlx::query::<D>(D::NOW_MS_SQL)
            .fetch_one(&mut **tx)
            .await
            .map_err(backend_err)?;
        col::<D, i64>(&row, 0)
    }

    async fn load_or_init_fence(
        tx: &mut Transaction<'_, D>,
        tenant: &TenantId,
        kind: &FenceKind,
        now_ms: i64,
    ) -> Result<FenceSnapshot, AdmissionError> {
        let (kind_tag, scope) = fence_kind_and_scope(kind);
        sqlx::query::<D>(D::INSERT_FENCE_IGNORE_SQL)
            .bind(tenant.as_str())
            .bind(kind_tag)
            .bind(scope.as_str())
            .bind(now_ms)
            .execute(&mut **tx)
            .await
            .map_err(backend_err)?;

        let select_sql = D::adapt_sql(SQL_SELECT_FENCE_LOCK);
        let row = sqlx::query::<D>(&select_sql)
            .bind(tenant.as_str())
            .bind(kind_tag)
            .bind(scope.as_str())
            .fetch_one(&mut **tx)
            .await
            .map_err(backend_err)?;

        let gen_i64: i64 = col::<D, i64>(&row, 0)?;
        let state_str: String = col::<D, String>(&row, 1)?;
        let gen_u64 = u64::try_from(gen_i64)
            .map_err(|_| AdmissionError::Backend("negative fence generation".to_owned()))?;
        Ok(FenceSnapshot {
            generation: FenceGeneration::new(gen_u64),
            state: parse_fence_state(&state_str)?,
        })
    }

    async fn load_all_fences(
        tx: &mut Transaction<'_, D>,
    ) -> Result<Vec<(FenceKey, FenceGeneration)>, AdmissionError> {
        let rows = sqlx::query::<D>(SQL_SELECT_ALL_FENCES)
            .fetch_all(&mut **tx)
            .await
            .map_err(backend_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let tenant_str: String = col::<D, String>(&row, 0)?;
            let kind_str: String = col::<D, String>(&row, 1)?;
            let scope_str: String = col::<D, String>(&row, 2)?;
            let gen_i64: i64 = col::<D, i64>(&row, 3)?;
            let tenant_id = TenantId::parse(&tenant_str).map_err(backend_err)?;
            let kind = parse_fence_kind_and_scope(&kind_str, &scope_str)?;
            let gen_u64 = u64::try_from(gen_i64)
                .map_err(|_| AdmissionError::Backend("negative generation".to_owned()))?;
            out.push((FenceKey { tenant_id, kind }, FenceGeneration::new(gen_u64)));
        }
        Ok(out)
    }

    async fn ensure_attempt_unique(
        tx: &mut Transaction<'_, D>,
        req: &AdmitAttemptRequest,
        attempt_id_str: &str,
        run_id_str: &str,
        step_val: D::StepInt,
    ) -> Result<(), AdmissionError> {
        let by_id_sql = D::adapt_sql(SQL_SELECT_ATTEMPT_STATUS_BY_ID);
        if let Some(row) = sqlx::query::<D>(&by_id_sql)
            .bind(attempt_id_str)
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend_err)?
        {
            let status_s: String = col::<D, String>(&row, 0)?;
            if status_s == AttemptStatus::Quarantined.as_str() {
                return Err(AdmissionError::Quarantined);
            }
            return Err(AdmissionError::DuplicateAttempt);
        }

        let by_nonce_sql = D::adapt_sql(SQL_SELECT_ATTEMPT_STATUS_BY_NONCE);
        if let Some(row) = sqlx::query::<D>(&by_nonce_sql)
            .bind(req.tenant_id.as_str())
            .bind(req.invocation_nonce.as_slice())
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend_err)?
        {
            let status_s: String = col::<D, String>(&row, 0)?;
            if status_s == AttemptStatus::Quarantined.as_str() {
                return Err(AdmissionError::Quarantined);
            }
            return Err(AdmissionError::NonceReplayed);
        }

        let by_step_sql = D::adapt_sql(SQL_SELECT_ATTEMPT_BY_RUN_STEP);
        if sqlx::query::<D>(&by_step_sql)
            .bind(run_id_str)
            .bind(step_val)
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend_err)?
            .is_some()
        {
            return Err(AdmissionError::DuplicateAttempt);
        }

        Ok(())
    }

    async fn snapshot_fences(
        &self,
        tenant: &TenantId,
        credential: &CredentialKey,
        room_or_space: &RoomScopeKey,
    ) -> Result<ThreeFenceSnapshot, AdmissionError> {
        let mut tx = self.pool.begin().await.map_err(backend_err)?;
        let now_ms = Self::now_ms(&mut tx).await?;
        let credential_snap = Self::load_or_init_fence(
            &mut tx,
            tenant,
            &FenceKind::Credential(credential.clone()),
            now_ms,
        )
        .await?;
        let room_snap = Self::load_or_init_fence(
            &mut tx,
            tenant,
            &FenceKind::RoomOrSpace(room_or_space.clone()),
            now_ms,
        )
        .await?;
        let placement_snap =
            Self::load_or_init_fence(&mut tx, tenant, &FenceKind::Placement, now_ms).await?;
        tx.commit().await.map_err(backend_err)?;
        Ok(ThreeFenceSnapshot {
            credential: credential_snap,
            room_or_space: room_snap,
            placement: placement_snap,
        })
    }

    async fn close_and_advance_fence(
        &self,
        key: &FenceKey,
    ) -> Result<FenceGeneration, AdmissionError> {
        let mut tx = self.pool.begin().await.map_err(backend_err)?;
        let now_ms = Self::now_ms(&mut tx).await?;
        let current = Self::load_or_init_fence(&mut tx, &key.tenant_id, &key.kind, now_ms).await?;
        let next = close_and_advance(to_kernel_fence(current))
            .map_err(|err| AdmissionError::Backend(format!("fence overflow: {err}")))?;
        let next_i64 = i64::try_from(next.generation)
            .map_err(|_| AdmissionError::Backend("generation exceeds i64".to_owned()))?;
        let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
        let update_sql = D::adapt_sql(SQL_UPDATE_FENCE_CLOSED);
        sqlx::query::<D>(&update_sql)
            .bind(next_i64)
            .bind(now_ms)
            .bind(key.tenant_id.as_str())
            .bind(kind_tag)
            .bind(scope.as_str())
            .execute(&mut *tx)
            .await
            .map_err(backend_err)?;
        tx.commit().await.map_err(backend_err)?;
        Ok(FenceGeneration::new(next.generation))
    }

    async fn reopen_fence(
        &self,
        key: &FenceKey,
        expected_generation: FenceGeneration,
    ) -> Result<(), AdmissionError> {
        let mut tx = self.pool.begin().await.map_err(backend_err)?;
        let now_ms = Self::now_ms(&mut tx).await?;
        let current = Self::load_or_init_fence(&mut tx, &key.tenant_id, &key.kind, now_ms).await?;
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
        let update_sql = D::adapt_sql(SQL_UPDATE_FENCE_OPEN);
        sqlx::query::<D>(&update_sql)
            .bind(now_ms)
            .bind(key.tenant_id.as_str())
            .bind(kind_tag)
            .bind(scope.as_str())
            .execute(&mut *tx)
            .await
            .map_err(backend_err)?;
        tx.commit().await.map_err(backend_err)?;
        Ok(())
    }

    async fn admit_attempt(
        &self,
        req: &AdmitAttemptRequest,
    ) -> Result<AdmitReceipt, AdmissionError> {
        let mut tx = self.pool.begin().await.map_err(backend_err)?;
        let now_ms = Self::now_ms(&mut tx).await?;
        if now_ms >= req.expires_at_ms {
            return Err(AdmissionError::InvocationExpired);
        }
        if req.holder.spiffe_id.as_str().is_empty() {
            return Err(AdmissionError::HolderUnauthorized);
        }

        let cred_kind = FenceKind::Credential(req.credential.clone());
        let room_kind = FenceKind::RoomOrSpace(req.room_or_space.clone());
        let cred_cur =
            Self::load_or_init_fence(&mut tx, &req.tenant_id, &cred_kind, now_ms).await?;
        let room_cur =
            Self::load_or_init_fence(&mut tx, &req.tenant_id, &room_kind, now_ms).await?;
        let place_cur =
            Self::load_or_init_fence(&mut tx, &req.tenant_id, &FenceKind::Placement, now_ms)
                .await?;

        validate_admit_fences(
            req, now_ms, cred_cur, room_cur, place_cur, &cred_kind, &room_kind,
        )?;

        let attempt_id_str = req.attempt_id.as_uuid().to_string();
        let run_id_str = req.run_id.as_uuid().to_string();
        let step_val = D::step_from_u32(req.step_id.get())?;

        Self::ensure_attempt_unique(&mut tx, req, &attempt_id_str, &run_id_str, step_val).await?;

        sqlx::query::<D>(D::INSERT_RUN_IGNORE_SQL)
            .bind(run_id_str.as_str())
            .bind(req.tenant_id.as_str())
            .bind(encode_u64(req.budget.max_spend_microcents))
            .bind(D::max_steps_from_u32(req.budget.max_steps))
            .bind(encode_u64(req.budget.max_input_tokens))
            .bind(encode_u64(req.budget.max_output_tokens))
            .bind(now_ms)
            .execute(&mut *tx)
            .await
            .map_err(backend_err)?;

        let select_run_sql = D::adapt_sql(SQL_SELECT_RUN_FOR_ADMIT);
        let run_row = sqlx::query::<D>(&select_run_sql)
            .bind(run_id_str.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(backend_err)?;
        let row_tenant_id: String = col::<D, String>(&run_row, 0)?;
        if row_tenant_id != req.tenant_id.as_str() {
            return Err(AdmissionError::HolderUnauthorized);
        }
        let max_steps: D::StepInt = col::<D, D::StepInt>(&run_row, 4)?;
        let steps_used: D::StepInt = col::<D, D::StepInt>(&run_row, 5)?;
        if steps_used >= max_steps {
            return Err(AdmissionError::BudgetExceeded);
        }

        let mut budget = BudgetState {
            signed_cap: decode_u64(&col::<D, String>(&run_row, 1)?)?,
            spent: decode_u64(&col::<D, String>(&run_row, 2)?)?,
            reserved: decode_u64(&col::<D, String>(&run_row, 3)?)?,
        };
        budget
            .reserve(req.reserved_spend_microcents)
            .map_err(map_budget_err)?;
        let next_steps =
            D::step_checked_add_one(steps_used).ok_or(AdmissionError::BudgetExceeded)?;

        let update_run_sql = D::adapt_sql(SQL_UPDATE_RUN_RESERVE);
        sqlx::query::<D>(&update_run_sql)
            .bind(encode_u64(budget.reserved))
            .bind(next_steps)
            .bind(run_id_str.as_str())
            .execute(&mut *tx)
            .await
            .map_err(backend_err)?;

        let (_, cred_scope_str) = fence_kind_and_scope(&cred_kind);
        let insert_attempt_sql = D::adapt_sql(SQL_INSERT_ATTEMPT);
        sqlx::query::<D>(&insert_attempt_sql)
            .bind(attempt_id_str.as_str())
            .bind(run_id_str.as_str())
            .bind(step_val)
            .bind(req.tenant_id.as_str())
            .bind(cred_scope_str.as_str())
            .bind(req.room_or_space.scope_id.as_str())
            .bind(req.holder.spiffe_id.as_str())
            .bind(req.holder.incarnation_pubkey.as_bytes().as_slice())
            .bind(req.invocation_nonce.as_slice())
            .bind(encode_u64(req.reserved_spend_microcents))
            .bind(now_ms)
            .execute(&mut *tx)
            .await
            .map_err(backend_err)?;

        tx.commit().await.map_err(backend_err)?;
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
        let mut tx = self.pool.begin().await.map_err(backend_err)?;
        let attempt_id_str = attempt_id.as_uuid().to_string();
        let select_attempt_sql = D::adapt_sql(SQL_SELECT_ATTEMPT_FOR_RECONCILE);
        let row = sqlx::query::<D>(&select_attempt_sql)
            .bind(attempt_id_str.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend_err)?
            .ok_or(AdmissionError::AttemptNotFound)?;
        let run_id_str: String = col::<D, String>(&row, 0)?;
        let status_s: String = col::<D, String>(&row, 1)?;
        let reserved_spend = decode_u64(&col::<D, String>(&row, 2)?)?;

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

        let select_run_sql = D::adapt_sql(SQL_SELECT_RUN_FOR_RECONCILE);
        let run_row = sqlx::query::<D>(&select_run_sql)
            .bind(run_id_str.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(backend_err)?;
        let mut budget = BudgetState {
            signed_cap: decode_u64(&col::<D, String>(&run_row, 0)?)?,
            spent: decode_u64(&col::<D, String>(&run_row, 1)?)?,
            reserved: decode_u64(&col::<D, String>(&run_row, 2)?)?,
        };
        let max_in = decode_u64(&col::<D, String>(&run_row, 3)?)?;
        let in_used = decode_u64(&col::<D, String>(&run_row, 4)?)?;
        let max_out = decode_u64(&col::<D, String>(&run_row, 5)?)?;
        let out_used = decode_u64(&col::<D, String>(&run_row, 6)?)?;

        let totals = apply_attempt_outcome(
            outcome,
            reserved_spend,
            &mut budget,
            max_in,
            in_used,
            max_out,
            out_used,
        )?;

        let update_run_sql = D::adapt_sql(SQL_UPDATE_RUN_RECONCILE);
        sqlx::query::<D>(&update_run_sql)
            .bind(encode_u64(budget.spent))
            .bind(encode_u64(budget.reserved))
            .bind(encode_u64(totals.new_in_tokens))
            .bind(encode_u64(totals.new_out_tokens))
            .bind(run_id_str.as_str())
            .execute(&mut *tx)
            .await
            .map_err(backend_err)?;

        let update_attempt_sql = D::adapt_sql(SQL_UPDATE_ATTEMPT_RECONCILE);
        sqlx::query::<D>(&update_attempt_sql)
            .bind(totals.status.as_str())
            .bind(encode_u64(totals.actual_spend))
            .bind(encode_u64(totals.attempt_in_tokens))
            .bind(encode_u64(totals.attempt_out_tokens))
            .bind(attempt_id_str.as_str())
            .execute(&mut *tx)
            .await
            .map_err(backend_err)?;

        tx.commit().await.map_err(backend_err)?;
        Ok(())
    }

    async fn register_replica(
        &self,
        replica_id: Uuid,
        ttl_ms: i64,
    ) -> Result<Vec<(FenceKey, FenceGeneration)>, AdmissionError> {
        let mut tx = self.pool.begin().await.map_err(backend_err)?;
        let now_ms = Self::now_ms(&mut tx).await?;
        let expires_at = now_ms.saturating_add(ttl_ms);
        let id_str = replica_id.to_string();
        sqlx::query::<D>(D::UPSERT_REPLICA_SQL)
            .bind(id_str.as_str())
            .bind(now_ms)
            .bind(expires_at)
            .execute(&mut *tx)
            .await
            .map_err(backend_err)?;
        let fences = Self::load_all_fences(&mut tx).await?;
        tx.commit().await.map_err(backend_err)?;
        Ok(fences)
    }

    async fn renew_replica(
        &self,
        replica_id: Uuid,
        ttl_ms: i64,
    ) -> Result<Option<Vec<(FenceKey, FenceGeneration)>>, AdmissionError> {
        let mut tx = self.pool.begin().await.map_err(backend_err)?;
        let now_ms = Self::now_ms(&mut tx).await?;
        let expires_at = now_ms.saturating_add(ttl_ms);
        let id_str = replica_id.to_string();
        let update_sql = D::adapt_sql(SQL_UPDATE_REPLICA_LEASE);
        let res = sqlx::query::<D>(&update_sql)
            .bind(expires_at)
            .bind(id_str.as_str())
            .execute(&mut *tx)
            .await
            .map_err(backend_err)?;
        if D::rows_affected(&res) == 0 {
            return Ok(None);
        }
        let fences = Self::load_all_fences(&mut tx).await?;
        tx.commit().await.map_err(backend_err)?;
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
        let mut tx = self.pool.begin().await.map_err(backend_err)?;
        let now_ms = Self::now_ms(&mut tx).await?;
        let id_str = replica_id.to_string();
        for (key, gen) in acks {
            let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
            let gen_i64 = i64::try_from(gen.get())
                .map_err(|_| AdmissionError::Backend("generation exceeds i64".to_owned()))?;
            sqlx::query::<D>(D::UPSERT_FENCE_ACK_SQL)
                .bind(id_str.as_str())
                .bind(key.tenant_id.as_str())
                .bind(kind_tag)
                .bind(scope.as_str())
                .bind(gen_i64)
                .bind(now_ms)
                .execute(&mut *tx)
                .await
                .map_err(backend_err)?;
        }
        tx.commit().await.map_err(backend_err)?;
        Ok(())
    }

    async fn release_replica(&self, replica_id: Uuid) -> Result<(), AdmissionError> {
        let id_str = replica_id.to_string();
        let delete_acks_sql = D::adapt_sql(SQL_DELETE_REPLICA_ACKS);
        sqlx::query::<D>(&delete_acks_sql)
            .bind(id_str.as_str())
            .execute(&self.pool)
            .await
            .map_err(backend_err)?;
        let delete_replica_sql = D::adapt_sql(SQL_DELETE_REPLICA);
        sqlx::query::<D>(&delete_replica_sql)
            .bind(id_str.as_str())
            .execute(&self.pool)
            .await
            .map_err(backend_err)?;
        Ok(())
    }

    async fn check_revocation_ack(
        &self,
        key: &FenceKey,
        target_gen: FenceGeneration,
        expiry_grace_ms: i64,
    ) -> Result<FenceAcknowledgement, AdmissionError> {
        let mut tx = self.pool.begin().await.map_err(backend_err)?;
        let now_ms = Self::now_ms(&mut tx).await?;
        let cutoff = now_ms.saturating_sub(expiry_grace_ms);
        let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
        let target_i64 = i64::try_from(target_gen.get())
            .map_err(|_| AdmissionError::Backend("target_gen exceeds i64".to_owned()))?;
        let count_sql = D::adapt_sql(SQL_COUNT_UNACKED_REPLICAS);
        let row = sqlx::query::<D>(&count_sql)
            .bind(cutoff)
            .bind(key.tenant_id.as_str())
            .bind(kind_tag)
            .bind(scope.as_str())
            .bind(target_i64)
            .fetch_one(&mut *tx)
            .await
            .map_err(backend_err)?;
        let count: i64 = col::<D, i64>(&row, 0)?;
        if count <= 0 {
            Ok(FenceAcknowledgement::Acknowledged)
        } else {
            Ok(FenceAcknowledgement::Pending {
                unacked_replicas: usize::try_from(count).unwrap_or(1),
            })
        }
    }

    fn decode_attempt_record(row: &D::Row) -> Result<AttemptRecord, AdmissionError> {
        let attempt_s: String = col::<D, String>(row, 0)?;
        let run_s: String = col::<D, String>(row, 1)?;
        let step_val: D::StepInt = col::<D, D::StepInt>(row, 2)?;
        let tenant_s: String = col::<D, String>(row, 3)?;
        let status_s: String = col::<D, String>(row, 4)?;
        let reserved_s: String = col::<D, String>(row, 5)?;
        let actual_s: String = col::<D, String>(row, 6)?;
        let in_s: String = col::<D, String>(row, 7)?;
        let out_s: String = col::<D, String>(row, 8)?;
        let admitted_at_ms: i64 = col::<D, i64>(row, 9)?;
        Ok(AttemptRecord {
            attempt_id: AttemptId::from_uuid(Uuid::parse_str(&attempt_s).map_err(backend_err)?),
            run_id: RunId::from_uuid(Uuid::parse_str(&run_s).map_err(backend_err)?),
            step_id: StepId::new(D::step_to_u32(step_val)),
            tenant_id: TenantId::parse(&tenant_s).map_err(backend_err)?,
            status: AttemptStatus::parse(&status_s)?,
            reserved_spend_microcents: decode_u64(&reserved_s)?,
            actual_spend_microcents: decode_u64(&actual_s)?,
            input_tokens: decode_u64(&in_s)?,
            output_tokens: decode_u64(&out_s)?,
            admitted_at_ms,
        })
    }

    fn decode_run_budget_snapshot(row: &D::Row) -> Result<RunBudgetSnapshot, AdmissionError> {
        let run_s: String = col::<D, String>(row, 0)?;
        let tenant_s: String = col::<D, String>(row, 1)?;
        let cap_s: String = col::<D, String>(row, 2)?;
        let spent_s: String = col::<D, String>(row, 3)?;
        let res_s: String = col::<D, String>(row, 4)?;
        let max_steps: D::StepInt = col::<D, D::StepInt>(row, 5)?;
        let steps_used: D::StepInt = col::<D, D::StepInt>(row, 6)?;
        let max_in: String = col::<D, String>(row, 7)?;
        let in_used: String = col::<D, String>(row, 8)?;
        let max_out: String = col::<D, String>(row, 9)?;
        let out_used: String = col::<D, String>(row, 10)?;
        Ok(RunBudgetSnapshot {
            run_id: RunId::from_uuid(Uuid::parse_str(&run_s).map_err(backend_err)?),
            tenant_id: TenantId::parse(&tenant_s).map_err(backend_err)?,
            signed_cap_microcents: decode_u64(&cap_s)?,
            spent_microcents: decode_u64(&spent_s)?,
            reserved_microcents: decode_u64(&res_s)?,
            max_steps: D::step_to_u32(max_steps),
            steps_used: D::step_to_u32(steps_used),
            max_input_tokens: decode_u64(&max_in)?,
            input_tokens_used: decode_u64(&in_used)?,
            max_output_tokens: decode_u64(&max_out)?,
            output_tokens_used: decode_u64(&out_used)?,
        })
    }

    async fn get_attempt(
        &self,
        attempt_id: AttemptId,
    ) -> Result<Option<AttemptRecord>, AdmissionError> {
        let id_str = attempt_id.as_uuid().to_string();
        let sql = D::adapt_sql(SQL_SELECT_ATTEMPT_BY_ID);
        let row = sqlx::query::<D>(&sql)
            .bind(id_str.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend_err)?;
        row.as_ref().map(Self::decode_attempt_record).transpose()
    }

    async fn get_attempt_by_nonce(
        &self,
        tenant_id: &TenantId,
        nonce: &[u8; 32],
    ) -> Result<Option<AttemptRecord>, AdmissionError> {
        let sql = D::adapt_sql(SQL_SELECT_ATTEMPT_BY_NONCE);
        let row = sqlx::query::<D>(&sql)
            .bind(tenant_id.as_str())
            .bind(nonce.as_slice())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend_err)?;
        row.as_ref().map(Self::decode_attempt_record).transpose()
    }

    async fn get_run_budget(
        &self,
        run_id: RunId,
    ) -> Result<Option<RunBudgetSnapshot>, AdmissionError> {
        let id_str = run_id.as_uuid().to_string();
        let sql = D::adapt_sql(SQL_SELECT_RUN_BUDGET);
        let row = sqlx::query::<D>(&sql)
            .bind(id_str.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend_err)?;
        row.as_ref()
            .map(Self::decode_run_budget_snapshot)
            .transpose()
    }
}

macro_rules! impl_admission_and_replica_stores {
    ($store:ty) => {
        #[async_trait]
        impl AdmissionLedgerStore for $store {
            async fn snapshot_fences(
                &self,
                tenant: &TenantId,
                credential: &CredentialKey,
                room_or_space: &RoomScopeKey,
            ) -> Result<ThreeFenceSnapshot, AdmissionError> {
                self.inner
                    .snapshot_fences(tenant, credential, room_or_space)
                    .await
            }

            async fn close_and_advance_fence(
                &self,
                key: &FenceKey,
            ) -> Result<FenceGeneration, AdmissionError> {
                self.inner.close_and_advance_fence(key).await
            }

            async fn reopen_fence(
                &self,
                key: &FenceKey,
                expected_generation: FenceGeneration,
            ) -> Result<(), AdmissionError> {
                self.inner.reopen_fence(key, expected_generation).await
            }

            async fn admit_attempt(
                &self,
                req: &AdmitAttemptRequest,
            ) -> Result<AdmitReceipt, AdmissionError> {
                self.inner.admit_attempt(req).await
            }

            async fn reconcile_attempt(
                &self,
                attempt_id: AttemptId,
                outcome: AttemptOutcome,
            ) -> Result<(), AdmissionError> {
                self.inner.reconcile_attempt(attempt_id, outcome).await
            }
        }

        #[async_trait]
        impl ReplicaFenceStore for $store {
            async fn register_replica(
                &self,
                replica_id: Uuid,
                ttl_ms: i64,
            ) -> Result<Vec<(FenceKey, FenceGeneration)>, AdmissionError> {
                self.inner.register_replica(replica_id, ttl_ms).await
            }

            async fn renew_replica(
                &self,
                replica_id: Uuid,
                ttl_ms: i64,
            ) -> Result<Option<Vec<(FenceKey, FenceGeneration)>>, AdmissionError> {
                self.inner.renew_replica(replica_id, ttl_ms).await
            }

            async fn ack_replica_fences(
                &self,
                replica_id: Uuid,
                acks: &[(FenceKey, FenceGeneration)],
            ) -> Result<(), AdmissionError> {
                self.inner.ack_replica_fences(replica_id, acks).await
            }

            async fn release_replica(&self, replica_id: Uuid) -> Result<(), AdmissionError> {
                self.inner.release_replica(replica_id).await
            }

            async fn check_revocation_ack(
                &self,
                key: &FenceKey,
                target_gen: FenceGeneration,
                expiry_grace_ms: i64,
            ) -> Result<FenceAcknowledgement, AdmissionError> {
                self.inner
                    .check_revocation_ack(key, target_gen, expiry_grace_ms)
                    .await
            }
        }
    };
}

// ============================================================================
// PostgreSQL implementation
// ============================================================================

/// Production PostgreSQL 3-fence `AdmissionLedgerStore` using `SELECT ... FOR UPDATE`.
#[derive(Clone)]
pub struct PostgresAdmissionLedgerStore {
    inner: SqlAdmissionStore<Postgres>,
}

impl PostgresAdmissionLedgerStore {
    pub async fn new(pool: PgPool) -> Result<Self, AdmissionError> {
        for stmt in build_admission_schema("BIGINT", "BYTEA") {
            sqlx::query(&stmt).execute(&pool).await.map_err(|err| {
                AdmissionError::Backend(format!("PG admission DDL failed: {err}"))
            })?;
        }
        Ok(Self {
            inner: SqlAdmissionStore::new(pool),
        })
    }
}

impl_admission_and_replica_stores!(PostgresAdmissionLedgerStore);

// ============================================================================
// SqliteCompat implementation (in-process SQLite parity for Postgres contract)
// ============================================================================

/// In-process `sqlx::SqlitePool` implementation mirroring `PostgresAdmissionLedgerStore`
/// for deterministic dual-backend parity testing without an external PostgreSQL daemon.
#[derive(Clone)]
pub struct SqliteCompatAdmissionLedgerStore {
    inner: SqlAdmissionStore<Sqlite>,
}

impl SqliteCompatAdmissionLedgerStore {
    pub async fn in_memory() -> Result<Self, AdmissionError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(|err| AdmissionError::Backend(format!("sqlite connect failed: {err}")))?;
        for stmt in build_admission_schema("INTEGER", "BLOB") {
            sqlx::query(&stmt)
                .execute(&pool)
                .await
                .map_err(|err| AdmissionError::Backend(format!("sqlite DDL failed: {err}")))?;
        }
        Ok(Self {
            inner: SqlAdmissionStore::new(pool),
        })
    }
}

impl_admission_and_replica_stores!(SqliteCompatAdmissionLedgerStore);

#[async_trait]
impl AdmissionQueryStore for SqliteCompatAdmissionLedgerStore {
    async fn get_attempt(
        &self,
        attempt_id: AttemptId,
    ) -> Result<Option<AttemptRecord>, AdmissionError> {
        self.inner.get_attempt(attempt_id).await
    }

    async fn get_attempt_by_nonce(
        &self,
        tenant_id: &TenantId,
        nonce: &[u8; 32],
    ) -> Result<Option<AttemptRecord>, AdmissionError> {
        self.inner.get_attempt_by_nonce(tenant_id, nonce).await
    }

    async fn get_run_budget(
        &self,
        run_id: RunId,
    ) -> Result<Option<RunBudgetSnapshot>, AdmissionError> {
        self.inner.get_run_budget(run_id).await
    }
}
