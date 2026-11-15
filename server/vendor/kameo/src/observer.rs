//! Process-wide observation of actor lifecycles and message handling.
//!
//! Waddle patch (see `WADDLE-PATCH.md`): a dependency-free hook so the host
//! can record uniform telemetry for every actor without kameo knowing the
//! telemetry stack.

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use crate::{Actor, error::ActorStopReason};

/// Receives actor lifecycle and message-handling observations. `actor` is
/// [`Actor::name`].
///
/// Methods run on the actor's own task, between messages: keep them cheap
/// and never block.
pub trait ActorObserver: Send + Sync {
    /// `on_start` succeeded and the actor is about to process its mailbox.
    fn actor_started(&self, _actor: &'static str) {}

    /// A started actor left its message loop for `reason`.
    fn actor_stopped(&self, _actor: &'static str, _reason: &ActorStopReason) {}

    /// A message handler returned (or panicked).
    ///
    /// `queue_wait` runs from the moment the sender built the message to the
    /// moment the handler began, so it includes time the sender spent waiting
    /// for mailbox capacity and, for messages buffered during startup,
    /// `on_start`. `handle_duration` is how long the handler occupied the
    /// actor; work a handler hands off through a delegated reply is not
    /// included.
    fn message_handled(
        &self,
        _actor: &'static str,
        _queue_wait: Duration,
        _handle_duration: Duration,
    ) {
    }
}

static OBSERVER: OnceLock<&'static dyn ActorObserver> = OnceLock::new();

/// Installs the process-wide observer. The first call wins; returns whether
/// this call installed it.
pub fn set_actor_observer(observer: &'static dyn ActorObserver) -> bool {
    OBSERVER.set(observer).is_ok()
}

#[inline]
fn observer() -> Option<&'static dyn ActorObserver> {
    OBSERVER.get().copied()
}

#[inline]
pub(crate) fn actor_started<A: Actor>() {
    if let Some(observer) = observer() {
        observer.actor_started(A::name());
    }
}

#[inline]
pub(crate) fn actor_stopped<A: Actor>(reason: &ActorStopReason) {
    if let Some(observer) = observer() {
        observer.actor_stopped(A::name(), reason);
    }
}

#[inline]
pub(crate) fn message_handled<A: Actor>(enqueued_at: Instant, started_at: Instant) {
    if let Some(observer) = observer() {
        observer.message_handled(
            A::name(),
            started_at.saturating_duration_since(enqueued_at),
            started_at.elapsed(),
        );
    }
}
