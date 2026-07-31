//! Typed counters for the `waddle.call.sfu_token.*` family, shared by
//! the protocol-layer Jingle handler and the server-side Muji gate so
//! one family is never emitted from divergent macro sites.

use super::attributes::{CallSetupFailureReason, SfuDenialReason};

/// Count a minted LiveKit SFU token.
pub fn increment_sfu_token_minted() {
    crate::counter_add!(
        "waddle.call.sfu_token.minted",
        "1",
        "LiveKit SFU tokens minted.",
        1,
    );
}

/// Count an SFU token denial by reason.
pub fn increment_sfu_token_denied(reason: SfuDenialReason) {
    crate::counter_add!(
        "waddle.call.sfu_token.denied",
        "1",
        "SFU token requests denied by reason.",
        1,
        reason,
    );
}

/// Count one Jingle `session-initiate` entering call setup (#1452).
///
/// This is the denominator of the call success rate. It is emitted
/// exactly once per attempt, at the point the action is known to be a
/// `session-initiate`; every attempt then terminates in exactly one
/// [`increment_call_setup_ok`] or [`increment_call_setup_failed`].
pub fn increment_call_setup_attempted() {
    add_call_setup_attempted(1);
}

fn add_call_setup_attempted(count: u64) {
    crate::counter_add!(
        "waddle.call.setup.attempted",
        "1",
        "Jingle call setups attempted (session-initiate).",
        count,
    );
}

/// Count a call setup that produced a usable session for the caller
/// (a join token was issued and the negotiation stanza was forwarded
/// or accepted).
///
/// Precisely: `ok` means the server authorized the attempt and handed
/// the invite to the router. A 1:1 invite whose peer then turns out to
/// be unroutable (offline/stale full JID) still counts `ok` — the
/// undeliverable IQ error is interpreted after the sans-I/O boundary,
/// where this tracker is out of scope. Client-side `chat.call.lifecycle`
/// telemetry captures that failure mode; folding the route disposition
/// into this counter is tracked as a follow-up (#1452 review).
pub fn increment_call_setup_ok() {
    add_call_setup_ok(1);
}

fn add_call_setup_ok(count: u64) {
    crate::counter_add!(
        "waddle.call.setup.ok",
        "1",
        "Jingle call setups that completed successfully.",
        count,
    );
}

/// Count a call setup that terminated in an error, by reason.
pub fn increment_call_setup_failed(reason: CallSetupFailureReason) {
    add_call_setup_failed(1, reason);
}

fn add_call_setup_failed(count: u64, reason: CallSetupFailureReason) {
    crate::counter_add!(
        "waddle.call.setup.failed",
        "1",
        "Jingle call setups that failed, by reason.",
        count,
        reason,
    );
}

/// `add(0)` the call setup SLI family — and every failure reason — so
/// a fresh pod exports it before the first call of a deploy (#1436):
/// `CallSetupFailureRate` and `CallSetupRoomNotFound` read these via
/// `increase()` and would otherwise evaluate to no-data. Called from
/// [`super::reliability::register_reliability_counters`].
pub(super) fn register_call_setup_counters() {
    add_call_setup_attempted(0);
    add_call_setup_ok(0);
    for reason in CallSetupFailureReason::ALL {
        add_call_setup_failed(0, reason);
    }
}

/// Count a call setup rejected at a gate that runs *before* the
/// per-attempt tracker in the Jingle handler opens — the server-side
/// Muji membership gate (which short-circuits IQ dispatch entirely),
/// the `session-initiate` rate limiter, and the handler's own
/// pre-routing wire-shape checks.
///
/// Emits the attempted/failed pair together so `attempted` stays a
/// true denominator: a gate rejection is a complete attempt that
/// simply never reached the negotiation path.
pub fn record_call_setup_rejected(reason: CallSetupFailureReason) {
    increment_call_setup_attempted();
    increment_call_setup_failed(reason);
}

