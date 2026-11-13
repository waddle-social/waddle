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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

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

/// Why an update was refused admission. The caller answers `Unavailable`
/// without applying anything, which every sender path already treats as
/// "no effect, resync the full snapshot".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum RemoteResourceUpdateOrderFull {
    /// [`MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS`] distinct
    /// registrations already have admitted updates.
    Registrations,
    /// This registration already has
    /// [`MAX_PENDING_REMOTE_RESOURCE_UPDATES_PER_REGISTRATION`] admitted
    /// updates.
    RegistrationQueue,
    /// A panic during a map edit poisoned the order lock. All later
    /// admissions fail closed instead of relying on state of unknown shape;
    /// the poisoned map is never read again.
    Poisoned,
}

/// The order lock was poisoned and the turn was not granted readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) struct RemoteResourceUpdateOrderClosed;

struct QueuedTurn {
    // Both the queue entry and its live turn retain this allocation, so an
    // identity cannot be reused while a turn can still try to remove it.
    identity: Arc<()>,
    readiness: Option<oneshot::Sender<()>>,
}

impl QueuedTurn {
    fn grant(&mut self) {
        if let Some(readiness) = self.readiness.take() {
            let _ = readiness.send(());
        }
    }
}

type Slots = Arc<Mutex<HashMap<RemoteResourceUpdateKey, VecDeque<QueuedTurn>>>>;

/// Bounded per-registration FIFO of update handlers. Each queue contains
/// every live admitted turn, including its active head. An idle registration
/// is removed immediately so its capacity can be reused.
///
/// A poisoned order lock closes the order for good. The queued readiness
/// senders live inside the poisoned map, which is not known to be consistent
/// and is therefore left untouched; `closed` is the signal outside the lock
/// that fails every turn still waiting for a grant.
#[derive(Default)]
pub(in super::super) struct RemoteResourceUpdateOrder {
    slots: Slots,
    closed: CancellationToken,
}

impl RemoteResourceUpdateOrder {
    /// Admit one update synchronously in the relay actor's turn. The mutex
    /// is held only for a queue edit, never across an await.
    pub(in super::super) fn admit(
        &self,
        key: RemoteResourceUpdateKey,
    ) -> Result<RemoteResourceUpdateTurn, RemoteResourceUpdateOrderFull> {
        let Ok(mut slots) = self.slots.lock() else {
            // Fail every ungranted turn without granting successors. The
            // already-granted head of each queue may finish.
            self.closed.cancel();
            return Err(RemoteResourceUpdateOrderFull::Poisoned);
        };
        let (sender, receiver) = oneshot::channel();
        let identity = Arc::new(());
        let queued = QueuedTurn {
            identity: Arc::clone(&identity),
            readiness: Some(sender),
        };
        if let Some(queue) = slots.get_mut(&key) {
            if queue.len() >= MAX_PENDING_REMOTE_RESOURCE_UPDATES_PER_REGISTRATION {
                return Err(RemoteResourceUpdateOrderFull::RegistrationQueue);
            }
            queue.push_back(queued);
        } else {
            if slots.len() >= MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS {
                return Err(RemoteResourceUpdateOrderFull::Registrations);
            }
            let mut queue = VecDeque::new();
            queue.push_back(queued);
            if let Some(head) = queue.front_mut() {
                head.grant();
            }
            slots.insert(key.clone(), queue);
        }
        drop(slots);
        Ok(RemoteResourceUpdateTurn {
            slots: Arc::clone(&self.slots),
            closed: self.closed.clone(),
            key,
            identity,
            readiness: TurnReadiness::Waiting(receiver),
        })
    }

    #[cfg(test)]
    pub(in super::super) fn tracked_registrations(&self) -> usize {
        self.slots.lock().expect("order lock").len()
    }

    #[cfg(test)]
    pub(in super::super) fn poison_for_test(&self) {
        let slots = Arc::clone(&self.slots);
        let poisoner = std::thread::spawn(move || {
            let _guard = slots.lock().expect("order lock");
            panic!("poison the order lock");
        });
        assert!(poisoner.join().is_err());
        assert!(self.slots.is_poisoned());
    }
}

enum TurnReadiness {
    Waiting(oneshot::Receiver<()>),
    Ready,
    Closed,
}

