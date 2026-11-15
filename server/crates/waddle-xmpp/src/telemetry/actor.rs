//! Uniform instruments for every kameo actor.
//!
//! `waddle-server` installs a `kameo::observer::ActorObserver` that calls
//! these, so each actor reports the same signals from one seam. The message
//! type is deliberately not an attribute (hundreds of types); the
//! `actor.handle_message` span carries it.

use std::time::Duration;

use super::attributes::{ActorKind, ActorStopKind};

/// Seconds-scale boundaries reaching down to 100 µs: most handlers finish
/// well under the 5 ms floor of `SECOND_SCALE_BUCKETS`.
const SUB_MILLISECOND_BUCKETS: [f64; 14] = [
    0.0001, 0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// An actor handled one message from its mailbox.
///
/// `queue_wait` is recorded only for messages that reach a handler, so a
/// stuck actor shows as a handled rate that stops, not as a long wait.
pub fn record_actor_message(actor: ActorKind, queue_wait: Duration, handle_duration: Duration) {
    crate::histogram_record!(
        "waddle.actor.message.queue_wait",
        "s",
        "Time from a sender building a message to its handler starting, by actor kind.",
        buckets: SUB_MILLISECOND_BUCKETS,
        queue_wait.as_secs_f64(),
        actor,
    );
    crate::histogram_record!(
        "waddle.actor.message.handle.duration",
        "s",
        "Time a message handler occupied its actor, by actor kind.",
        buckets: SUB_MILLISECOND_BUCKETS,
        handle_duration.as_secs_f64(),
        actor,
    );
}

/// An actor finished `on_start` and began processing its mailbox.
pub fn record_actor_started(actor: ActorKind) {
    crate::counter_add!(
        "waddle.actor.started",
        "{actor}",
        "Actors that started, by actor kind.",
        1,
        actor,
    );
}

fn add_actor_stopped(count: u64, actor: ActorKind, reason: ActorStopKind) {
    crate::counter_add!(
        "waddle.actor.stopped",
        "{actor}",
        "Started actors that stopped, by actor kind and whether a panic caused it.",
        count,
        actor,
        reason,
    );
}

/// A started actor left its message loop.
pub fn record_actor_stopped(actor: ActorKind, reason: ActorStopKind) {
    add_actor_stopped(1, actor, reason);
}

/// Zero-registers the stop counter so the first panic of a pod is an
/// increase from 0, not a new series an alert cannot see.
pub(super) fn register_actor_counters() {
    for actor in ActorKind::ALL {
        for reason in ActorStopKind::ALL {
            add_actor_stopped(0, actor, reason);
        }
    }
}