/// Count a webhook-driven teardown dropped because its observed SID
/// belongs to an older call incarnation.
pub fn increment_call_teardown_stale_dropped() {
    crate::counter_add!(
        "waddle.call.teardown.stale_dropped",
        "1",
        "Webhook-driven call teardowns dropped due to stale room or participant SIDs.",
        1,
    );
}

/// Record the durable teardown queue depth observed by one janitor sweep.
pub fn record_call_teardown_outbox_depth(depth: u64) {
    crate::histogram_record!(
        "waddle.call.teardown.outbox.depth",
        "1",
        "Queued durable call teardown intents observed at sweep time.",
        depth as f64,
    );
}

/// Record the age of the oldest queued teardown intent observed by a sweep.
pub fn record_call_teardown_outbox_oldest_age(seconds: f64) {
    crate::histogram_record!(
        "waddle.call.teardown.outbox.oldest_age",
        "s",
        "Age of the oldest queued durable call teardown intent.",
        buckets: crate::telemetry::SECOND_SCALE_BUCKETS,
        seconds,
    );
}

/// Count durable teardown intents completed by the outbox drain.
pub fn add_call_teardown_outbox_drained(count: u64) {
    if count > 0 {
        crate::counter_add!(
            "waddle.call.teardown.outbox.drained",
            "1",
            "Durable call teardown intents completed by the outbox drain.",
            count,
        );
    }
}

/// Count durable teardown intents scheduled for another attempt.
pub fn add_call_teardown_outbox_requeued(count: u64) {
    if count > 0 {
        crate::counter_add!(
            "waddle.call.teardown.outbox.requeued",
            "1",
            "Durable call teardown intents requeued after retryable failures.",
            count,
        );
    }
}

/// Count durable teardown intents that exhausted their retry budget.
pub fn add_call_teardown_outbox_failed(count: u64) {
    if count > 0 {
        crate::counter_add!(
            "waddle.call.teardown.outbox.failed",
            "1",
            "Durable call teardown intents moved to terminal failure.",
            count,
        );
    }
}

/// Record the wall-clock duration of one LiveKit reconciliation pass.
pub fn record_reconcile_pass_duration(seconds: f64) {
    crate::histogram_record!(
        "waddle.call.reconcile.pass_duration",
        "s",
        "Wall-clock duration of one LiveKit call reconciliation pass.",
        buckets: crate::telemetry::SECOND_SCALE_BUCKETS,
        seconds,
    );
}

/// Add the number of rooms whose occupancy was examined in a pass.
pub fn add_reconcile_rooms_examined(count: u64) {
    if count == 0 {
        return;
    }
    crate::counter_add!(
        "waddle.call.reconcile.rooms_examined",
        "1",
        "LiveKit rooms examined by call reconciliation.",
        count,
    );
}

/// Add the number of LiveKit rooms adopted into the local registry.
pub fn add_reconcile_rooms_adopted(count: u64) {
    if count == 0 {
        return;
    }
    crate::counter_add!(
        "waddle.call.reconcile.rooms_adopted",
        "1",
        "LiveKit rooms adopted into the local call registry.",
        count,
    );
}

/// Add the number of rooms from which at least one ghost was swept.
pub fn add_reconcile_rooms_swept(count: u64) {
    if count == 0 {
        return;
    }
    crate::counter_add!(
        "waddle.call.reconcile.rooms_swept",
        "1",
        "LiveKit rooms with registry ghosts swept by reconciliation.",
        count,
    );
}

/// Add failed per-room occupancy probes.
///
/// This is alert-worthy operational evidence, but unlike the #1436
/// reliability-rate families it must not be startup zero-registered:
/// absence means no failure has ever ticked in this process.
pub fn add_reconcile_occupancy_failures(count: u64) {
    if count == 0 {
        return;
    }
    crate::counter_add!(
        "waddle.call.reconcile.occupancy_failures",
        "1",
        "LiveKit room occupancy probes failed during call reconciliation.",
        count,
    );
}
