//! Short-tick `BrokerReplicaFence` modeled on `permissions/host_grant/fence.rs`.
//!
//! Uses [`BootClock`] (`CLOCK_BOOTTIME` on Linux) so host suspend counts
//! against the lease. Before acknowledging any advanced fence generation to the
//! store, the replica cancels every local in-flight stream bound to an older
//! generation of that fence.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::watch;
use uuid::Uuid;
use waddle_broker_core::admission::{AdmissionError, AttemptId, FenceKey};
use waddle_broker_core::boot_clock::{BootClock, BootInstant};
use waddle_broker_core::placement::FenceGeneration;

use crate::admission::{FenceAcknowledgement, ReplicaFenceStore};

pub const BROKER_FENCE_LEASE_TTL: Duration = Duration::from_secs(5);
pub const BROKER_FENCE_LEASE_MARGIN: Duration = Duration::from_secs(1);
pub const BROKER_FENCE_EXPIRY_GRACE: Duration = Duration::from_secs(1);
pub const BROKER_FENCE_TICK: Duration = Duration::from_millis(250);
pub const BROKER_FENCE_ACK_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
struct ActiveStreamEntry {
    fences: [(FenceKey, FenceGeneration); 3],
    cancel_tx: watch::Sender<bool>,
}

#[derive(Debug, Default)]
struct ReplicaFenceState {
    registration: u64,
    valid_until: Option<BootInstant>,
    installed_generations: HashMap<FenceKey, FenceGeneration>,
    active_streams: HashMap<AttemptId, ActiveStreamEntry>,
}

/// Ticket bound to a specific replica lease registration and three fence snapshots.
#[derive(Debug, Clone)]
pub struct FenceTicket {
    registration: u64,
    fences: [(FenceKey, FenceGeneration); 3],
}

/// Per-replica short-tick revocation fence with local in-flight stream cancellation.
pub struct BrokerReplicaFence {
    replica_id: Uuid,
    store: Arc<dyn ReplicaFenceStore>,
    clock: Arc<dyn BootClock>,
    lease_ttl: Duration,
    lease_margin: Duration,
    expiry_grace: Duration,
    state: RwLock<ReplicaFenceState>,
}

impl BrokerReplicaFence {
    pub fn new(
        replica_id: Uuid,
        store: Arc<dyn ReplicaFenceStore>,
        clock: Arc<dyn BootClock>,
    ) -> Self {
        Self {
            replica_id,
            store,
            clock,
            lease_ttl: BROKER_FENCE_LEASE_TTL,
            lease_margin: BROKER_FENCE_LEASE_MARGIN,
            expiry_grace: BROKER_FENCE_EXPIRY_GRACE,
            state: RwLock::new(ReplicaFenceState::default()),
        }
    }

    pub fn replica_id(&self) -> Uuid {
        self.replica_id
    }

    /// Executes one deterministic fence heartbeat tick:
    /// 1. Reads `BootClock` (if unreadable, lapses and cancels all local streams).
    /// 2. Registers or renews the replica lease in `ReplicaFenceStore` and fetches
    ///    all current fence generations.
    /// 3. Installs any advanced generations in memory and cancels every local
    ///    in-flight stream bound to an older generation.
    /// 4. ONLY AFTER local streams are canceled, persists acknowledgements in the store.
    ///
    /// Returns `(canceled_attempt_ids, newly_acked_fences)`.
    pub async fn tick_once(
        &self,
    ) -> Result<(Vec<AttemptId>, Vec<(FenceKey, FenceGeneration)>), AdmissionError> {
        let Some(sent_at) = self.clock.now() else {
            self.lapse_and_cancel_all();
            return Err(AdmissionError::HolderUnauthorized);
        };

        let ttl_ms = i64::try_from(self.lease_ttl.as_millis()).unwrap_or(5_000);
        let needs_register = {
            let guard = self
                .state
                .read()
                .map_err(|_| AdmissionError::Backend("replica fence lock poisoned".to_owned()))?;
            guard.registration == 0 || guard.valid_until.is_none_or(|until| sent_at >= until)
        };

        let fetched = if needs_register {
            self.store.register_replica(self.replica_id, ttl_ms).await?
        } else {
            match self.store.renew_replica(self.replica_id, ttl_ms).await? {
                Some(fences) => fences,
                None => self.store.register_replica(self.replica_id, ttl_ms).await?,
            }
        };

        let valid_window = self.lease_ttl.saturating_sub(self.lease_margin);
        let new_valid_until = sent_at.checked_add(valid_window);

        let (canceled, acks) = {
            let mut guard = self
                .state
                .write()
                .map_err(|_| AdmissionError::Backend("replica fence lock poisoned".to_owned()))?;
            if needs_register {
                guard.registration = guard.registration.saturating_add(1);
            }
            guard.valid_until = new_valid_until;

            let mut acks = Vec::new();
            for (key, gen) in fetched {
                let prev = guard.installed_generations.get(&key).copied();
                if prev.is_none_or(|p| gen > p) {
                    guard.installed_generations.insert(key.clone(), gen);
                    acks.push((key, gen));
                }
            }

            let mut canceled = Vec::new();
            let installed = guard.installed_generations.clone();
            guard.active_streams.retain(|attempt_id, entry| {
                let stale = entry.fences.iter().any(|(fkey, bound_gen)| {
                    installed
                        .get(fkey)
                        .is_some_and(|current_gen| *current_gen > *bound_gen)
                });
                if stale {
                    let _ = entry.cancel_tx.send(true);
                    canceled.push(*attempt_id);
                    false
                } else {
                    true
                }
            });
            (canceled, acks)
        };

        if !acks.is_empty() {
            self.store
                .ack_replica_fences(self.replica_id, &acks)
                .await?;
        }
        Ok((canceled, acks))
    }