/// One admitted update. Hold it for the whole handler. Only dropping the
/// head grants the next live turn; cancelling an interior turn merely
/// removes that turn from the queue.
pub(in super::super) struct RemoteResourceUpdateTurn {
    slots: Slots,
    closed: CancellationToken,
    key: RemoteResourceUpdateKey,
    identity: Arc<()>,
    readiness: TurnReadiness,
}

impl RemoteResourceUpdateTurn {
    /// Wait for an explicit grant from the queue. The receiver stays in the
    /// turn while this borrowed future is pending, so cancelling that future
    /// cannot bypass the queue on a later retry. A grant that was already
    /// sent wins over a closed order; otherwise a closed order or a closed
    /// sender is failure, never permission to run.
    pub(in super::super) async fn wait_for_predecessors(
        &mut self,
    ) -> Result<(), RemoteResourceUpdateOrderClosed> {
        let result = match &mut self.readiness {
            TurnReadiness::Waiting(receiver) => {
                tokio::select! {
                    biased;
                    granted = receiver => granted.map_err(|_| RemoteResourceUpdateOrderClosed),
                    () = self.closed.cancelled() => Err(RemoteResourceUpdateOrderClosed),
                }
            }
            TurnReadiness::Ready => return Ok(()),
            TurnReadiness::Closed => return Err(RemoteResourceUpdateOrderClosed),
        };
        self.readiness = if result.is_ok() {
            TurnReadiness::Ready
        } else {
            TurnReadiness::Closed
        };
        result
    }
}

