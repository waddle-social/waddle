//! Per-tenant bounded concurrency semaphores, aggregate Wasm memory tracking,
//! and strictly segregated control-plane capacity for lease heartbeats and
//! cancellation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::identity::TenantId;

/// Minimum reserved control-plane permits (at least 1 for lease heartbeat +
/// 1 for stream/task cancellation).
pub const MIN_CONTROL_PLANE_PERMITS: usize = 2;

/// Per-tenant resource bounds enforced before instantiation and during execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantResourceQuotas {
    /// Max concurrent actor mailbox dispatches / queued messages per tenant.
    pub max_mailbox_permits: usize,
    /// Max concurrent background tasks (observers, outbox, push) per tenant.
    pub max_concurrent_tasks: usize,
    /// Max concurrent active XMPP/WS/inference streams per tenant.
    pub max_outstanding_streams: usize,
    /// Max concurrent Wasm instantiations per tenant.
    pub max_wasm_instances: usize,
    /// Aggregate Wasm linear memory cap across all active stores for this tenant (bytes).
    pub max_wasm_memory_bytes: usize,
    /// Per-invocation Wasm fuel ceiling for this tenant.
    pub max_wasm_fuel_per_invocation: u64,
    /// Per-invocation wall-clock deadline ceiling (ms).
    pub max_wasm_wall_clock_ms: u32,
    /// Max concurrent DB operations / connections per tenant.
    pub max_db_concurrency: usize,
    /// Reserved permits strictly for lease heartbeats and cancellation signals
    /// (never consumable by data-plane traffic; must be >= 2).
    pub reserved_control_plane_permits: usize,
}

impl Default for TenantResourceQuotas {
    fn default() -> Self {
        Self {
            max_mailbox_permits: 256,
            max_concurrent_tasks: 32,
            max_outstanding_streams: 64,
            max_wasm_instances: 8,
            max_wasm_memory_bytes: 256 * 1024 * 1024,
            max_wasm_fuel_per_invocation: 50_000_000,
            max_wasm_wall_clock_ms: 45_000,
            max_db_concurrency: 16,
            reserved_control_plane_permits: 4,
        }
    }
}

impl TenantResourceQuotas {
    pub fn validate(&self) -> Result<(), QuotaError> {
        if self.max_mailbox_permits == 0
            || self.max_concurrent_tasks == 0
            || self.max_outstanding_streams == 0
            || self.max_wasm_instances == 0
            || self.max_wasm_memory_bytes == 0
            || self.max_wasm_fuel_per_invocation == 0
            || self.max_wasm_wall_clock_ms == 0
            || self.max_db_concurrency == 0
        {
            return Err(QuotaError::InvalidConfiguration(
                "all data-plane quota limits must be >= 1".to_owned(),
            ));
        }
        if self.reserved_control_plane_permits < MIN_CONTROL_PLANE_PERMITS {
            return Err(QuotaError::InvalidConfiguration(format!(
                "reserved_control_plane_permits must be >= {MIN_CONTROL_PLANE_PERMITS}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuotaResourceKind {
    Mailbox,
    BackgroundTask,
    Stream,
    WasmInstance,
    WasmMemory,
    Database,
    ControlPlaneHeartbeat,
    ControlPlaneCancellation,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum QuotaError {
    #[error("invalid tenant resource quota configuration: {0}")]
    InvalidConfiguration(String),
    #[error("tenant {tenant_id} exceeded {resource:?} quota")]
    Exceeded {
        tenant_id: TenantId,
        resource: QuotaResourceKind,
    },
}

/// Lock-free atomic tracker enforcing a tenant's aggregate Wasm linear memory
/// budget across all concurrently active `wasmtime::Store` instances.
#[derive(Debug)]
pub struct TenantMemoryTracker {
    tenant_id: TenantId,
    max_bytes: usize,
    allocated_bytes: AtomicUsize,
}

impl TenantMemoryTracker {
    pub fn new(tenant_id: TenantId, max_bytes: usize) -> Self {
        Self {
            tenant_id,
            max_bytes,
            allocated_bytes: AtomicUsize::new(0),
        }
    }

    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub fn allocated_bytes(&self) -> usize {
        self.allocated_bytes.load(Ordering::Acquire)
    }

    /// Atomically reserve `delta` bytes if `allocated + delta <= max_bytes`.
    pub fn try_reserve(&self, delta: usize) -> Result<(), QuotaError> {
        if delta == 0 {
            return Ok(());
        }
        self.allocated_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                let next = current.checked_add(delta)?;
                if next <= self.max_bytes {
                    Some(next)
                } else {
                    None
                }
            })
            .map(|_| ())
            .map_err(|_| QuotaError::Exceeded {
                tenant_id: self.tenant_id.clone(),
                resource: QuotaResourceKind::WasmMemory,
            })
    }

    /// Release `delta` bytes back to the tenant's Wasm memory budget.
    pub fn release(&self, delta: usize) {
        if delta == 0 {
            return;
        }
        let _ = self
            .allocated_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_sub(delta))
            });
    }
}

/// RAII permit acquired **before** `wasmtime::Store::new` / instantiation.
/// Releases the Wasm instance semaphore permit and initial memory reservation on drop.
#[derive(Debug)]
pub struct WasmInvocationPermit {
    _instance_permit: OwnedSemaphorePermit,
    memory_tracker: Arc<TenantMemoryTracker>,
    initial_reserved_bytes: usize,
}

impl WasmInvocationPermit {
    pub fn memory_tracker(&self) -> Arc<TenantMemoryTracker> {
        Arc::clone(&self.memory_tracker)
    }

