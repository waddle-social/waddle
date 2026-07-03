//! Connection-local timer wheel for state-machine-owned timers.
//!
//! The sans-io core emits typed timer effects
//! ([`OutboundEvent::SetTimer`] / [`OutboundEvent::CancelTimer`],
//! relayed as [`TimerCommand`]s by the interpreter); this wheel is the
//! transport adapter's clock that realizes them. The connection loop
//! selects on [`TransportTimers::next_expired`] and feeds each fired
//! id back into the machine as `InboundEvent::Tick(id)`.
//!
//! Timers are one-shot: a fired id is popped and stays disarmed until
//! the machine re-arms it (the RFC 7395 keepalive policy re-arms on
//! every tick). Arming an already-armed id replaces its deadline.
//!
//! [`OutboundEvent::SetTimer`]: waddle_xmpp::protocol::OutboundEvent::SetTimer
//! [`OutboundEvent::CancelTimer`]: waddle_xmpp::protocol::OutboundEvent::CancelTimer

use crate::server::routes::interpret::TimerCommand;
use std::time::Duration;
use tokio::time::Instant;
use waddle_xmpp::protocol::TimerId;

/// The adapter-owned realization of the state machine's timers.
///
/// Sized for a handful of concurrent timers per connection (today:
/// exactly one, the keepalive clock), hence a `Vec` rather than a heap.
#[derive(Debug, Default)]
pub(super) struct TransportTimers {
    deadlines: Vec<(TimerId, Instant)>,
}

impl TransportTimers {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Apply a batch of interpreter-relayed timer commands.
    pub(super) fn apply(&mut self, commands: impl IntoIterator<Item = TimerCommand>) {
        for command in commands {
            match command {
                TimerCommand::Set { id, duration_ms } => {
                    self.set(id, Duration::from_millis(duration_ms));
                }
                TimerCommand::Cancel(id) => self.cancel(id),
            }
        }
    }

    fn set(&mut self, id: TimerId, after: Duration) {
        let deadline = Instant::now() + after;
        if let Some(slot) = self.deadlines.iter_mut().find(|(armed, _)| *armed == id) {
            slot.1 = deadline;
        } else {
            self.deadlines.push((id, deadline));
        }
    }

    fn cancel(&mut self, id: TimerId) {
        self.deadlines.retain(|(armed, _)| *armed != id);
    }

    /// Sleep until the earliest armed timer expires, pop it, and return
    /// its id. Pends forever when nothing is armed.
    ///
    /// Cancellation-safe for `tokio::select!`: state is only mutated
    /// after the sleep completes, so a dropped future leaves every
    /// deadline armed.
    pub(super) async fn next_expired(&mut self) -> TimerId {
        let Some((id, deadline)) = self
            .deadlines
            .iter()
            .min_by_key(|(_, deadline)| *deadline)
            .copied()
        else {
            return std::future::pending().await;
        };
        tokio::time::sleep_until(deadline).await;
        self.cancel(id);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: TimerId = TimerId(1);
    const B: TimerId = TimerId(2);

    #[tokio::test(start_paused = true)]
    async fn fires_in_deadline_order_and_pops() {
        let mut timers = TransportTimers::new();
        timers.apply([
            TimerCommand::Set {
                id: B,
                duration_ms: 200,
            },
            TimerCommand::Set {
                id: A,
                duration_ms: 100,
            },
        ]);
        assert_eq!(timers.next_expired().await, A);
        assert_eq!(timers.next_expired().await, B);
        // Both popped: nothing armed → pending forever.
        let idle = tokio::time::timeout(Duration::from_secs(3600), timers.next_expired()).await;
        assert!(idle.is_err(), "empty wheel must pend");
    }

    #[tokio::test(start_paused = true)]
    async fn rearm_replaces_the_deadline() {
        let mut timers = TransportTimers::new();
        timers.apply([TimerCommand::Set {
            id: A,
            duration_ms: 100,
        }]);
        timers.apply([TimerCommand::Set {
            id: A,
            duration_ms: 500,
        }]);
        let fired = tokio::time::timeout(Duration::from_millis(300), timers.next_expired()).await;
        assert!(fired.is_err(), "old deadline must be replaced");
        assert_eq!(timers.next_expired().await, A);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_disarms() {
        let mut timers = TransportTimers::new();
        timers.apply([TimerCommand::Set {
            id: A,
            duration_ms: 100,
        }]);
        timers.apply([TimerCommand::Cancel(A)]);
        let idle = tokio::time::timeout(Duration::from_secs(3600), timers.next_expired()).await;
        assert!(idle.is_err(), "cancelled timer must not fire");
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_wait_leaves_timers_armed() {
        let mut timers = TransportTimers::new();
        timers.apply([TimerCommand::Set {
            id: A,
            duration_ms: 200,
        }]);
        // Poll-and-drop before expiry (select! losing the race).
        let premature =
            tokio::time::timeout(Duration::from_millis(50), timers.next_expired()).await;
        assert!(premature.is_err());
        // The deadline survives the dropped future.
        assert_eq!(timers.next_expired().await, A);
    }
}
