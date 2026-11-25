//! Turso WAL-mode 3-fence `AdmissionLedgerStore` (`PRAGMA journal_mode = WAL`,
//! `PRAGMA synchronous = FULL`, `BEGIN IMMEDIATE`, `CAST(unixepoch('subsec') * 1000 AS INTEGER)`).
//!
//! All `u64` spend/budget/token counters are stored losslessly as base-10 `TEXT`
//! and checked inside `BEGIN IMMEDIATE` via Verus-verified `BudgetState` and
//! `three_fence_guard`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::sync::Mutex;
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

const ADMISSION_SCHEMA: &[&str] = &[
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

struct TursoHandle {
    _db: turso::Database,
    conn: turso::Connection,
}

/// Turso WAL-mode 3-fence admission ledger and replica fence store.
pub struct TursoAdmissionLedgerStore {
    path: PathBuf,
    handle: Mutex<TursoHandle>,
}

impl TursoAdmissionLedgerStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, AdmissionError> {
        let path_buf = path.as_ref().to_path_buf();
        if let Some(parent) = path_buf.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                AdmissionError::Backend(format!("create admission db dir failed: {err}"))
            })?;
        }
        let path_str = path_buf
            .to_str()
            .ok_or_else(|| AdmissionError::Backend("non-UTF-8 admission db path".to_owned()))?;
        let db = turso::Builder::new_local(path_str)
            .build()
            .await
            .map_err(|err| AdmissionError::Backend(format!("turso open failed: {err}")))?;
        let conn = db
            .connect()
            .map_err(|err| AdmissionError::Backend(format!("turso connect failed: {err}")))?;
        let _ = conn
            .pragma_update("journal_mode", "WAL")
            .await
            .map_err(|err| AdmissionError::Backend(format!("WAL pragma failed: {err}")))?;
        let _ = conn
            .pragma_update("synchronous", "FULL")
            .await
            .map_err(|err| AdmissionError::Backend(format!("synchronous pragma failed: {err}")))?;
        let _ = conn
            .pragma_update("foreign_keys", "ON")
            .await
            .map_err(|err| AdmissionError::Backend(format!("foreign_keys pragma failed: {err}")))?;
        for stmt in ADMISSION_SCHEMA {
            conn.execute(stmt, ())
                .await
                .map_err(|err| AdmissionError::Backend(format!("admission DDL failed: {err}")))?;
        }
        Ok(Self {
            path: path_buf,
            handle: Mutex::new(TursoHandle { _db: db, conn }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

async fn db_now_ms(conn: &turso::Connection) -> Result<i64, AdmissionError> {
    let mut rows = conn
        .query("SELECT CAST(unixepoch('subsec') * 1000 AS INTEGER)", ())
        .await
        .map_err(|err| AdmissionError::Backend(format!("clock query failed: {err}")))?;
    let row = rows
        .next()
        .await
        .map_err(|err| AdmissionError::Backend(format!("clock row failed: {err}")))?
        .ok_or_else(|| AdmissionError::Backend("clock query returned no row".to_owned()))?;
    row.get::<i64>(0)
        .map_err(|err| AdmissionError::Backend(format!("clock column decode failed: {err}")))
}

async fn begin_immediate(conn: &turso::Connection) -> Result<(), AdmissionError> {
    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|err| AdmissionError::Backend(format!("BEGIN IMMEDIATE failed: {err}")))?;
    Ok(())
}

async fn commit_tx(conn: &turso::Connection) -> Result<(), AdmissionError> {
    conn.execute("COMMIT", ())
        .await
        .map_err(|err| AdmissionError::Backend(format!("COMMIT failed: {err}")))?;
    Ok(())
}

async fn rollback_tx(conn: &turso::Connection) {
    let _ = conn.execute("ROLLBACK", ()).await;
}

async fn load_or_init_fence(
    conn: &turso::Connection,
    tenant: &TenantId,
    kind: &FenceKind,
    now_ms: i64,
) -> Result<FenceSnapshot, AdmissionError> {
    let (kind_tag, scope) = fence_kind_and_scope(kind);
    let mut rows = conn
        .query(
            "SELECT generation, state FROM admission_fences WHERE tenant_id = ?1 AND fence_kind = ?2 AND fence_scope = ?3",
            (tenant.as_str(), kind_tag, scope.as_str()),
        )
        .await
        .map_err(|err| AdmissionError::Backend(format!("fence select failed: {err}")))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|err| AdmissionError::Backend(format!("fence row failed: {err}")))?
    {
        let gen_i64: i64 = row
            .get(0)
            .map_err(|err| AdmissionError::Backend(format!("fence gen decode failed: {err}")))?;
        let state_str: String = row
            .get(1)
            .map_err(|err| AdmissionError::Backend(format!("fence state decode failed: {err}")))?;
        let gen_u64 = u64::try_from(gen_i64).map_err(|_| {
            AdmissionError::Backend("negative fence generation in ledger".to_owned())
        })?;
        let state = match state_str.as_str() {
            "open" => FenceState::Open,
            "closed" => FenceState::Closed,
            other => {
                return Err(AdmissionError::Backend(format!(
                    "invalid fence state: {other}"
                )));
            }
        };
        Ok(FenceSnapshot {
            generation: FenceGeneration::new(gen_u64),
            state,
        })
    } else {
        drop(rows);
        conn.execute(
            "INSERT INTO admission_fences (tenant_id, fence_kind, fence_scope, generation, state, updated_at_ms) VALUES (?1, ?2, ?3, 1, 'open', ?4)",
            (tenant.as_str(), kind_tag, scope.as_str(), now_ms),
        )
        .await
        .map_err(|err| AdmissionError::Backend(format!("fence init insert failed: {err}")))?;
        Ok(FenceSnapshot {
            generation: FenceGeneration::new(1),
            state: FenceState::Open,
        })
    }
}

async fn load_all_fence_generations(
    conn: &turso::Connection,
) -> Result<Vec<(FenceKey, FenceGeneration)>, AdmissionError> {
    let mut rows = conn
        .query(
            "SELECT tenant_id, fence_kind, fence_scope, generation FROM admission_fences",
            (),
        )
        .await
        .map_err(|err| AdmissionError::Backend(format!("list fences failed: {err}")))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| AdmissionError::Backend(format!("list fences row failed: {err}")))?
    {
        let tenant_str: String = row
            .get(0)
            .map_err(|err| AdmissionError::Backend(format!("tenant decode failed: {err}")))?;
        let kind_str: String = row
            .get(1)
            .map_err(|err| AdmissionError::Backend(format!("kind decode failed: {err}")))?;
        let scope_str: String = row
            .get(2)
            .map_err(|err| AdmissionError::Backend(format!("scope decode failed: {err}")))?;
        let gen_i64: i64 = row
            .get(3)
            .map_err(|err| AdmissionError::Backend(format!("gen decode failed: {err}")))?;
        let tenant_id = TenantId::parse(&tenant_str)
            .map_err(|err| AdmissionError::Backend(format!("invalid tenant_id in fence: {err}")))?;
        let kind = parse_fence_kind_and_scope(&kind_str, &scope_str)?;
        let gen_u64 = u64::try_from(gen_i64)
            .map_err(|_| AdmissionError::Backend("negative generation".to_owned()))?;
        out.push((FenceKey { tenant_id, kind }, FenceGeneration::new(gen_u64)));
    }
    Ok(out)
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

#[async_trait]
impl AdmissionLedgerStore for TursoAdmissionLedgerStore {
    async fn snapshot_fences(
        &self,
        tenant: &TenantId,
        credential: &CredentialKey,
        room_or_space: &RoomScopeKey,
    ) -> Result<ThreeFenceSnapshot, AdmissionError> {
        let guard = self.handle.lock().await;
        begin_immediate(&guard.conn).await?;
        let res = async {
            let now_ms = db_now_ms(&guard.conn).await?;
            let cred_snap = load_or_init_fence(
                &guard.conn,
                tenant,
                &FenceKind::Credential(credential.clone()),
                now_ms,
            )
            .await?;
            let room_snap = load_or_init_fence(
                &guard.conn,
                tenant,
                &FenceKind::RoomOrSpace(room_or_space.clone()),
                now_ms,
            )
            .await?;
            let place_snap =
                load_or_init_fence(&guard.conn, tenant, &FenceKind::Placement, now_ms).await?;
            Ok(ThreeFenceSnapshot {
                credential: cred_snap,
                room_or_space: room_snap,
                placement: place_snap,
            })
        }
        .await;
        match res {
            Ok(snap) => {
                commit_tx(&guard.conn).await?;
                Ok(snap)
            }
            Err(err) => {
                rollback_tx(&guard.conn).await;
                Err(err)
            }
        }
    }

    async fn close_and_advance_fence(
        &self,
        key: &FenceKey,
    ) -> Result<FenceGeneration, AdmissionError> {
        let guard = self.handle.lock().await;
        begin_immediate(&guard.conn).await?;
        let res = async {
            let now_ms = db_now_ms(&guard.conn).await?;
            let current = load_or_init_fence(&guard.conn, &key.tenant_id, &key.kind, now_ms).await?;
            let next = close_and_advance(to_kernel_fence(current)).map_err(|err| {
                AdmissionError::Backend(format!("fence generation overflow: {err}"))
            })?;
            let next_i64 = i64::try_from(next.generation).map_err(|_| {
                AdmissionError::Backend("fence generation exceeds SQLite i64::MAX".to_owned())
            })?;
            let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
            guard
                .conn
                .execute(
                    "UPDATE admission_fences SET generation = ?1, state = 'closed', updated_at_ms = ?2 WHERE tenant_id = ?3 AND fence_kind = ?4 AND fence_scope = ?5",
                    (next_i64, now_ms, key.tenant_id.as_str(), kind_tag, scope.as_str()),
                )
                .await
                .map_err(|err| AdmissionError::Backend(format!("fence close update failed: {err}")))?;
            Ok(FenceGeneration::new(next.generation))
        }
        .await;
        match res {
            Ok(gen) => {
                commit_tx(&guard.conn).await?;
                Ok(gen)
            }
            Err(err) => {
                rollback_tx(&guard.conn).await;
                Err(err)
            }
        }
    }

    async fn reopen_fence(
        &self,
        key: &FenceKey,
        expected_generation: FenceGeneration,
    ) -> Result<(), AdmissionError> {
        let guard = self.handle.lock().await;
        begin_immediate(&guard.conn).await?;
        let res = async {
            let now_ms = db_now_ms(&guard.conn).await?;
            let current = load_or_init_fence(&guard.conn, &key.tenant_id, &key.kind, now_ms).await?;
            let _reopened = reopen_fence(to_kernel_fence(current), expected_generation.get())
                .map_err(|err| match err {
                    FenceGuardError::GenerationMismatch | FenceGuardError::AlreadyOpen => {
                        AdmissionError::FenceStaleOrClosed(key.kind.clone())
                    }
                    FenceGuardError::GenerationOverflow => {
                        AdmissionError::Backend(err.to_string())
                    }
                })?;
            let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
            guard
                .conn
                .execute(
                    "UPDATE admission_fences SET state = 'open', updated_at_ms = ?1 WHERE tenant_id = ?2 AND fence_kind = ?3 AND fence_scope = ?4",
                    (now_ms, key.tenant_id.as_str(), kind_tag, scope.as_str()),
                )
                .await
                .map_err(|err| AdmissionError::Backend(format!("fence reopen update failed: {err}")))?;
            Ok(())
        }
        .await;
        match res {
            Ok(()) => {
                commit_tx(&guard.conn).await?;
                Ok(())
            }
            Err(err) => {
                rollback_tx(&guard.conn).await;
                Err(err)
            }
        }
    }

    async fn admit_attempt(
        &self,
        req: &AdmitAttemptRequest,
    ) -> Result<AdmitReceipt, AdmissionError> {
        let guard = self.handle.lock().await;
        begin_immediate(&guard.conn).await?;
        let res = async {
            let now_ms = db_now_ms(&guard.conn).await?;
            if now_ms >= req.expires_at_ms {
                return Err(AdmissionError::InvocationExpired);
            }
            if req.holder.spiffe_id.as_str().is_empty() {
                return Err(AdmissionError::HolderUnauthorized);
            }

            let cred_kind = FenceKind::Credential(req.credential.clone());
            let room_kind = FenceKind::RoomOrSpace(req.room_or_space.clone());
            let cred_cur = load_or_init_fence(&guard.conn, &req.tenant_id, &cred_kind, now_ms).await?;
            let room_cur = load_or_init_fence(&guard.conn, &req.tenant_id, &room_kind, now_ms).await?;
            let place_cur =
                load_or_init_fence(&guard.conn, &req.tenant_id, &FenceKind::Placement, now_ms)
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

            // Check duplicate attempt_id.
            {
                let mut rows = guard
                    .conn
                    .query(
                        "SELECT status FROM admission_attempts WHERE attempt_id = ?1",
                        [attempt_id_str.as_str()],
                    )
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("attempt lookup failed: {err}")))?;
                if let Some(row) = rows
                    .next()
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("attempt row failed: {err}")))?
                {
                    let status_str: String = row.get(0).map_err(|err| {
                        AdmissionError::Backend(format!("attempt status decode failed: {err}"))
                    })?;
                    if status_str == AttemptStatus::Quarantined.as_str() {
                        return Err(AdmissionError::Quarantined);
                    }
                    return Err(AdmissionError::DuplicateAttempt);
                }
            }

            // Check duplicate (tenant_id, nonce_bytes) replay.
            {
                let mut rows = guard
                    .conn
                    .query(
                        "SELECT status FROM admission_attempts WHERE tenant_id = ?1 AND nonce_bytes = ?2",
                        (req.tenant_id.as_str(), req.invocation_nonce.as_slice()),
                    )
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("nonce lookup failed: {err}")))?;
                if let Some(row) = rows
                    .next()
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("nonce row failed: {err}")))?
                {
                    let status_str: String = row.get(0).map_err(|err| {
                        AdmissionError::Backend(format!("nonce status decode failed: {err}"))
                    })?;
                    if status_str == AttemptStatus::Quarantined.as_str() {
                        return Err(AdmissionError::Quarantined);
                    }
                    return Err(AdmissionError::NonceReplayed);
                }
            }

            // Check duplicate (run_id, step_id).
            {
                let mut rows = guard
                    .conn
                    .query(
                        "SELECT 1 FROM admission_attempts WHERE run_id = ?1 AND step_id = ?2",
                        (run_id_str.as_str(), step_i64),
                    )
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("step lookup failed: {err}")))?;
                if rows
                    .next()
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("step row failed: {err}")))?
                    .is_some()
                {
                    return Err(AdmissionError::DuplicateAttempt);
                }
            }

            // Load or initialize run budget.
            let (mut budget, max_steps, steps_used) = {
                let mut rows = guard
                    .conn
                    .query(
                        "SELECT tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used FROM admission_runs WHERE run_id = ?1",
                        [run_id_str.as_str()],
                    )
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("run select failed: {err}")))?;
                if let Some(row) = rows
                    .next()
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("run row failed: {err}")))?
                {
                    let row_tenant_id: String = row.get(0).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                    if row_tenant_id != req.tenant_id.as_str() {
                        return Err(AdmissionError::HolderUnauthorized);
                    }
                    let cap_s: String = row.get(1).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                    let spent_s: String = row.get(2).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                    let res_s: String = row.get(3).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                    let max_s: i64 = row.get(4).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                    let used_s: i64 = row.get(5).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                    (
                        BudgetState {
                            signed_cap: decode_u64(&cap_s)?,
                            spent: decode_u64(&spent_s)?,
                            reserved: decode_u64(&res_s)?,
                        },
                        u32::try_from(max_s).unwrap_or(0),
                        u32::try_from(used_s).unwrap_or(u32::MAX),
                    )
                } else {
                    drop(rows);
                    guard
                        .conn
                        .execute(
                            "INSERT INTO admission_runs (run_id, tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used, created_at_ms) VALUES (?1, ?2, ?3, '0', '0', ?4, 0, ?5, '0', ?6, '0', ?7)",
                            (
                                run_id_str.as_str(),
                                req.tenant_id.as_str(),
                                encode_u64(req.budget.max_spend_microcents),
                                i64::from(req.budget.max_steps),
                                encode_u64(req.budget.max_input_tokens),
                                encode_u64(req.budget.max_output_tokens),
                                now_ms,
                            ),
                        )
                        .await
                        .map_err(|err| AdmissionError::Backend(format!("run insert failed: {err}")))?;
                    (
                        BudgetState::new(req.budget.max_spend_microcents),
                        req.budget.max_steps,
                        0u32,
                    )
                }
            };

            if steps_used >= max_steps {
                return Err(AdmissionError::BudgetExceeded);
            }
            budget
                .reserve(req.reserved_spend_microcents)
                .map_err(map_budget_err)?;
            let next_steps = steps_used
                .checked_add(1)
                .ok_or(AdmissionError::BudgetExceeded)?;

            guard
                .conn
                .execute(
                    "UPDATE admission_runs SET reserved_microcents = ?1, steps_used = ?2 WHERE run_id = ?3",
                    (encode_u64(budget.reserved), i64::from(next_steps), run_id_str.as_str()),
                )
                .await
                .map_err(|err| AdmissionError::Backend(format!("run update failed: {err}")))?;

            let (_, cred_scope_str) = fence_kind_and_scope(&cred_kind);
            guard
                .conn
                .execute(
                    "INSERT INTO admission_attempts (attempt_id, run_id, step_id, tenant_id, credential_scope, room_scope, holder_spiffe_id, holder_incarnation_pubkey, nonce_bytes, status, reserved_spend_microcents, actual_spend_microcents, input_tokens, output_tokens, admitted_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'admitted', ?10, '0', '0', '0', ?11)",
                    (
                        attempt_id_str.as_str(),
                        run_id_str.as_str(),
                        step_i64,
                        req.tenant_id.as_str(),
                        cred_scope_str.as_str(),
                        req.room_or_space.scope_id.as_str(),
                        req.holder.spiffe_id.as_str(),
                        req.holder.incarnation_pubkey.as_bytes().as_slice(),
                        req.invocation_nonce.as_slice(),
                        encode_u64(req.reserved_spend_microcents),
                        now_ms,
                    ),
                )
                .await
                .map_err(|err| AdmissionError::Backend(format!("attempt insert failed: {err}")))?;

            Ok(AdmitReceipt {
                attempt_id: req.attempt_id,
                run_id: req.run_id,
                step_id: req.step_id,
                admitted_at_ms: now_ms,
                reserved_spend_microcents: req.reserved_spend_microcents,
            })
        }
        .await;
        match res {
            Ok(receipt) => {
                commit_tx(&guard.conn).await?;
                Ok(receipt)
            }
            Err(err) => {
                rollback_tx(&guard.conn).await;
                Err(err)
            }
        }
    }

    async fn reconcile_attempt(
        &self,
        attempt_id: AttemptId,
        outcome: AttemptOutcome,
    ) -> Result<(), AdmissionError> {
        let guard = self.handle.lock().await;
        begin_immediate(&guard.conn).await?;
        let res = async {
            let attempt_id_str = attempt_id.as_uuid().to_string();
            let (run_id_str, status_str, reserved_spend) = {
                let mut rows = guard
                    .conn
                    .query(
                        "SELECT run_id, status, reserved_spend_microcents FROM admission_attempts WHERE attempt_id = ?1",
                        [attempt_id_str.as_str()],
                    )
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("reconcile select failed: {err}")))?;
                let row = rows
                    .next()
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("reconcile row failed: {err}")))?
                    .ok_or(AdmissionError::AttemptNotFound)?;
                let run_id_s: String = row.get(0).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                let status_s: String = row.get(1).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                let reserved_s: String = row.get(2).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                (run_id_s, status_s, decode_u64(&reserved_s)?)
            };

            let current_status = AttemptStatus::parse(&status_str)?;
            if current_status == AttemptStatus::Quarantined {
                return Err(AdmissionError::Quarantined);
            }
            if current_status != AttemptStatus::Admitted {
                return Err(AdmissionError::Backend(format!(
                    "attempt already reconciled in status {}",
                    current_status.as_str()
                )));
            }

            let (mut budget, max_in, in_used, max_out, out_used) = {
                let mut rows = guard
                    .conn
                    .query(
                        "SELECT max_spend_microcents, spent_microcents, reserved_microcents, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used FROM admission_runs WHERE run_id = ?1",
                        [run_id_str.as_str()],
                    )
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("run budget select failed: {err}")))?;
                let row = rows
                    .next()
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("run budget row failed: {err}")))?
                    .ok_or_else(|| AdmissionError::Backend("missing run row".to_owned()))?;
                let cap_s: String = row.get(0).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                let spent_s: String = row.get(1).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                let res_s: String = row.get(2).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                let max_in_s: String = row.get(3).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                let in_s: String = row.get(4).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                let max_out_s: String = row.get(5).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                let out_s: String = row.get(6).map_err(|e| AdmissionError::Backend(e.to_string()))?;
                (
                    BudgetState {
                        signed_cap: decode_u64(&cap_s)?,
                        spent: decode_u64(&spent_s)?,
                        reserved: decode_u64(&res_s)?,
                    },
                    decode_u64(&max_in_s)?,
                    decode_u64(&in_s)?,
                    decode_u64(&max_out_s)?,
                    decode_u64(&out_s)?,
                )
            };

            let (new_status, actual_spend, new_in_tokens, new_out_tokens, attempt_in, attempt_out) =
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

            guard
                .conn
                .execute(
                    "UPDATE admission_runs SET spent_microcents = ?1, reserved_microcents = ?2, input_tokens_used = ?3, output_tokens_used = ?4 WHERE run_id = ?5",
                    (
                        encode_u64(budget.spent),
                        encode_u64(budget.reserved),
                        encode_u64(new_in_tokens),
                        encode_u64(new_out_tokens),
                        run_id_str.as_str(),
                    ),
                )
                .await
                .map_err(|err| AdmissionError::Backend(format!("run reconcile update failed: {err}")))?;

            guard
                .conn
                .execute(
                    "UPDATE admission_attempts SET status = ?1, actual_spend_microcents = ?2, input_tokens = ?3, output_tokens = ?4 WHERE attempt_id = ?5",
                    (
                        new_status.as_str(),
                        encode_u64(actual_spend),
                        encode_u64(attempt_in),
                        encode_u64(attempt_out),
                        attempt_id_str.as_str(),
                    ),
                )
                .await
                .map_err(|err| {
                    AdmissionError::Backend(format!("attempt reconcile update failed: {err}"))
                })?;

            Ok(())
        }
        .await;
        match res {
            Ok(()) => {
                commit_tx(&guard.conn).await?;
                Ok(())
            }
            Err(err) => {
                rollback_tx(&guard.conn).await;
                Err(err)
            }
        }
    }
}