    pub const fn initial_reserved_bytes(&self) -> usize {
        self.initial_reserved_bytes
    }
}

impl Drop for WasmInvocationPermit {
    fn drop(&mut self) {
        if self.initial_reserved_bytes > 0 {
            self.memory_tracker.release(self.initial_reserved_bytes);
        }
    }
}

#[derive(Debug)]
struct TenantResourceBucket {
    mailbox_sem: Arc<Semaphore>,
    task_sem: Arc<Semaphore>,
    stream_sem: Arc<Semaphore>,
    wasm_instance_sem: Arc<Semaphore>,
    wasm_memory: Arc<TenantMemoryTracker>,
    db_sem: Arc<Semaphore>,
    control_plane_sem: Arc<Semaphore>,
}

impl TenantResourceBucket {
    fn new(tenant_id: TenantId, quotas: &TenantResourceQuotas) -> Self {
        Self {
            mailbox_sem: Arc::new(Semaphore::new(quotas.max_mailbox_permits)),
            task_sem: Arc::new(Semaphore::new(quotas.max_concurrent_tasks)),
            stream_sem: Arc::new(Semaphore::new(quotas.max_outstanding_streams)),
            wasm_instance_sem: Arc::new(Semaphore::new(quotas.max_wasm_instances)),
            wasm_memory: Arc::new(TenantMemoryTracker::new(
                tenant_id,
                quotas.max_wasm_memory_bytes,
            )),
            db_sem: Arc::new(Semaphore::new(quotas.max_db_concurrency)),
            control_plane_sem: Arc::new(Semaphore::new(quotas.reserved_control_plane_permits)),
        }
    }
}

/// Per-tenant resource controller isolating data-plane semaphores across tenants
/// and strictly segregating control-plane permits for heartbeats & cancellation.
#[derive(Debug)]
pub struct TenantResourceController {
    default_quotas: TenantResourceQuotas,
    buckets: RwLock<HashMap<TenantId, Arc<TenantResourceBucket>>>,
}

impl TenantResourceController {
    pub fn new(default_quotas: TenantResourceQuotas) -> Result<Self, QuotaError> {
        default_quotas.validate()?;
        Ok(Self {
            default_quotas,
            buckets: RwLock::new(HashMap::new()),
        })
    }

    pub fn default_quotas(&self) -> &TenantResourceQuotas {
        &self.default_quotas
    }

    /// Override quotas for a specific tenant (e.g., dedicated cell tier).
    pub fn configure_tenant(
        &self,
        tenant_id: TenantId,
        quotas: &TenantResourceQuotas,
    ) -> Result<(), QuotaError> {
        quotas.validate()?;
        let mut guard = self.buckets.write().unwrap_or_else(PoisonError::into_inner);
        guard.insert(
            tenant_id.clone(),
            Arc::new(TenantResourceBucket::new(tenant_id, quotas)),
        );
        Ok(())
    }

