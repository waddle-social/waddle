use super::{ConnectionEntry, ConnectionRegistry};
use dashmap::DashMap;
use jid::FullJid;
use std::collections::HashSet;
#[cfg(feature = "test-utils")]
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{watch, Mutex, OwnedMutexGuard};
use waddle_xmpp_core::OccupancySessionGeneration;

/// Completion of the owning socket task, independent of routing publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketCleanupState {
    Running,
    Detached,
    Retired,
    CleanupPending,
}

/// Nonblocking observation for remote cleanup, which can be requested while a
/// successor holds the bind gate and waits for its remote mirror registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketLifecycleProbe {
    Found(SocketCleanupState),
    Absent,
    Busy,
}

/// Remains discoverable while the socket has removed its route but is still
/// completing generation-scoped cleanup. Dropping a task is not completion.
pub struct SocketLifecycle {
    pub entry: ConnectionEntry,
    pub generation: OccupancySessionGeneration,
    completion: watch::Sender<SocketCleanupState>,
}

impl SocketLifecycle {
    pub fn finish(&self, outcome: SocketCleanupState) {
        self.completion.send_replace(outcome);
    }

    pub fn state(&self) -> SocketCleanupState {
        *self.completion.borrow()
    }

    pub async fn wait_stopped(&self) -> SocketCleanupState {
        let mut receiver = self.completion.subscribe();
        // This record itself owns the sender, so it cannot close while awaited.
        let _ = receiver
            .wait_for(|state| *state != SocketCleanupState::Running)
            .await;
        self.state()
    }
}

#[derive(Default)]
pub(super) struct BindState {
    incumbent: Option<Arc<SocketLifecycle>>,
    retirements: HashSet<OccupancySessionGeneration>,
    actor_retirements: Vec<Arc<SocketLifecycle>>,
}
type BindSlot = Mutex<BindState>;
pub(super) type BindSlots = DashMap<FullJid, Arc<BindSlot>>;

pub struct ConnectionBindGuard {
    slots: Arc<BindSlots>,
    jid: FullJid,
    guard: Option<OwnedMutexGuard<BindState>>,
}

impl ConnectionBindGuard {
    pub fn incumbent(&self) -> Option<Arc<SocketLifecycle>> {
        self.guard
            .as_ref()
            .and_then(|guard| guard.incumbent.clone())
    }

    pub fn retain_retirement(&mut self, generation: OccupancySessionGeneration) {
        if let Some(guard) = self.guard.as_mut() {
            guard.retirements.insert(generation);
        }
    }

