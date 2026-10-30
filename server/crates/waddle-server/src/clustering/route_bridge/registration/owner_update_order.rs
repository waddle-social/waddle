//! Owner-side arrival-order fence for `remote_resource_update` handlers
//! (#1685 interim narrowing).
//!
//! The relay actor delegates every update ask into its own task, so two
//! updates for one registration used to run concurrently and could commit
//! in either order. Each update is admitted here synchronously in the relay
//! actor's turn — i.e. in the order the relay mailbox delivers them — and
//! its handler waits until every earlier-admitted handler for the same
//! exact registration has finished. Different registrations never wait for
//! each other.
//!
//! This orders handlers by *arrival at the owner* only. The update carries
//! no revision, so asks reordered in flight, and a sender-side timeout
//! (`MaybeCommitted`) whose update still commits after a later one, are not
//! fenced here; #1685 stays open for that.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::oneshot;

use super::super::{RemoteResourceRegistrationId, RemoteResourceSocketGeneration};

/// Distinct registrations that may hold admitted updates at once. A new
/// registration beyond this bound is refused (see [`RemoteResourceUpdateOrderFull`]).
pub(in super::super) const MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS: usize = 4096;

/// Admitted (running or waiting) updates per registration. One origin
/// socket produces these: live flips plus at most one resync pass, which
/// sends its snapshot one ask at a time.
pub(in super::super) const MAX_PENDING_REMOTE_RESOURCE_UPDATES_PER_REGISTRATION: usize = 32;

/// Exact registration identity an update is addressed to. The full JID
/// alone would serialize a replacement registration behind its stale
/// predecessor; the triple is what the owner's currency check compares.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in super::super) struct RemoteResourceUpdateKey {
    pub(in super::super) jid: jid::FullJid,
    pub(in super::super) registration_id: RemoteResourceRegistrationId,
    pub(in super::super) socket_generation: RemoteResourceSocketGeneration,
}

/// Why an update was refused admission. Full policy: the caller answers
/// `Unavailable` without applying anything, which every sender path
/// already treats as "no effect, resync the full snapshot".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum RemoteResourceUpdateOrderFull {
    /// [`MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS`] distinct
    /// registrations already have admitted updates.
    Registrations,
    /// This registration already has
    /// [`MAX_PENDING_REMOTE_RESOURCE_UPDATES_PER_REGISTRATION`] admitted
    /// updates.
    RegistrationQueue,
}

struct Slot {
    /// Completion signal of the most recently admitted turn: its sender is
    /// dropped when that turn ends, however it ends.
    tail: oneshot::Receiver<()>,
    admitted: usize,
}

type Slots = Arc<Mutex<HashMap<RemoteResourceUpdateKey, Slot>>>;

/// Bounded per-registration FIFO of update handlers. A slot exists only
/// while its registration has at least one admitted turn; the last turn to
/// end removes it, so the map never retains idle registrations.
#[derive(Default)]
pub(in super::super) struct RemoteResourceUpdateOrder {
    slots: Slots,
}

impl RemoteResourceUpdateOrder {
    /// Admit one update. Synchronous by design: the caller invokes it in
    /// the relay actor's turn, which fixes the position at arrival. The
    /// lock is a plain mutex held only for this map edit, never across an
    /// await, so unrelated registrations do not serialize on it.
    pub(in super::super) fn admit(
        &self,
        key: RemoteResourceUpdateKey,
    ) -> Result<RemoteResourceUpdateTurn, RemoteResourceUpdateOrderFull> {
        let (release, tail) = oneshot::channel();
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let predecessor = match slots.get_mut(&key) {
            Some(slot) => {
                if slot.admitted >= MAX_PENDING_REMOTE_RESOURCE_UPDATES_PER_REGISTRATION {
                    return Err(RemoteResourceUpdateOrderFull::RegistrationQueue);
                }
                slot.admitted += 1;
                Some(std::mem::replace(&mut slot.tail, tail))
            }
            None => {
                if slots.len() >= MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS {
                    return Err(RemoteResourceUpdateOrderFull::Registrations);
                }
                slots.insert(key.clone(), Slot { tail, admitted: 1 });
                None
            }
        };
        drop(slots);
        Ok(RemoteResourceUpdateTurn {
            slots: Arc::clone(&self.slots),
            key,
            predecessor,
            _release: release,
        })
    }