    /// Lapses this replica's lease and cancels all locally tracked in-flight streams.
    pub fn lapse_and_cancel_all(&self) {
        if let Ok(mut guard) = self.state.write() {
            guard.valid_until = None;
            for (_, entry) in guard.active_streams.drain() {
                let _ = entry.cancel_tx.send(true);
            }
        }
    }

    /// Issues a [`FenceTicket`] if this replica currently holds a valid non-expired
    /// lease and none of the three fences have already advanced past `fences`.
    pub fn issue_ticket(
        &self,
        fences: [(FenceKey, FenceGeneration); 3],
    ) -> Result<FenceTicket, AdmissionError> {
        let Some(now) = self.clock.now() else {
            self.lapse_and_cancel_all();
            return Err(AdmissionError::HolderUnauthorized);
        };
        let guard = self
            .state
            .read()
            .map_err(|_| AdmissionError::Backend("replica fence lock poisoned".to_owned()))?;
        let Some(valid_until) = guard.valid_until else {
            return Err(AdmissionError::HolderUnauthorized);
        };
        if now >= valid_until {
            return Err(AdmissionError::HolderUnauthorized);
        }
        for (key, gen) in &fences {
            if let Some(installed) = guard.installed_generations.get(key) {
                if *installed > *gen {
                    return Err(AdmissionError::FenceStaleOrClosed(key.kind.clone()));
                }
            }
        }
        Ok(FenceTicket {
            registration: guard.registration,
            fences,
        })
    }

    /// Checks whether `ticket` is still valid on this replica right before dispatch.
    pub fn is_ticket_current(&self, ticket: &FenceTicket) -> bool {
        let Some(now) = self.clock.now() else {
            return false;
        };
        let Ok(guard) = self.state.read() else {
            return false;
        };
        if guard.registration != ticket.registration {
            return false;
        }
        let Some(valid_until) = guard.valid_until else {
            return false;
        };
        if now >= valid_until {
            return false;
        }
        for (key, gen) in &ticket.fences {
            if let Some(installed) = guard.installed_generations.get(key) {
                if *installed > *gen {
                    return false;
                }
            }
        }
        true
    }

    /// Registers an in-flight stream bound to three fence generations, returning
    /// a `watch::Receiver<bool>` that flips to `true` when any bound fence advances
    /// or the replica lease lapses.
    pub fn register_stream(
        &self,
        attempt_id: AttemptId,
        fences: [(FenceKey, FenceGeneration); 3],
    ) -> Result<watch::Receiver<bool>, AdmissionError> {
        let (tx, rx) = watch::channel(false);
        let mut guard = self
            .state
            .write()
            .map_err(|_| AdmissionError::Backend("replica fence lock poisoned".to_owned()))?;
        for (key, gen) in &fences {
            if let Some(installed) = guard.installed_generations.get(key) {
                if *installed > *gen {
                    let _ = tx.send(true);
                    return Err(AdmissionError::FenceStaleOrClosed(key.kind.clone()));
                }
            }
        }
        guard.active_streams.insert(
            attempt_id,
            ActiveStreamEntry {
                fences,
                cancel_tx: tx,
            },
        );
        Ok(rx)
    }

    pub fn unregister_stream(&self, attempt_id: AttemptId) {
        if let Ok(mut guard) = self.state.write() {
            guard.active_streams.remove(&attempt_id);
        }
    }

    pub fn active_stream_count(&self) -> usize {
        self.state
            .read()
            .map(|g| g.active_streams.len())
            .unwrap_or(0)
    }

    /// Polls `ReplicaFenceStore::check_revocation_ack` until every active peer
    /// replica has acknowledged `>= target_gen` or `timeout` elapses.
    pub async fn await_revocation_fence(
        &self,
        key: &FenceKey,
        target_gen: FenceGeneration,
        timeout: Duration,
    ) -> Result<(), AdmissionError> {
        let grace_ms = i64::try_from(self.expiry_grace.as_millis()).unwrap_or(1_000);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self
                .store
                .check_revocation_ack(key, target_gen, grace_ms)
                .await?
            {
                FenceAcknowledgement::Acknowledged => return Ok(()),
                FenceAcknowledgement::Pending { .. } => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(AdmissionError::Backend(
                            "revocation fence acknowledgement timed out".to_owned(),
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    }

    pub async fn release(&self) -> Result<(), AdmissionError> {
        self.lapse_and_cancel_all();
        self.store.release_replica(self.replica_id).await
    }
}