#[async_trait]
impl ReplicaFenceStore for TursoAdmissionLedgerStore {
    async fn register_replica(
        &self,
        replica_id: Uuid,
        ttl_ms: i64,
    ) -> Result<Vec<(FenceKey, FenceGeneration)>, AdmissionError> {
        let guard = self.handle.lock().await;
        begin_immediate(&guard.conn).await?;
        let res = async {
            let now_ms = db_now_ms(&guard.conn).await?;
            let expires_at = now_ms.saturating_add(ttl_ms);
            let id_str = replica_id.to_string();
            guard
                .conn
                .execute(
                    "INSERT INTO admission_fence_replicas (replica_id, registered_at_ms, lease_expires_at_ms) VALUES (?1, ?2, ?3) ON CONFLICT(replica_id) DO UPDATE SET lease_expires_at_ms = excluded.lease_expires_at_ms",
                    (id_str.as_str(), now_ms, expires_at),
                )
                .await
                .map_err(|err| AdmissionError::Backend(format!("register replica failed: {err}")))?;
            load_all_fence_generations(&guard.conn).await
        }
        .await;
        match res {
            Ok(fences) => {
                commit_tx(&guard.conn).await?;
                Ok(fences)
            }
            Err(err) => {
                rollback_tx(&guard.conn).await;
                Err(err)
            }
        }
    }