    #[cfg(test)]
    pub(in super::super) fn tracked_registrations(&self) -> usize {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// One admitted update. Hold it for the whole handler; dropping it (on
/// completion, cancellation or panic) releases the next update for the
/// same registration.
pub(in super::super) struct RemoteResourceUpdateTurn {
    slots: Slots,
    key: RemoteResourceUpdateKey,
    predecessor: Option<oneshot::Receiver<()>>,
    _release: oneshot::Sender<()>,
}

impl RemoteResourceUpdateTurn {
    /// Wait until every earlier-admitted update for this registration has
    /// ended. Never waits on a later turn, so the wait graph is acyclic;
    /// each predecessor's handler is bounded by its own ask timeouts.
    pub(in super::super) async fn wait_for_predecessors(&mut self) {
        if let Some(predecessor) = self.predecessor.take() {
            // The sender is never used to send: `Err` means the
            // predecessor turn ended, which is the only signal.
            let _ = predecessor.await;
        }
    }
}

impl Drop for RemoteResourceUpdateTurn {
    fn drop(&mut self) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let idle = match slots.get_mut(&self.key) {
            Some(slot) => {
                slot.admitted = slot.admitted.saturating_sub(1);
                slot.admitted == 0
            }
            None => false,
        };
        if idle {
            slots.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn key(jid: &str, registration_id: RemoteResourceRegistrationId) -> RemoteResourceUpdateKey {
        RemoteResourceUpdateKey {
            jid: jid.parse().expect("valid full jid"),
            registration_id,
            socket_generation: RemoteResourceSocketGeneration::next(None),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn same_registration_handlers_finish_in_arrival_order_when_the_first_is_delayed() {
        let order = RemoteResourceUpdateOrder::default();
        let registration = key(
            "juliet@example.test/phone",
            RemoteResourceRegistrationId::fresh(),
        );
        let first = order.admit(registration.clone()).expect("admit first");
        let second = order.admit(registration).expect("admit second");
        let state = Arc::new(Mutex::new(Vec::new()));
        let (gate_tx, gate_rx) = oneshot::channel::<()>();

        // The second-arrived handler is scheduled first and is not delayed.
        let mut second_task = tokio::spawn({
            let state = Arc::clone(&state);
            async move {
                let mut turn = second;
                turn.wait_for_predecessors().await;
                state.lock().expect("state").push("B");
            }
        });
        // The first-arrived handler is delayed mid-handler.
        let first_task = tokio::spawn({
            let state = Arc::clone(&state);
            async move {
                let mut turn = first;
                turn.wait_for_predecessors().await;
                let _ = gate_rx.await;
                state.lock().expect("state").push("A");
            }
        });

        assert!(
            tokio::time::timeout(Duration::from_secs(1), &mut second_task)
                .await
                .is_err(),
            "the later update must not run while the earlier one is in progress"
        );
        assert!(state.lock().expect("state").is_empty());

        gate_tx.send(()).expect("release first handler");
        first_task.await.expect("first handler");
        second_task.await.expect("second handler");
        assert_eq!(*state.lock().expect("state"), vec!["A", "B"]);
        assert_eq!(
            state.lock().expect("state").last(),
            Some(&"B"),
            "final state is the last-arrived update"
        );
        assert_eq!(order.tracked_registrations(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn different_registrations_do_not_wait_for_each_other() {
        let order = RemoteResourceUpdateOrder::default();
        let held = order
            .admit(key(
                "juliet@example.test/phone",
                RemoteResourceRegistrationId::fresh(),
            ))
            .expect("admit held");
        // Same full JID, different registration: a distinct key.
        let mut replacement = order
            .admit(key(
                "juliet@example.test/phone",
                RemoteResourceRegistrationId::fresh(),
            ))
            .expect("admit replacement registration");
        let mut other = order
            .admit(key(
                "romeo@example.test/laptop",
                RemoteResourceRegistrationId::fresh(),
            ))
            .expect("admit other registration");
        // Same JID and registration id, newer socket generation: distinct.
        let shared_id = RemoteResourceRegistrationId::fresh();
        let older_generation = order
            .admit(key("nurse@example.test/tablet", shared_id))
            .expect("admit older generation");
        let mut newer_generation = order
            .admit(RemoteResourceUpdateKey {
                socket_generation: RemoteResourceSocketGeneration::next(Some(
                    RemoteResourceSocketGeneration::next(None),
                )),
                ..key("nurse@example.test/tablet", shared_id)
            })
            .expect("admit newer generation");

        tokio::time::timeout(Duration::from_secs(1), replacement.wait_for_predecessors())
            .await
            .expect("a replacement registration runs while the old one is held");
        tokio::time::timeout(Duration::from_secs(1), other.wait_for_predecessors())
            .await
            .expect("another JID's registration runs while the first is held");
        tokio::time::timeout(
            Duration::from_secs(1),
            newer_generation.wait_for_predecessors(),
        )
        .await
        .expect("a newer socket generation runs while the older one is held");
        assert_eq!(order.tracked_registrations(), 5);
        drop((held, replacement, other, older_generation, newer_generation));
        assert_eq!(order.tracked_registrations(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn an_abandoned_predecessor_releases_its_successor() {
        let order = RemoteResourceUpdateOrder::default();
        let registration = key(
            "juliet@example.test/phone",
            RemoteResourceRegistrationId::fresh(),
        );
        let first = order.admit(registration.clone()).expect("admit first");
        let mut second = order.admit(registration).expect("admit second");
        // A handler cancelled or panicking before it ran drops its turn.
        drop(first);
        tokio::time::timeout(Duration::from_secs(1), second.wait_for_predecessors())
            .await
            .expect("successor proceeds once the predecessor turn is dropped");
        drop(second);
        assert_eq!(order.tracked_registrations(), 0);
    }

    #[test]
    fn admission_is_bounded_per_registration_and_across_registrations() {
        let order = RemoteResourceUpdateOrder::default();
        let registration = key(
            "juliet@example.test/phone",
            RemoteResourceRegistrationId::fresh(),
        );
        let mut queued = Vec::new();
        for _ in 0..MAX_PENDING_REMOTE_RESOURCE_UPDATES_PER_REGISTRATION {
            queued.push(
                order
                    .admit(registration.clone())
                    .expect("within queue bound"),
            );
        }
        assert_eq!(
            order.admit(registration.clone()).err(),
            Some(RemoteResourceUpdateOrderFull::RegistrationQueue)
        );
        queued.pop();
        queued.push(
            order
                .admit(registration.clone())
                .expect("a freed queue position is reusable"),
        );

        let mut others = Vec::new();
        for index in 1..MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS {
            others.push(
                order
                    .admit(key(
                        &format!("user{index}@example.test/r"),
                        RemoteResourceRegistrationId::fresh(),
                    ))
                    .expect("within registration bound"),
            );
        }
        assert_eq!(
            order.tracked_registrations(),
            MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS
        );
        assert_eq!(
            order
                .admit(key(
                    "late@example.test/r",
                    RemoteResourceRegistrationId::fresh()
                ))
                .err(),
            Some(RemoteResourceUpdateOrderFull::Registrations)
        );

        drop(others);
        drop(queued);
        assert_eq!(
            order.tracked_registrations(),
            0,
            "idle registrations are removed, not retained"
        );
        order
            .admit(key(
                "late@example.test/r",
                RemoteResourceRegistrationId::fresh(),
            ))
            .expect("capacity returns once registrations go idle");
    }
}
