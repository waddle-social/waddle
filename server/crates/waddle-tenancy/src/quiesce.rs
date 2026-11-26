//! Local `BootInstant` lease validity guard, graceful quiesce/drain controller
//! respecting `WADDLE_DRAIN_TIMEOUT_SECS`, and secret-free JSONL trace emitter
//! compatible with `waddle_broker_core::trace_verifier` (`PlacementLeaseMonitor.p`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use thiserror::Error;
use tokio_util::sync::CancellationToken;
use waddle_broker_core::boot_clock::{BootClock, BootInstant};
use waddle_broker_core::holder_pop::format_trace_holder_key;
use waddle_broker_core::identity::{HolderInstance, TenantId};
use waddle_broker_core::placement::{
    FenceGeneration, LeaseError, PlacementLease, PlacementLeaseStore,
};
use waddle_broker_core::store::{TenantStore, TenantStoreError};
use waddle_broker_core::trace_verifier::{TraceEvent, TraceRecord};

/// Default drain timeout in seconds if `WADDLE_DRAIN_TIMEOUT_SECS` is unset.
pub const DEFAULT_DRAIN_TIMEOUT_SECS: u64 = 30;
/// Minimum clamped drain timeout in seconds.
pub const MIN_DRAIN_TIMEOUT_SECS: u64 = 1;
/// Maximum clamped drain timeout in seconds.
pub const MAX_DRAIN_TIMEOUT_SECS: u64 = 600;