    async fn renew_replica(
        &self,
        replica_id: Uuid,
        ttl_ms: i64,
    ) -> Result<Option<Vec<(FenceKey, FenceGeneration)>>, AdmissionError> {
        let guard = self.handle.lock().await;
        begin_immediate(&guard.conn).await?;
        let res = async {
            let now_ms = db_now_ms(&guard.conn).await?;
            let expires_at = now_ms.saturating_add(ttl_ms);
            let id_str = replica_id.to_string();
            let updated = guard
                .conn
                .execute(
                    "UPDATE admission_fence_replicas SET lease_expires_at_ms = ?1 WHERE replica_id = ?2",
                    (expires_at, id_str.as_str()),
                )
                .await
                .map_err(|err| AdmissionError::Backend(format!("renew replica failed: {err}")))?;
            if updated == 0 {
                return Ok(None);
            }
            let fences = load_all_fence_generations(&guard.conn).await?;
            Ok(Some(fences))
        }
        .await;
        match res {
            Ok(opt) => {
                commit_tx(&guard.conn).await?;
                Ok(opt)
            }
            Err(err) => {
                rollback_tx(&guard.conn).await;
                Err(err)
            }
        }
    }

    async fn ack_replica_fences(
        &self,
        replica_id: Uuid,
        acks: &[(FenceKey, FenceGeneration)],
    ) -> Result<(), AdmissionError> {
        if acks.is_empty() {
            return Ok(());
        }
        let guard = self.handle.lock().await;
        begin_immediate(&guard.conn).await?;
        let res = async {
            let now_ms = db_now_ms(&guard.conn).await?;
            let id_str = replica_id.to_string();
            for (key, gen) in acks {
                let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
                let gen_i64 = i64::try_from(gen.get()).map_err(|_| {
                    AdmissionError::Backend("generation exceeds i64::MAX".to_owned())
                })?;
                guard
                    .conn
                    .execute(
                        "INSERT INTO admission_fence_acks (replica_id, tenant_id, fence_kind, fence_scope, generation, acknowledged_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(replica_id, tenant_id, fence_kind, fence_scope) DO UPDATE SET generation = MAX(admission_fence_acks.generation, excluded.generation), acknowledged_at_ms = excluded.acknowledged_at_ms",
                        (id_str.as_str(), key.tenant_id.as_str(), kind_tag, scope.as_str(), gen_i64, now_ms),
                    )
                    .await
                    .map_err(|err| AdmissionError::Backend(format!("ack fence failed: {err}")))?;
            }
            Ok(())
        }
        .await;
        match res {
            Ok(()) => {
                commit_tx(&guard.conn).await?;
                Ok(())
            }
            Err(err) => {
                rollback_tx(&guard.conn).await;
                Err(err)
            }
        }
    }