    /// Remove a tenant's resource bucket (e.g., on tenant purge).
    pub fn remove_tenant(&self, tenant_id: &TenantId) {
        let mut guard = self.buckets.write().unwrap_or_else(PoisonError::into_inner);
        guard.remove(tenant_id);
    }

    fn bucket_for(&self, tenant_id: &TenantId) -> Arc<TenantResourceBucket> {
        {
            let guard = self.buckets.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(bucket) = guard.get(tenant_id) {
                return Arc::clone(bucket);
            }
        }
        let mut guard = self.buckets.write().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(guard.entry(tenant_id.clone()).or_insert_with(|| {
            Arc::new(TenantResourceBucket::new(
                tenant_id.clone(),
                &self.default_quotas,
            ))
        }))
    }

    pub fn memory_tracker(&self, tenant_id: &TenantId) -> Arc<TenantMemoryTracker> {
        Arc::clone(&self.bucket_for(tenant_id).wasm_memory)
    }

    pub fn try_acquire_mailbox(
        &self,
        tenant_id: &TenantId,
    ) -> Result<OwnedSemaphorePermit, QuotaError> {
        let bucket = self.bucket_for(tenant_id);
        Arc::clone(&bucket.mailbox_sem)
            .try_acquire_owned()
            .map_err(|_| QuotaError::Exceeded {
                tenant_id: tenant_id.clone(),
                resource: QuotaResourceKind::Mailbox,
            })
    }

    pub fn try_acquire_task(
        &self,
        tenant_id: &TenantId,
    ) -> Result<OwnedSemaphorePermit, QuotaError> {
        let bucket = self.bucket_for(tenant_id);
        Arc::clone(&bucket.task_sem)
            .try_acquire_owned()
            .map_err(|_| QuotaError::Exceeded {
                tenant_id: tenant_id.clone(),
                resource: QuotaResourceKind::BackgroundTask,
            })
    }

    pub fn try_acquire_stream(
        &self,
        tenant_id: &TenantId,
    ) -> Result<OwnedSemaphorePermit, QuotaError> {
        let bucket = self.bucket_for(tenant_id);
        Arc::clone(&bucket.stream_sem)
            .try_acquire_owned()
            .map_err(|_| QuotaError::Exceeded {
                tenant_id: tenant_id.clone(),
                resource: QuotaResourceKind::Stream,
            })
    }

    pub fn try_acquire_db(&self, tenant_id: &TenantId) -> Result<OwnedSemaphorePermit, QuotaError> {
        let bucket = self.bucket_for(tenant_id);
        Arc::clone(&bucket.db_sem)
            .try_acquire_owned()
            .map_err(|_| QuotaError::Exceeded {
                tenant_id: tenant_id.clone(),
                resource: QuotaResourceKind::Database,
            })
    }

    /// Acquire a Wasm invocation permit and optional initial memory reservation
    /// **before** constructing `wasmtime::Store` or instantiating a component.
    pub fn try_acquire_wasm_invocation(
        &self,
        tenant_id: &TenantId,
        initial_memory_reservation: usize,
    ) -> Result<WasmInvocationPermit, QuotaError> {
        let bucket = self.bucket_for(tenant_id);
        let permit = Arc::clone(&bucket.wasm_instance_sem)
            .try_acquire_owned()
            .map_err(|_| QuotaError::Exceeded {
                tenant_id: tenant_id.clone(),
                resource: QuotaResourceKind::WasmInstance,
            })?;
        bucket.wasm_memory.try_reserve(initial_memory_reservation)?;
        Ok(WasmInvocationPermit {
            _instance_permit: permit,
            memory_tracker: Arc::clone(&bucket.wasm_memory),
            initial_reserved_bytes: initial_memory_reservation,
        })
    }