/// Read `WADDLE_DRAIN_TIMEOUT_SECS` from the environment, clamped to `[1, 600]`.
pub fn drain_timeout_from_env() -> Duration {
    let secs = std::env::var("WADDLE_DRAIN_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DRAIN_TIMEOUT_SECS)
        .clamp(MIN_DRAIN_TIMEOUT_SECS, MAX_DRAIN_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Suspend-aware local validity guard for a placement lease measured on [`BootClock`].
#[derive(Clone)]
pub struct LocalLeaseGuard {
    clock: Arc<dyn BootClock>,
    tenant_id: TenantId,
    generation: FenceGeneration,
    valid_until: BootInstant,
    cancel: CancellationToken,
}

impl LocalLeaseGuard {
    /// Construct a local validity guard from the `sent_at` [`BootInstant`]
    /// captured **before** issuing `claim`/`renew`, subtracting `safety_margin`.
    /// Returns `None` if the clock is unavailable or `ttl <= safety_margin`.
    pub fn from_sent(
        clock: Arc<dyn BootClock>,
        tenant_id: TenantId,
        generation: FenceGeneration,
        sent_at: BootInstant,
        ttl: Duration,
        safety_margin: Duration,
    ) -> Option<Self> {
        let usable = ttl.checked_sub(safety_margin)?;
        if usable.is_zero() {
            return None;
        }
        let valid_until = sent_at.checked_add(usable)?;
        Some(Self {
            clock,
            tenant_id,
            generation,
            valid_until,
            cancel: CancellationToken::new(),
        })
    }

    pub fn tenant_id(&self) -> &TenantId {
        &self.tenant_id
    }

    pub const fn generation(&self) -> FenceGeneration {
        self.generation
    }

    pub const fn valid_until(&self) -> BootInstant {
        self.valid_until
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Trigger immediate local self-fencing (e.g., on drain timeout or lease loss).
    pub fn revoke_locally(&self) {
        self.cancel.cancel();
    }

    /// Returns `true` iff the lease has not been locally revoked and the
    /// suspend-aware boot clock is strictly before `valid_until`.
    pub fn is_valid(&self) -> bool {
        if self.cancel.is_cancelled() {
            return false;
        }
        self.clock.now().is_some_and(|now| now < self.valid_until)
    }
}

#[derive(Debug, Error)]
pub enum QuiesceError {
    #[error("boot clock unavailable during quiesce")]
    ClockUnavailable,
    #[error("drain budget ({0:?}) exceeded while in-flight work remained; aborted handoff for crash reclaim")]
    DrainTimeoutExceeded(Duration),
    #[error("storage checkpoint failed during quiesce: {0}")]
    Storage(#[from] TenantStoreError),
    #[error("placement lease handoff failed: {0}")]
    Lease(#[from] LeaseError),
}

/// Outcome of a graceful quiesce + handoff attempt.
#[derive(Debug, Clone)]
pub struct HandoffOutcome {
    pub new_lease: PlacementLease,
    pub drained_elapsed: Duration,
}

/// Coordinates graceful tenant quiesce within `drain_timeout` (measured on
/// [`BootClock`]), Turso WAL checkpointing, and atomic `PlacementLeaseStore::transfer`.
///
/// If in-flight work does not finish within `drain_timeout`, this controller
/// **refuses** to call `transfer()` while writes remain active; instead it
/// cancels the tenant's local [`CancellationToken`] and returns
/// [`QuiesceError::DrainTimeoutExceeded`] so crash-reclaim handles takeover
/// strictly after `expires_at` elapses.
pub struct QuiesceController {
    clock: Arc<dyn BootClock>,
    drain_timeout: Duration,
}

impl QuiesceController {
    pub fn new(clock: Arc<dyn BootClock>, drain_timeout: Duration) -> Self {
        Self {
            clock,
            drain_timeout,
        }
    }

    pub fn from_env(clock: Arc<dyn BootClock>) -> Self {
        Self::new(clock, drain_timeout_from_env())
    }

    /// Execute graceful handoff:
    /// 1. Run `quiesce_fn` (which stops new admissions, evicts `CommandRegistry`
    ///    sessions, and drains in-flight workers) while checking `BootClock`
    ///    against `drain_timeout`.
    /// 2. Checkpoint and release tenant storage handles (`checkpoint_and_release`).
    /// 3. Atomically transfer the placement lease to `to_holder`.
    #[allow(clippy::too_many_arguments)]
    pub async fn graceful_handoff<F, Fut>(
        &self,
        lease_store: &dyn PlacementLeaseStore,
        tenant_store: &dyn TenantStore,
        local_guard: &LocalLeaseGuard,
        tenant_id: &TenantId,
        from_holder: &HolderInstance,
        to_holder: &HolderInstance,
        expected_generation: FenceGeneration,
        new_ttl_ms: u64,
        quiesce_fn: F,
    ) -> Result<HandoffOutcome, QuiesceError>
    where
        F: FnOnce(BootInstant) -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let start = self.clock.now().ok_or(QuiesceError::ClockUnavailable)?;
        let deadline = start
            .checked_add(self.drain_timeout)
            .ok_or(QuiesceError::ClockUnavailable)?;

        let drained = quiesce_fn(deadline).await;
        let finished_at = self.clock.now().ok_or(QuiesceError::ClockUnavailable)?;
        let elapsed = finished_at.saturating_duration_since(start);

        if !drained || finished_at >= deadline {
            // Do NOT transfer while work is still in flight; self-fence and let
            // crash-reclaim take over after TTL expiry.
            local_guard.revoke_locally();
            return Err(QuiesceError::DrainTimeoutExceeded(self.drain_timeout));
        }

        // Revoke local admission before checkpoint + transfer.
        local_guard.revoke_locally();
        tenant_store.checkpoint_and_release(tenant_id).await?;
        let new_lease = lease_store
            .transfer(
                tenant_id,
                from_holder,
                to_holder,
                expected_generation,
                new_ttl_ms,
            )
            .await?;

        Ok(HandoffOutcome {
            new_lease,
            drained_elapsed: elapsed,
        })
    }
}

/// Secret-free JSONL trace emitter matching `PlacementLeaseMonitor.p` and
/// `waddle_broker_core::trace_verifier`.
#[derive(Debug, Default)]
pub struct PlacementTraceEmitter {
    logical_ts: AtomicU64,
    lines: Mutex<Vec<String>>,
}

impl PlacementTraceEmitter {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&self, actor_id: &str, tenant_hash: &str, event: TraceEvent) {
        let ts_logical = self.logical_ts.fetch_add(1, Ordering::SeqCst) + 1;
        let record = TraceRecord {
            ts_logical,
            actor_id: actor_id.to_owned(),
            tenant_hash: tenant_hash.to_owned(),
            event,
        };
        if let Ok(line) = serde_json::to_string(&record) {
            let mut guard = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
            guard.push(line);
        }
    }

    pub fn emit_claim(
        &self,
        actor_id: &str,
        tenant_hash: &str,
        holder: &HolderInstance,
        generation: FenceGeneration,
    ) {
        self.emit(
            actor_id,
            tenant_hash,
            TraceEvent::LeaseClaim {
                holder_key: format_trace_holder_key(holder),
                gen: generation.get(),
            },
        );
    }

    pub fn emit_transfer(
        &self,
        actor_id: &str,
        tenant_hash: &str,
        from_holder: &HolderInstance,
        to_holder: &HolderInstance,
        new_generation: FenceGeneration,
    ) {
        self.emit(
            actor_id,
            tenant_hash,
            TraceEvent::LeaseTransfer {
                from_key: format_trace_holder_key(from_holder),
                to_key: format_trace_holder_key(to_holder),
                new_gen: new_generation.get(),
            },
        );
    }

    pub fn emit_release(
        &self,
        actor_id: &str,
        tenant_hash: &str,
        holder: &HolderInstance,
        generation: FenceGeneration,
    ) {
        self.emit(
            actor_id,
            tenant_hash,
            TraceEvent::LeaseRelease {
                holder_key: format_trace_holder_key(holder),
                gen: generation.get(),
            },
        );
    }

    pub fn emit_tenant_write(
        &self,
        actor_id: &str,
        tenant_hash: &str,
        writer: &HolderInstance,
        generation: FenceGeneration,
    ) {
        self.emit(
            actor_id,
            tenant_hash,
            TraceEvent::TenantWrite {
                writer_key: format_trace_holder_key(writer),
                gen: generation.get(),
            },
        );
    }

    pub fn to_jsonl(&self) -> String {
        let guard = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        guard.join("\n")
    }
}