    async fn release_replica(&self, replica_id: Uuid) -> Result<(), AdmissionError> {
        let guard = self.handle.lock().await;
        let id_str = replica_id.to_string();
        guard
            .conn
            .execute(
                "DELETE FROM admission_fence_acks WHERE replica_id = ?1",
                [id_str.as_str()],
            )
            .await
            .map_err(|err| AdmissionError::Backend(format!("delete replica acks failed: {err}")))?;
        guard
            .conn
            .execute(
                "DELETE FROM admission_fence_replicas WHERE replica_id = ?1",
                [id_str.as_str()],
            )
            .await
            .map_err(|err| AdmissionError::Backend(format!("delete replica failed: {err}")))?;
        Ok(())
    }

    async fn check_revocation_ack(
        &self,
        key: &FenceKey,
        target_gen: FenceGeneration,
        expiry_grace_ms: i64,
    ) -> Result<FenceAcknowledgement, AdmissionError> {
        let guard = self.handle.lock().await;
        let now_ms = db_now_ms(&guard.conn).await?;
        let cutoff_ms = now_ms.saturating_sub(expiry_grace_ms);
        let (kind_tag, scope) = fence_kind_and_scope(&key.kind);
        let target_i64 = i64::try_from(target_gen.get())
            .map_err(|_| AdmissionError::Backend("target_gen exceeds i64::MAX".to_owned()))?;
        let mut rows = guard
            .conn
            .query(
                "SELECT COUNT(*) FROM admission_fence_replicas r WHERE r.lease_expires_at_ms > ?1 AND NOT EXISTS (SELECT 1 FROM admission_fence_acks a WHERE a.replica_id = r.replica_id AND a.tenant_id = ?2 AND a.fence_kind = ?3 AND a.fence_scope = ?4 AND a.generation >= ?5)",
                (cutoff_ms, key.tenant_id.as_str(), kind_tag, scope.as_str(), target_i64),
            )
            .await
            .map_err(|err| AdmissionError::Backend(format!("check revocation ack failed: {err}")))?;
        let row = rows
            .next()
            .await
            .map_err(|err| AdmissionError::Backend(format!("check ack row failed: {err}")))?
            .ok_or_else(|| AdmissionError::Backend("count query returned no row".to_owned()))?;
        let count: i64 = row
            .get(0)
            .map_err(|err| AdmissionError::Backend(format!("count decode failed: {err}")))?;
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
impl AdmissionQueryStore for TursoAdmissionLedgerStore {
    async fn get_attempt(
        &self,
        attempt_id: AttemptId,
    ) -> Result<Option<AttemptRecord>, AdmissionError> {
        let guard = self.handle.lock().await;
        let id_str = attempt_id.as_uuid().to_string();
        let mut rows = guard
            .conn
            .query(
                "SELECT attempt_id, run_id, step_id, tenant_id, status, reserved_spend_microcents, actual_spend_microcents, input_tokens, output_tokens, admitted_at_ms FROM admission_attempts WHERE attempt_id = ?1",
                [id_str.as_str()],
            )
            .await
            .map_err(|err| AdmissionError::Backend(format!("get_attempt failed: {err}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|err| AdmissionError::Backend(format!("get_attempt row failed: {err}")))?
        else {
            return Ok(None);
        };
        decode_attempt_row(&row).map(Some)
    }

    async fn get_attempt_by_nonce(
        &self,
        tenant_id: &TenantId,
        nonce: &[u8; 32],
    ) -> Result<Option<AttemptRecord>, AdmissionError> {
        let guard = self.handle.lock().await;
        let mut rows = guard
            .conn
            .query(
                "SELECT attempt_id, run_id, step_id, tenant_id, status, reserved_spend_microcents, actual_spend_microcents, input_tokens, output_tokens, admitted_at_ms FROM admission_attempts WHERE tenant_id = ?1 AND nonce_bytes = ?2",
                (tenant_id.as_str(), nonce.as_slice()),
            )
            .await
            .map_err(|err| AdmissionError::Backend(format!("get_attempt_by_nonce failed: {err}")))?;
        let Some(row) = rows.next().await.map_err(|err| {
            AdmissionError::Backend(format!("get_attempt_by_nonce row failed: {err}"))
        })?
        else {
            return Ok(None);
        };
        decode_attempt_row(&row).map(Some)
    }

    async fn get_run_budget(
        &self,
        run_id: RunId,
    ) -> Result<Option<RunBudgetSnapshot>, AdmissionError> {
        let guard = self.handle.lock().await;
        let id_str = run_id.as_uuid().to_string();
        let mut rows = guard
            .conn
            .query(
                "SELECT run_id, tenant_id, max_spend_microcents, spent_microcents, reserved_microcents, max_steps, steps_used, max_input_tokens, input_tokens_used, max_output_tokens, output_tokens_used FROM admission_runs WHERE run_id = ?1",
                [id_str.as_str()],
            )
            .await
            .map_err(|err| AdmissionError::Backend(format!("get_run_budget failed: {err}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|err| AdmissionError::Backend(format!("get_run_budget row failed: {err}")))?
        else {
            return Ok(None);
        };
        let run_s: String = row
            .get(0)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let tenant_s: String = row
            .get(1)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let cap_s: String = row
            .get(2)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let spent_s: String = row
            .get(3)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let res_s: String = row
            .get(4)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_steps: i64 = row
            .get(5)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let steps_used: i64 = row
            .get(6)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_in: String = row
            .get(7)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let in_used: String = row
            .get(8)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let max_out: String = row
            .get(9)
            .map_err(|e| AdmissionError::Backend(e.to_string()))?;
        let out_used: String = row
            .get(10)
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

fn decode_attempt_row(row: &turso::Row) -> Result<AttemptRecord, AdmissionError> {
    let attempt_s: String = row
        .get(0)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let run_s: String = row
        .get(1)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let step_i64: i64 = row
        .get(2)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let tenant_s: String = row
        .get(3)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let status_s: String = row
        .get(4)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let reserved_s: String = row
        .get(5)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let actual_s: String = row
        .get(6)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let in_s: String = row
        .get(7)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let out_s: String = row
        .get(8)
        .map_err(|e| AdmissionError::Backend(e.to_string()))?;
    let admitted_at_ms: i64 = row
        .get(9)
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