    /// Acquire a reserved control-plane permit for placement lease heartbeats.
    /// Draws from `control_plane_sem` only, so data-plane saturation can never
    /// starve lease renewals.
    pub fn try_acquire_heartbeat(
        &self,
        tenant_id: &TenantId,
    ) -> Result<OwnedSemaphorePermit, QuotaError> {
        let bucket = self.bucket_for(tenant_id);
        Arc::clone(&bucket.control_plane_sem)
            .try_acquire_owned()
            .map_err(|_| QuotaError::Exceeded {
                tenant_id: tenant_id.clone(),
                resource: QuotaResourceKind::ControlPlaneHeartbeat,
            })
    }

    /// Acquire a reserved control-plane permit for stream/task cancellation.
    /// Draws from `control_plane_sem` only, so data-plane saturation can never
    /// delay cancellation.
    pub fn try_acquire_cancellation(
        &self,
        tenant_id: &TenantId,
    ) -> Result<OwnedSemaphorePermit, QuotaError> {
        let bucket = self.bucket_for(tenant_id);
        Arc::clone(&bucket.control_plane_sem)
            .try_acquire_owned()
            .map_err(|_| QuotaError::Exceeded {
                tenant_id: tenant_id.clone(),
                resource: QuotaResourceKind::ControlPlaneCancellation,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tight_quotas() -> TenantResourceQuotas {
        TenantResourceQuotas {
            max_mailbox_permits: 1,
            max_concurrent_tasks: 1,
            max_outstanding_streams: 1,
            max_wasm_instances: 1,
            max_wasm_memory_bytes: 128 * 1024,
            max_wasm_fuel_per_invocation: 1_000_000,
            max_wasm_wall_clock_ms: 5_000,
            max_db_concurrency: 1,
            reserved_control_plane_permits: 2,
        }
    }

    #[test]
    fn noisy_neighbor_saturation_does_not_affect_sibling_or_control_plane() {
        let controller = TenantResourceController::new(tight_quotas()).expect("valid quotas");
        let tenant_a = TenantId::parse("tenant-a.example").expect("valid");
        let tenant_b = TenantId::parse("tenant-b.example").expect("valid");

        // Saturate 100% of Tenant A's data-plane semaphores and memory.
        let _a_mb = controller
            .try_acquire_mailbox(&tenant_a)
            .expect("a mailbox");
        let _a_task = controller.try_acquire_task(&tenant_a).expect("a task");
        let _a_stream = controller.try_acquire_stream(&tenant_a).expect("a stream");
        let _a_db = controller.try_acquire_db(&tenant_a).expect("a db");
        let _a_wasm = controller
            .try_acquire_wasm_invocation(&tenant_a, 128 * 1024)
            .expect("a wasm");

        assert!(controller.try_acquire_mailbox(&tenant_a).is_err());
        assert!(controller.try_acquire_task(&tenant_a).is_err());
        assert!(controller.try_acquire_stream(&tenant_a).is_err());
        assert!(controller.try_acquire_db(&tenant_a).is_err());
        assert!(controller
            .try_acquire_wasm_invocation(&tenant_a, 0)
            .is_err());

        // Control-plane heartbeats & cancellation for Tenant A MUST still succeed!
        let _a_hb = controller
            .try_acquire_heartbeat(&tenant_a)
            .expect("reserved heartbeat permit must succeed under saturation");
        let _a_cancel = controller
            .try_acquire_cancellation(&tenant_a)
            .expect("reserved cancel permit must succeed under saturation");

        // Sibling Tenant B MUST be completely unaffected.
        let _b_mb = controller
            .try_acquire_mailbox(&tenant_b)
            .expect("b mailbox");
        let _b_task = controller.try_acquire_task(&tenant_b).expect("b task");
        let _b_stream = controller.try_acquire_stream(&tenant_b).expect("b stream");
        let _b_db = controller.try_acquire_db(&tenant_b).expect("b db");
        let _b_wasm = controller
            .try_acquire_wasm_invocation(&tenant_b, 64 * 1024)
            .expect("b wasm");
    }

    #[test]
    fn rejects_insufficient_control_plane_reserve() {
        let mut quotas = tight_quotas();
        quotas.reserved_control_plane_permits = 1;
        assert!(TenantResourceController::new(quotas).is_err());
    }
}