impl Drop for RemoteResourceUpdateTurn {
    fn drop(&mut self) {
        let Ok(mut slots) = self.slots.lock() else {
            // Closing wakes ungranted waiters with an error. Poison is
            // observed only on the next admission or turn drop, so a
            // panic alone does not guarantee an immediate wakeup.
            self.closed.cancel();
            return;
        };
        let idle = if let Some(queue) = slots.get_mut(&self.key) {
            let Some(position) = queue
                .iter()
                .position(|queued| Arc::ptr_eq(&queued.identity, &self.identity))
            else {
                return;
            };
            drop(queue.remove(position));
            if let (0, Some(next)) = (position, queue.front_mut()) {
                next.grant();
            }
            queue.is_empty()
        } else {
            false
        };
        if idle {
            slots.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Poll;
    use std::time::Duration;

    fn key(jid: &str, registration_id: RemoteResourceRegistrationId) -> RemoteResourceUpdateKey {
        RemoteResourceUpdateKey {
            jid: jid.parse().expect("valid full jid"),
            registration_id,
            socket_generation: RemoteResourceSocketGeneration::next(None),
        }
    }

    fn registration() -> RemoteResourceUpdateKey {
        key(
            "juliet@example.test/phone",
            RemoteResourceRegistrationId::fresh(),
        )
    }

    #[tokio::test]
    async fn cancelling_an_unpolled_interior_turn_does_not_release_its_successor() {
        let order = RemoteResourceUpdateOrder::default();
        let key = registration();
        let mut first = order.admit(key.clone()).expect("admit first");
        let second = order.admit(key.clone()).expect("admit second");
        let mut third = order.admit(key).expect("admit third");
        first.wait_for_predecessors().await.expect("first is ready");

        drop(second);
        assert_eq!(
            futures::poll!(std::pin::pin!(third.wait_for_predecessors())),
            Poll::Pending
        );
        drop(first);
        third.wait_for_predecessors().await.expect("third is ready");
        drop(third);
        assert_eq!(order.tracked_registrations(), 0);
    }

    #[tokio::test]
    async fn cancelling_a_waiting_interior_turn_does_not_release_its_successor() {
        let order = RemoteResourceUpdateOrder::default();
        let key = registration();
        let first = order.admit(key.clone()).expect("admit first");
        let mut second = order.admit(key.clone()).expect("admit second");
        let mut third = order.admit(key).expect("admit third");

        assert_eq!(
            futures::poll!(std::pin::pin!(second.wait_for_predecessors())),
            Poll::Pending
        );
        drop(second);
        assert_eq!(
            futures::poll!(std::pin::pin!(third.wait_for_predecessors())),
            Poll::Pending
        );
        drop(first);
        third.wait_for_predecessors().await.expect("third is ready");
    }

    #[tokio::test]
    async fn a_cancelled_borrowed_wait_can_be_retried_without_bypassing_the_head() {
        let order = RemoteResourceUpdateOrder::default();
        let key = registration();
        let first = order.admit(key.clone()).expect("admit first");
        let mut second = order.admit(key).expect("admit second");

        assert_eq!(
            futures::poll!(std::pin::pin!(second.wait_for_predecessors())),
            Poll::Pending
        );
        assert_eq!(
            futures::poll!(std::pin::pin!(second.wait_for_predecessors())),
            Poll::Pending
        );
        drop(first);
        second
            .wait_for_predecessors()
            .await
            .expect("second is ready");
        second
            .wait_for_predecessors()
            .await
            .expect("readiness persists");
    }

    #[tokio::test]
    async fn multiple_interior_cancellations_preserve_the_live_turn_order() {
        let order = RemoteResourceUpdateOrder::default();
        let key = registration();
        let first = order.admit(key.clone()).expect("admit first");
        let second = order.admit(key.clone()).expect("admit second");
        let mut third = order.admit(key.clone()).expect("admit third");
        let fourth = order.admit(key.clone()).expect("admit fourth");
        let mut fifth = order.admit(key).expect("admit fifth");

        drop((second, fourth));
        assert_eq!(
            futures::poll!(std::pin::pin!(third.wait_for_predecessors())),
            Poll::Pending
        );
        assert_eq!(
            futures::poll!(std::pin::pin!(fifth.wait_for_predecessors())),
            Poll::Pending
        );
        drop(first);
        third.wait_for_predecessors().await.expect("third is ready");
        assert_eq!(
            futures::poll!(std::pin::pin!(fifth.wait_for_predecessors())),
            Poll::Pending
        );
        drop(third);
        fifth.wait_for_predecessors().await.expect("fifth is ready");
    }

    #[tokio::test]
    async fn concurrently_dropping_the_head_and_an_interior_turn_releases_the_next_live_turn() {
        for _ in 0..128 {
            let order = RemoteResourceUpdateOrder::default();
            let key = registration();
            let first = order.admit(key.clone()).expect("admit first");
            let second = order.admit(key.clone()).expect("admit second");
            let mut third = order.admit(key).expect("admit third");
            let barrier = Arc::new(std::sync::Barrier::new(3));
            let first_barrier = Arc::clone(&barrier);
            let second_barrier = Arc::clone(&barrier);
            let first_drop = std::thread::spawn(move || {
                first_barrier.wait();
                drop(first);
            });
            let second_drop = std::thread::spawn(move || {
                second_barrier.wait();
                drop(second);
            });
            barrier.wait();
            first_drop.join().expect("drop first");
            second_drop.join().expect("drop second");
            tokio::time::timeout(Duration::from_secs(1), third.wait_for_predecessors())
                .await
                .expect("the live head wakes")
                .expect("the live head is granted readiness");
            drop(third);
            assert_eq!(order.tracked_registrations(), 0);
        }
    }

    #[tokio::test]
    async fn different_registrations_do_not_wait_for_each_other() {
        let order = RemoteResourceUpdateOrder::default();
        let held_key = registration();
        let held = order.admit(held_key.clone()).expect("admit held");
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
        let mut newer_generation = order
            .admit(RemoteResourceUpdateKey {
                socket_generation: RemoteResourceSocketGeneration::next(Some(
                    held_key.socket_generation,
                )),
                ..held_key.clone()
            })
            .expect("admit newer generation");
        let mut blocked = order.admit(held_key).expect("admit blocked");

        replacement
            .wait_for_predecessors()
            .await
            .expect("replacement is ready");
        other.wait_for_predecessors().await.expect("other is ready");
        newer_generation
            .wait_for_predecessors()
            .await
            .expect("new generation is ready");
        assert_eq!(
            futures::poll!(std::pin::pin!(blocked.wait_for_predecessors())),
            Poll::Pending
        );
        drop(held);
        blocked
            .wait_for_predecessors()
            .await
            .expect("blocked turn is ready");
        drop((replacement, other, newer_generation, blocked));
        assert_eq!(order.tracked_registrations(), 0);
    }

    #[tokio::test]
    async fn admission_capacity_is_bounded_and_reused_immediately() {
        let order = RemoteResourceUpdateOrder::default();
        let key = registration();
        let mut queued = Vec::new();
        for _ in 0..MAX_PENDING_REMOTE_RESOURCE_UPDATES_PER_REGISTRATION {
            queued.push(order.admit(key.clone()).expect("within queue bound"));
        }
        assert_eq!(
            order.admit(key.clone()).err(),
            Some(RemoteResourceUpdateOrderFull::RegistrationQueue)
        );
        drop(queued.remove(1));
        let mut replacement = order
            .admit(key.clone())
            .expect("interior capacity is immediately reusable");
        assert_eq!(
            futures::poll!(std::pin::pin!(replacement.wait_for_predecessors())),
            Poll::Pending
        );
        assert_eq!(
            order.admit(key).err(),
            Some(RemoteResourceUpdateOrderFull::RegistrationQueue)
        );

        let mut others = Vec::new();
        for index in 1..MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS {
            others.push(
                order
                    .admit(key_for_index(index))
                    .expect("within registration bound"),
            );
        }
        let late_key = key_for_index(MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS);
        assert_eq!(
            order.admit(late_key.clone()).err(),
            Some(RemoteResourceUpdateOrderFull::Registrations)
        );
        drop(others.pop());
        let late = order
            .admit(late_key)
            .expect("registration capacity is immediately reusable");
        assert_eq!(
            order.tracked_registrations(),
            MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS
        );
        drop((queued, replacement, others, late));
        assert_eq!(order.tracked_registrations(), 0);
    }

    fn key_for_index(index: usize) -> RemoteResourceUpdateKey {
        key(
            &format!("user{index}@example.test/r"),
            RemoteResourceRegistrationId::fresh(),
        )
    }

    #[tokio::test]
    async fn poison_rejects_admission_and_closes_waiters_without_granting_them() {
        let order = RemoteResourceUpdateOrder::default();
        let key = registration();
        let mut first = order.admit(key.clone()).expect("admit first");
        let mut second = order.admit(key.clone()).expect("admit second");
        let mut third = order.admit(key.clone()).expect("admit third");
        let other_key = key_for_index(1);
        let mut other_head = order.admit(other_key.clone()).expect("admit other head");
        let mut other_waiter = order.admit(other_key).expect("admit other waiter");
        first
            .wait_for_predecessors()
            .await
            .expect("first is granted");
        other_head
            .wait_for_predecessors()
            .await
            .expect("other head is granted");
        assert_eq!(
            futures::poll!(std::pin::pin!(second.wait_for_predecessors())),
            Poll::Pending
        );
        assert_eq!(
            futures::poll!(std::pin::pin!(other_waiter.wait_for_predecessors())),
            Poll::Pending
        );
        order.poison_for_test();

        assert_eq!(
            order.admit(key).err(),
            Some(RemoteResourceUpdateOrderFull::Poisoned)
        );
        assert_eq!(
            second.wait_for_predecessors().await,
            Err(RemoteResourceUpdateOrderClosed)
        );
        assert_eq!(
            second.wait_for_predecessors().await,
            Err(RemoteResourceUpdateOrderClosed)
        );
        assert_eq!(
            third.wait_for_predecessors().await,
            Err(RemoteResourceUpdateOrderClosed)
        );
        assert_eq!(
            other_waiter.wait_for_predecessors().await,
            Err(RemoteResourceUpdateOrderClosed)
        );
        assert_eq!(first.wait_for_predecessors().await, Ok(()));
        assert_eq!(other_head.wait_for_predecessors().await, Ok(()));
    }

    #[tokio::test]
    async fn observing_poison_on_drop_closes_waiters_without_granting_them() {
        let order = RemoteResourceUpdateOrder::default();
        let key = registration();
        let first = order.admit(key.clone()).expect("admit first");
        let mut second = order.admit(key).expect("admit second");
        order.poison_for_test();
        drop(first);
        assert_eq!(
            second.wait_for_predecessors().await,
            Err(RemoteResourceUpdateOrderClosed)
        );
    }

    #[tokio::test]
    async fn a_grant_sent_before_the_order_closed_still_admits_its_unpolled_head() {
        let order = RemoteResourceUpdateOrder::default();
        let key = registration();
        let mut head = order.admit(key.clone()).expect("admit head");
        let mut waiter = order.admit(key.clone()).expect("admit waiter");
        order.poison_for_test();
        assert_eq!(
            order.admit(key).err(),
            Some(RemoteResourceUpdateOrderFull::Poisoned)
        );

        assert_eq!(head.wait_for_predecessors().await, Ok(()));
        drop(head);
        assert_eq!(
            waiter.wait_for_predecessors().await,
            Err(RemoteResourceUpdateOrderClosed)
        );
    }
}