    pub fn pending_retirements(&self) -> Vec<OccupancySessionGeneration> {
        self.guard
            .as_ref()
            .map(|guard| guard.retirements.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn complete_retirement(&mut self, generation: OccupancySessionGeneration) {
        if let Some(guard) = self.guard.as_mut() {
            guard.retirements.remove(&generation);
        }
    }

    /// Actor cleanup is separate from occupancy cleanup: a same-generation
    /// resume must retain its room membership while retiring the old token.
    pub fn retain_actor_retirement(&mut self, lifecycle: Arc<SocketLifecycle>) {
        if let Some(guard) = self.guard.as_mut() {
            let owner = lifecycle.entry.binding_id();
            if !guard
                .actor_retirements
                .iter()
                .any(|pending| pending.entry.binding_id() == owner)
            {
                guard.actor_retirements.push(lifecycle);
            }
        }
    }

    pub fn pending_actor_retirements(&self) -> Vec<Arc<SocketLifecycle>> {
        self.guard
            .as_ref()
            .map(|guard| guard.actor_retirements.clone())
            .unwrap_or_default()
    }

    pub fn complete_actor_retirement(&mut self, lifecycle: &Arc<SocketLifecycle>) {
        if let Some(guard) = self.guard.as_mut() {
            let owner = lifecycle.entry.binding_id();
            guard
                .actor_retirements
                .retain(|pending| pending.entry.binding_id() != owner);
        }
    }

    /// A confirmed local actor registration atomically supersedes every old
    /// token for this full JID. Failed or cancelled registration cannot clear
    /// this inventory, even if it already published a new socket lifecycle.
    pub fn complete_actor_retirements_after_registration(&mut self) {
        if let Some(guard) = self.guard.as_mut() {
            guard.actor_retirements.clear();
        }
    }

    pub fn publish(
        &mut self,
        entry: ConnectionEntry,
        generation: OccupancySessionGeneration,
    ) -> Arc<SocketLifecycle> {
        let (completion, _) = watch::channel(SocketCleanupState::Running);
        let lifecycle = Arc::new(SocketLifecycle {
            entry,
            generation,
            completion,
        });
        if let Some(guard) = self.guard.as_mut() {
            guard.incumbent = Some(lifecycle.clone());
        }
        lifecycle
    }
}

impl Drop for ConnectionBindGuard {
    fn drop(&mut self) {
        drop(self.guard.take());
        prune(&self.slots, &self.jid);
    }
}

fn prune(slots: &BindSlots, jid: &FullJid) {
    // Test and removal share the map shard lock with bind-slot acquisition.
    // A waiter holds another Arc, and failed retirement remains discoverable.
    slots.remove_if(jid, |_, slot| {
        Arc::strong_count(slot) == 1
            && slot.try_lock().is_ok_and(|current| {
                current.retirements.is_empty()
                    && current.actor_retirements.is_empty()
                    && current.incumbent.as_ref().is_none_or(|lifecycle| {
                        matches!(
                            lifecycle.state(),
                            SocketCleanupState::Detached | SocketCleanupState::Retired
                        )
                    })
            })
    });
}

impl ConnectionRegistry {
    /// Observe actual contention on the next bind lock for this JID. The
    /// signal is sent only after polling its lock future returns `Pending`.
    #[cfg(feature = "test-utils")]
    pub fn observe_next_pending_bind(&self, jid: &FullJid) -> tokio::sync::oneshot::Receiver<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        assert!(
            self.bind_pending_probes
                .insert(jid.clone(), sender)
                .is_none(),
            "a pending-bind observer is already armed for {jid}"
        );
        receiver
    }

    /// Never wait for a local binder while holding a remote registration or
    /// durable generation lock: that binder may be waiting for those locks.
    pub fn try_lock_bind(&self, jid: &FullJid) -> Option<ConnectionBindGuard> {
        let slot = self
            .bind_slots
            .entry(jid.clone())
            .or_insert_with(|| Arc::new(Mutex::new(BindState::default())))
            .clone();
        let guard = slot.try_lock_owned().ok()?;
        Some(ConnectionBindGuard {
            slots: self.bind_slots.clone(),
            jid: jid.clone(),
            guard: Some(guard),
        })
    }

    pub fn probe_socket_lifecycle(
        &self,
        jid: &FullJid,
        generation: OccupancySessionGeneration,
    ) -> SocketLifecycleProbe {
        let Some(slot) = self.bind_slots.get(jid) else {
            return SocketLifecycleProbe::Absent;
        };
        let probe = match slot.try_lock() {
            Ok(state) => state
                .incumbent
                .as_ref()
                .filter(|incumbent| incumbent.generation == generation)
                .map_or(SocketLifecycleProbe::Absent, |incumbent| {
                    SocketLifecycleProbe::Found(incumbent.state())
                }),
            Err(_) => SocketLifecycleProbe::Busy,
        };
        probe
    }

    /// Serialize socket admission without holding a routing-map guard across
    /// awaits. Socket cleanup never acquires this mutex.
    pub async fn lock_bind(&self, jid: &FullJid) -> ConnectionBindGuard {
        let slot = self
            .bind_slots
            .entry(jid.clone())
            .or_insert_with(|| Arc::new(Mutex::new(BindState::default())))
            .clone();
        #[cfg(feature = "test-utils")]
        let guard = {
            let mut lock = std::pin::pin!(slot.lock_owned());
            std::future::poll_fn(|cx| {
                let result = lock.as_mut().poll(cx);
                if result.is_pending() {
                    if let Some((_, sender)) = self.bind_pending_probes.remove(jid) {
                        let _ = sender.send(());
                    }
                }
                result
            })
            .await
        };
        #[cfg(not(feature = "test-utils"))]
        let guard = slot.lock_owned().await;
        ConnectionBindGuard {
            slots: self.bind_slots.clone(),
            jid: jid.clone(),
            guard: Some(guard),
        }
    }

    pub fn prune_completed_bind(&self, jid: &FullJid) {
        prune(&self.bind_slots, jid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "test-utils")]
    #[tokio::test]
    async fn bind_contention_probe_requires_a_pending_lock_poll() {
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "alice@example.com/phone".parse().expect("JID");
        let mut pending = registry.observe_next_pending_bind(&jid);
        let first = registry.lock_bind(&jid).await;
        assert!(matches!(
            pending.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        let mut second = std::pin::pin!(registry.lock_bind(&jid));
        assert!(futures::poll!(second.as_mut()).is_pending());
        pending.try_recv().expect("actual contention was observed");
        drop(first);
        let _second = second.await;
    }

    #[tokio::test]
    async fn failed_successor_publication_cannot_discard_prior_actor_retirement() {
        let registry = ConnectionRegistry::new();
        let jid: FullJid = "alice@example.com/retirement".parse().expect("JID");
        let mut guard = registry.lock_bind(&jid).await;
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let old = guard.publish(ConnectionEntry::new(tx), OccupancySessionGeneration::mint());
        old.finish(SocketCleanupState::Detached);
        guard.retain_actor_retirement(old.clone());
        guard.retain_actor_retirement(old.clone());
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let successor = guard.publish(ConnectionEntry::new(tx), OccupancySessionGeneration::mint());
        successor.finish(SocketCleanupState::Retired);
        // The successor's actor mirror failed. Its lifecycle is complete but
        // the old exact token still needs cleanup, even after the slot drops.
        drop(guard);
        let mut guard = registry.lock_bind(&jid).await;
        let pending = guard.pending_actor_retirements();
        assert_eq!(pending.len(), 1, "retries deduplicate the exact owner");
        assert!(Arc::ptr_eq(&pending[0], &old));
        assert!(
            guard.pending_retirements().is_empty(),
            "same-generation actor retirement is not a room cleanup obligation"
        );
        guard.complete_actor_retirement(&old);
        drop(guard);
        let guard = registry.lock_bind(&jid).await;
        assert!(
            guard.incumbent().is_none(),
            "only confirmed actor cleanup makes the completed slot prunable"
        );
    }
}
