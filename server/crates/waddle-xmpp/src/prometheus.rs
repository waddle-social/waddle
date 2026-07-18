//! Lightweight Prometheus exporter for core XMPP runtime metrics.
//!
//! This module tracks a small set of process-level metrics required for
//! operational health dashboards and exposes them in Prometheus text format.

use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(test, feature = "test-utils"))]
use std::sync::OnceLock;

use crate::telemetry::attributes::PushSuppressReason;

static CONNECTED_USERS: AtomicU64 = AtomicU64::new(0);
static ROOM_COUNT: AtomicU64 = AtomicU64::new(0);
static MESSAGES_TOTAL: AtomicU64 = AtomicU64::new(0);
static CURRENT_SECOND: AtomicU64 = AtomicU64::new(0);
static CURRENT_SECOND_MESSAGES: AtomicU64 = AtomicU64::new(0);
static LAST_SECOND_MESSAGES: AtomicU64 = AtomicU64::new(0);

// Non-blocking broadcast outcomes (see `registry::BroadcastOutcome`).
// Counts every attempt made via `ConnectionRegistry::try_send_to`; a
// non-zero `broadcast_dropped_full` is the signal that a recipient's
// outbound channel backpressured and a stanza was silently dropped.
static BROADCAST_DELIVERED: AtomicU64 = AtomicU64::new(0);
static BROADCAST_NOT_CONNECTED: AtomicU64 = AtomicU64::new(0);
static BROADCAST_DROPPED_FULL: AtomicU64 = AtomicU64::new(0);
static BROADCAST_DROPPED_CLOSED: AtomicU64 = AtomicU64::new(0);
// ADR-0017 Phase 1 Slice 2 actor-path delivery: a terminal `ask` failure
// whose message MAY have been enqueued (`ActorStopped`, reply-wait
// `Timeout(None)`) drops the frame rather than routing it to the detached
// XEP-0198 buffer, because kameo does not cancel an enqueued handler and a
// post-timeout run plus a detached replay would double-deliver. A non-zero
// value is the signal that a live-recipient frame was dropped under actor
// backpressure — the enqueue-uncertain sibling of `broadcast_dropped_full`.
static DELIVERY_TERMINAL_ERROR_DROP: AtomicU64 = AtomicU64::new(0);

// #1263 delivery-loss surfacing: a frame (groupchat reflection, MUC
// presence fan-out, or actor-path full-JID delivery) was dropped because
// the recipient's outbound channel was STILL full after the bounded
// in-line retries. A non-zero value means a live recipient missed a
// stanza under sustained backpressure — for MUC presence the recipient's
// occupant roster may be stale until their next rejoin/resync.
static DELIVERY_RETRY_EXHAUSTED_DROP: AtomicU64 = AtomicU64::new(0);

// Rejected-join resolver-affiliation repairs dropped because the bounded
// process-wide scheduler already had its maximum number of active worker
// keys. A non-zero value means the authoritative join decision was still
// enforced, but a live RoomActor may retain a stale resolver-derived
// affiliation until a later repair or eviction.
static RESOLVER_AFFILIATION_SYNC_CAPACITY_DROP: AtomicU64 = AtomicU64::new(0);

// ADR-0017 Phase 1 Slice 2: empty `UserActor`s reaped by the periodic reaper
// after `try_deliver`'s closed-channel eviction removed their last resource
// without the explicit unregister-prune path running. A non-zero value is the
// signal that the delivery-eviction path is orphaning actors that the reaper is
// cleaning up — expected to stay low; sustained growth hints at dropped
// unregister mirrors.
static USER_ACTOR_REAPED: AtomicU64 = AtomicU64::new(0);

// XEP-0198 unacked-queue evictions (see `stream_management::UnackedQueue`).
// A non-zero counter means at least one stanza was evicted from an SM
// session's replay buffer while that session was still resumable — a
// later `<resume/>` with an older h must fail rather than claiming a
// complete replay window.
static SM_UNACKED_EVICTED: AtomicU64 = AtomicU64::new(0);

// Issue #209 finding #11 — observability for the offline-DM /
// SM-expiry surface. None of these existed before; the entire
// runtime behavior described by issue #209 was previously
// unobservable beyond grep'ing log lines.
//
// `pending_delivery_quota_exceeded`: per-recipient cap hit at intake
// (XEP-0160 §3 step 3 bounce). Sustained non-zero indicates a
// recipient queue saturated by a single sender or a permanently-
// offline target.
static PENDING_DELIVERY_QUOTA_EXCEEDED: AtomicU64 = AtomicU64::new(0);
// `pending_delivery_orphan_claims_released`: claim-expiry janitor
// activity — non-zero is normal (sessions die without acks); a
// growing rate signals broken SM lifecycle.
static PENDING_DELIVERY_ORPHAN_CLAIMS_RELEASED: AtomicU64 = AtomicU64::new(0);
// `pending_delivery_aged_out`: aging janitor (issue #209 finding #5)
// drops rows older than the configured max age. Sustained non-zero
// indicates recipients with permanently-stale queues.
static PENDING_DELIVERY_AGED_OUT: AtomicU64 = AtomicU64::new(0);
// `pending_delivery_unresolved_poison_pill`: flush could not
// materialize a row's MAM payload and dropped it. Should be ~0 on a
// healthy deployment; non-zero signals MAM corruption.
static PENDING_DELIVERY_UNRESOLVED_POISON_PILL: AtomicU64 = AtomicU64::new(0);
// `pending_delivery_archive_lookup_transient_failure`: flush hit a
// transient MAM storage error (Database outage — permanent decode
// corruption takes the poison-pill path instead) resolving an
// Archived row. The failure is batch-fatal: the failing row and every
// remaining claimed row are RELEASED (FIFO preserved) and the flush
// aborts; the client's next presence update retries (issue #1122).
// Distinct from the poison-pill counter: this one signals MAM storage
// availability problems, not archive corruption, and no mail is lost.
// Incremented once per aborted flush, not once per released row.
static PENDING_DELIVERY_ARCHIVE_LOOKUP_TRANSIENT_FAILURE: AtomicU64 = AtomicU64::new(0);
// `sm_promotion_storage_failed`: Q6 promotion encountered a
// pending_delivery insert error and preserved the durable SM row for
// retry (issue #209 PR #346 + finding #14 dead-letter cap).
static SM_PROMOTION_STORAGE_FAILED: AtomicU64 = AtomicU64::new(0);
// `sm_promotion_not_promotable`: Q6 promotion saw an unacked stanza
// that is valid XMPP but not an XEP-0160 offline-message candidate.
// This is expected for XEP-0313 MAM result/fin frames addressed to
// stale full-JID resources.
static SM_PROMOTION_NOT_PROMOTABLE: AtomicU64 = AtomicU64::new(0);
// `sm_promotion_blocklist_failed`: blocklist storage load failed
// during Q6 promotion; the session was skipped fail-closed.
static SM_PROMOTION_BLOCKLIST_FAILED: AtomicU64 = AtomicU64::new(0);
// `sm_promotion_dead_lettered`: a session crossed the configured
// promotion-attempt threshold and was dead-lettered (issue #209
// finding #14). Each event is a permanent loss of unacked stanzas
// from one session.
static SM_PROMOTION_DEAD_LETTERED: AtomicU64 = AtomicU64::new(0);
// `sm_drain_timeout`: graceful-shutdown drain hit the configured
// deadline with sessions still pending. Each event implies durable
// rows surviving for restart-time retry.
static SM_DRAIN_TIMEOUT: AtomicU64 = AtomicU64::new(0);
// `sm_resume_window_clamped`: a client requested a resume window
// larger than the server-side cap (`WADDLE_SM_MAX_RESUME_SECS`) and
// was silently lowered. Sustained non-zero indicates the cap is too
// tight for the client population.
static SM_RESUME_WINDOW_CLAMPED: AtomicU64 = AtomicU64::new(0);

// XEP-0198 send-window pacing (issue #1219). The consumer-side pace
// gate stops feeding the SM unacked queue once the outstanding count
// crosses the high watermark and awaits client acks instead, so the
// 1000-slot queue never overflows and poisons resume.
//
// `sm_send_window_pauses`: times a wire-write path engaged the pause.
// Healthy non-zero under burst (MAM catch-up, fan-out); it is the
// signal pacing is doing its job INSTEAD of evicting.
static SM_SEND_WINDOW_PAUSES: AtomicU64 = AtomicU64::new(0);
// `sm_send_window_pause_timeouts`: a pause outlived its deadline with
// no recovering ack — the client is dead/stalled. The connection closes
// into the normal detach-for-resume path with a capped replay queue.
// Sustained non-zero hints at a widespread client-ack or network fault.
static SM_SEND_WINDOW_PAUSE_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
// `sm_detached_unacked_evicted`: entries evicted from a DETACHED
// session's unacked queue when it hit capacity while the stream was
// awaiting resume. Previously silent (`session.rs`); a non-zero value
// means a resume with an older `h` for that session must fail rather
// than replay an incomplete window — the detached sibling of
// `sm_unacked_evicted`.
static SM_DETACHED_UNACKED_EVICTED: AtomicU64 = AtomicU64::new(0);

// XEP-0160 batched pending-delivery flush (issue #1220).
// `pending_flush_batches`: `claim_batch_for_session` batches drained
// across all flushes. `pending_flush_rows_pushed`: replay stanzas
// pushed to recovering resources. Together they make the off-task
// batched flush observable (rows/batches ≈ mean batch fill).
static PENDING_FLUSH_BATCHES: AtomicU64 = AtomicU64::new(0);
static PENDING_FLUSH_ROWS_PUSHED: AtomicU64 = AtomicU64::new(0);

// XEP-0357 push pipeline pass-through counters (#531). Provider-side
// metrics (`provider_sent`, `provider_rejected`, `expired_token`)
// land alongside #528/#529/#530; this slice covers the durable-
// pipeline side that is observable today.
//
// `waddle_push_candidate_created_total`: every `Inserted` outcome
// from `notification_outbox::insert_candidate`. A sustained
// upward slope tracks the post-T0 notification-eligible message
// volume — T0-suppressed candidates never call insert_candidate
// (they bump only `waddle_push_suppressed_total{reason}`).
// However, T1 re-evaluation (the race-window guard inside
// `drain_pending_candidates_into_outbox`) can also fire
// suppression on rows this counter already counted at T0; for
// those, both this counter AND
// `waddle_push_suppressed_total{reason}` increment. Reconcile
// against published + suppressed over a window, not against
// strict equality at a point in time.
static PUSH_CANDIDATE_CREATED: AtomicU64 = AtomicU64::new(0);

// `waddle_push_candidate_coalesced_total`: every `Duplicate`
// outcome from `notification_outbox::insert_candidate`. The
// `notification_candidates` table carries TWO intentional
// unique constraints; both bucket here:
//
// - PRIMARY KEY on the six-column tuple
//   `(recipient_bare_jid, conversation_jid, thread_id,
//    stanza_id_by, stanza_id, class)` — exact-identity dedup.
// - `idx_notification_candidates_identity` UNIQUE index on
//   `(recipient_bare_jid, conversation_jid, thread_id,
//    stanza_id, class)` — cross-archive dedup for the same
//   logical stanza minted under different `by=` JIDs (XEP-0359).
//
// Either constraint firing returns Duplicate. Burst replays,
// retried T0 emission, cross-archive duplicates, or genuine
// duplicate stanzas coalesce here. A sudden spike often signals
// a retry loop upstream rather than a real burst.
static PUSH_CANDIDATE_COALESCED: AtomicU64 = AtomicU64::new(0);

// `waddle_push_outbox_published_total`: every successful
// `publish_claimed_job` outcome (the typed
// `NotificationOutboxPublishOutcome::Published` arm). The
// XEP-0060 §7.1 `<publish>` IQ to the user-server's Push Service
// node was accepted — i.e. the `push_publish_jobs` row was
// created. Provider-side fanout (Web/APNs/FCM) happens
// asynchronously and is observed by separate counters landing
// alongside #528/#529/#530; this counter stops at the XMPP layer.
// Difference between this and candidates_created over a window
// equals coalesced + in-flight + suppressed-at-T1.
static PUSH_OUTBOX_PUBLISHED: AtomicU64 = AtomicU64::new(0);

// `waddle_push_outbox_retry_scheduled_total{reason}`: every
// transient-failure outcome from
// `retry_or_fail_outcome_for_claimed_job` that schedules a future
// retry (the typed `RetryScheduled` arm). Sustained non-zero with
// flat `published_total` indicates the Push Service boundary is
// wedged.
//
// Labeled by the typed transient-failure reason. Today the counter
// is a single bucket rendered as `reason="unknown"` — the closed-
// set values (`5xx`, `timeout`, `auth`, `unknown`) land alongside
// the provider slices in #528/#529/#530. The labeling decision is
// taken NOW so PromQL alerts written today match all future
// variants without breaking on label introduction.
static PUSH_OUTBOX_RETRY_SCHEDULED: AtomicU64 = AtomicU64::new(0);

// `waddle_push_outbox_dead_lettered_total`: every terminal-`failed`
// outcome from `drain_due_outbox_jobs` — i.e. any path that
// produces `NotificationOutboxPublishOutcome::Failed`. The arm
// covers BOTH (a) `retry_or_fail_outcome_for_claimed_job` after
// the `MAX_OUTBOX_ATTEMPTS` retry budget is exhausted, AND
// (b) immediate hard-failure branches inside `publish_claimed_job`
// that bypass the retry budget — non-first-party Push Service
// target, XEP-0191 blocked sender at publish time, missing
// XEP-0357 registration row, etc. Operators cannot distinguish
// "retry exhaustion" from "policy hard-fail" purely from this
// counter today; correlate against the recent outbox row's
// `last_error` column or wait for the typed
// `Permanent`-vs-`Transient` mapping landing alongside the
// provider PRs (#528/#529/#530).
//
// Sustained non-zero rate is alert-worthy, but isolated dead-
// letters are expected during normal provider-side device
// revocation (APNs `Unregistered` / FCM `UNREGISTERED` flows
// produce a terminal failure per job).
static PUSH_OUTBOX_DEAD_LETTERED: AtomicU64 = AtomicU64::new(0);

// `waddle_dnd_projection_read_errored_total` — incremented every
// time `crate::dnd_reader::PepDndReader::dnd_state` swallows a
// `DndProjectionError` and defaults the recipient to `Inactive`.
//
// This is the silent-fail-open signal: a transient SQLite contention
// storm could flip every DND-active user to "not in DND" at the T1
// push gate, and the only otherwise-visible trace would be
// per-recipient `warn!` lines. A non-zero rate on this counter
// indicates Operator-Action-Required, because users are receiving
// push notifications they explicitly opted out of.
static DND_PROJECTION_READ_ERRORED: AtomicU64 = AtomicU64::new(0);

// `waddle_push_suppressed_total{reason="..."}` — incremented every
// time a XEP-0357 push candidate is suppressed at either T0 emission
// or the T1 drain. Labeled by the typed `SuppressedReason` enum.
//
// **Wire contract**: [`PUSH_SUPPRESSED_REASONS`] is compiled directly
// from [`PushSuppressReason::VALUES`]. `waddle-server` exhaustively maps
// its persisted audit enum into that metric enum and tests that its DB
// values remain byte-identical.
//
// Storage is a fixed `[AtomicU64; N]` array indexed by the typed reason.
// No mutex, no allocations, no cardinality growth at runtime. The sealed
// `PushSuppressReason` enum makes an unmapped reason a compile error, so
// no unknown-reason catch-all exists (deleted with #1330's typed plumbing).
pub(crate) const PUSH_SUPPRESSED_REASONS: &[&str] = &PushSuppressReason::VALUES;

const PUSH_SUPPRESSED_COUNTERS_LEN: usize = PUSH_SUPPRESSED_REASONS.len();

static PUSH_SUPPRESSED_COUNTERS: [AtomicU64; PUSH_SUPPRESSED_COUNTERS_LEN] = {
    // `AtomicU64` is not `Copy`, so the array initializer must spell
    // every slot. The length is locked at compile time to
    // `PUSH_SUPPRESSED_REASONS.len()` via a `const _: () = assert!`
    // below, so a future reason addition that forgets a slot fails to
    // compile.
    [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ]
};

// Compile-time guard: a future reason addition that grows
// `PUSH_SUPPRESSED_REASONS` MUST also grow the counter array, or
// this assertion will fail at build time.
const _: () = assert!(
    PUSH_SUPPRESSED_REASONS.len() == PUSH_SUPPRESSED_COUNTERS_LEN,
    "PUSH_SUPPRESSED_REASONS and PUSH_SUPPRESSED_COUNTERS must stay the same length"
);

/// Public read-only view of the metric reason values. The
/// `waddle-server` test suite uses this to assert that every
/// `SuppressedReason::as_db_value()` appears in the wire contract.
pub fn push_suppressed_reasons() -> &'static [&'static str] {
    PUSH_SUPPRESSED_REASONS
}

/// Serializes unit tests that mutate process-global metrics.
///
/// Re-exported under `cfg(any(test, feature = "test-utils"))` so the
/// `waddle-server` test suite (which exercises the labeled
/// push-suppressed counter) can serialize against the same lock the
/// in-crate tests use. Backed by an async-aware mutex so async tests
/// can hold the lock across `.await` without tripping clippy's
/// `await_holding_lock`.
#[cfg(any(test, feature = "test-utils"))]
pub fn metrics_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn unix_timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn rotate_second_bucket(now: u64) {
    let tracked_second = CURRENT_SECOND.load(Ordering::Acquire);
    if tracked_second == now {
        return;
    }

    if CURRENT_SECOND
        .compare_exchange(tracked_second, now, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let previous_second_count = CURRENT_SECOND_MESSAGES.swap(0, Ordering::AcqRel);
        LAST_SECOND_MESSAGES.store(previous_second_count, Ordering::Release);
    }
}

pub fn increment_connected_users() {
    CONNECTED_USERS.fetch_add(1, Ordering::AcqRel);
}

pub fn decrement_connected_users() {
    let _ = CONNECTED_USERS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_sub(1))
    });
}

pub fn increment_room_count() {
    ROOM_COUNT.fetch_add(1, Ordering::AcqRel);
}

pub fn decrement_room_count() {
    let _ = ROOM_COUNT.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_sub(1))
    });
}

pub fn record_message_processed() {
    let now = unix_timestamp_secs();
    rotate_second_bucket(now);
    MESSAGES_TOTAL.fetch_add(1, Ordering::Relaxed);
    CURRENT_SECOND_MESSAGES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_broadcast_delivered() {
    BROADCAST_DELIVERED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_broadcast_not_connected() {
    BROADCAST_NOT_CONNECTED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_broadcast_dropped_full() {
    BROADCAST_DROPPED_FULL.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_broadcast_dropped_closed() {
    BROADCAST_DROPPED_CLOSED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_delivery_terminal_error_drop() {
    DELIVERY_TERMINAL_ERROR_DROP.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_delivery_retry_exhausted_drop() {
    DELIVERY_RETRY_EXHAUSTED_DROP.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_resolver_affiliation_sync_capacity_drop() {
    RESOLVER_AFFILIATION_SYNC_CAPACITY_DROP.fetch_add(1, Ordering::Relaxed);
}

#[cfg(any(test, feature = "test-utils"))]
pub fn resolver_affiliation_sync_capacity_drop_count() -> u64 {
    RESOLVER_AFFILIATION_SYNC_CAPACITY_DROP.load(Ordering::Relaxed)
}

#[cfg(any(test, feature = "test-utils"))]
pub fn delivery_retry_exhausted_drop_count() -> u64 {
    DELIVERY_RETRY_EXHAUSTED_DROP.load(Ordering::Relaxed)
}

pub(crate) fn increment_user_actor_reaped() {
    USER_ACTOR_REAPED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_sm_unacked_evicted() {
    SM_UNACKED_EVICTED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_pending_delivery_quota_exceeded() {
    PENDING_DELIVERY_QUOTA_EXCEEDED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn add_pending_delivery_orphan_claims_released(n: u64) {
    PENDING_DELIVERY_ORPHAN_CLAIMS_RELEASED.fetch_add(n, Ordering::Relaxed);
}

pub(crate) fn add_pending_delivery_aged_out(n: u64) {
    PENDING_DELIVERY_AGED_OUT.fetch_add(n, Ordering::Relaxed);
}

pub(crate) fn increment_pending_delivery_unresolved_poison_pill() {
    PENDING_DELIVERY_UNRESOLVED_POISON_PILL.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_pending_delivery_archive_lookup_transient_failure() {
    PENDING_DELIVERY_ARCHIVE_LOOKUP_TRANSIENT_FAILURE.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn add_sm_promotion_storage_failed(n: u64) {
    SM_PROMOTION_STORAGE_FAILED.fetch_add(n, Ordering::Relaxed);
}

pub(crate) fn increment_sm_promotion_not_promotable() {
    SM_PROMOTION_NOT_PROMOTABLE.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_sm_promotion_blocklist_failed() {
    SM_PROMOTION_BLOCKLIST_FAILED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_sm_promotion_dead_lettered() {
    SM_PROMOTION_DEAD_LETTERED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_sm_drain_timeout() {
    SM_DRAIN_TIMEOUT.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn increment_sm_resume_window_clamped() {
    SM_RESUME_WINDOW_CLAMPED.fetch_add(1, Ordering::Relaxed);
}

/// Increment `waddle_sm_send_window_pauses_total` — a wire-write path
/// engaged the XEP-0198 send-window pause (issue #1219).
pub(crate) fn increment_sm_send_window_pause() {
    SM_SEND_WINDOW_PAUSES.fetch_add(1, Ordering::Relaxed);
}

/// Increment `waddle_sm_send_window_pause_timeouts_total` — a send-window
/// pause outlived its deadline with no recovering ack (issue #1219).
pub(crate) fn increment_sm_send_window_pause_timeout() {
    SM_SEND_WINDOW_PAUSE_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
}

/// Increment `waddle_sm_detached_unacked_evicted_total` — a detached
/// session's unacked queue evicted an entry at capacity while awaiting
/// resume (issue #1219; previously silent).
pub(crate) fn increment_sm_detached_unacked_evicted() {
    SM_DETACHED_UNACKED_EVICTED.fetch_add(1, Ordering::Relaxed);
}

/// Add to `waddle_pending_flush_batches_total` — batches drained by the
/// batched offline flush (issue #1220).
pub(crate) fn add_pending_flush_batches(n: u64) {
    PENDING_FLUSH_BATCHES.fetch_add(n, Ordering::Relaxed);
}

/// Add to `waddle_pending_flush_rows_pushed_total` — replay stanzas pushed
/// to recovering resources by the offline flush (issue #1220).
pub(crate) fn add_pending_flush_rows_pushed(n: u64) {
    PENDING_FLUSH_ROWS_PUSHED.fetch_add(n, Ordering::Relaxed);
}

/// Increment the `waddle_push_candidate_created_total` counter. Call
/// from the `Inserted` arm of `notification_outbox::insert_candidate`.
pub(crate) fn increment_push_candidate_created() {
    PUSH_CANDIDATE_CREATED.fetch_add(1, Ordering::Relaxed);
}

/// Increment the `waddle_push_candidate_coalesced_total` counter.
/// Call from the `Duplicate` arm of
/// `notification_outbox::insert_candidate` — a candidate row already
/// existed for this `(recipient, conversation, thread, stanza_id,
/// class)` tuple.
pub(crate) fn increment_push_candidate_coalesced() {
    PUSH_CANDIDATE_COALESCED.fetch_add(1, Ordering::Relaxed);
}

/// Increment the `waddle_push_outbox_published_total` counter. Call
/// from the `Published` outcome of `publish_claimed_job` — the
/// XEP-0357 publish made it past the Push Service boundary.
pub(crate) fn increment_push_outbox_published() {
    PUSH_OUTBOX_PUBLISHED.fetch_add(1, Ordering::Relaxed);
}

/// Increment the `waddle_push_outbox_retry_scheduled_total` counter.
/// Call from the `RetryScheduled` outcome of
/// `retry_or_fail_outcome_for_claimed_job` — the job failed
/// transiently and a future retry is queued.
pub(crate) fn increment_push_outbox_retry_scheduled() {
    PUSH_OUTBOX_RETRY_SCHEDULED.fetch_add(1, Ordering::Relaxed);
}

/// Increment the `waddle_push_outbox_dead_lettered_total` counter.
/// Call from the `Failed` outcome of
/// `retry_or_fail_outcome_for_claimed_job` — the job hit the
/// permanent-failure threshold and was flipped to `failed` status.
pub(crate) fn increment_push_outbox_dead_lettered() {
    PUSH_OUTBOX_DEAD_LETTERED.fetch_add(1, Ordering::Relaxed);
}

/// Increment the `waddle_dnd_projection_read_errored_total` counter.
///
/// Bumped by `crate::dnd_reader::PepDndReader::dnd_state` whenever
/// the projection read fails and the recipient is defaulted to
/// `Inactive` — i.e. a user who was in DND silently becomes
/// un-DND'd at the push gate. Non-zero rate is alert-worthy.
pub(crate) fn increment_dnd_projection_read_errored() {
    DND_PROJECTION_READ_ERRORED.fetch_add(1, Ordering::Relaxed);
}

/// Increment the `waddle_push_suppressed_total{reason}` counter.
///
/// The typed reason indexes a fixed atomic slot directly: no allocation,
/// lookup, or unbounded label can enter the legacy renderer.
pub(crate) fn increment_push_suppressed(reason: PushSuppressReason) {
    PUSH_SUPPRESSED_COUNTERS[reason.index()].fetch_add(1, Ordering::Relaxed);
}

/// Snapshot of every push-suppressed counter for rendering.
fn render_push_suppressed_lines(out: &mut String) {
    out.push_str("# HELP waddle_push_suppressed_total XEP-0357 push notification candidates suppressed by a XEP/Waddle rule. Labeled by the typed `SuppressedReason` enum.\n");
    out.push_str("# TYPE waddle_push_suppressed_total counter\n");
    for (idx, reason) in PUSH_SUPPRESSED_REASONS.iter().enumerate() {
        let value = PUSH_SUPPRESSED_COUNTERS[idx].load(Ordering::Relaxed);
        out.push_str("waddle_push_suppressed_total{reason=\"");
        out.push_str(reason);
        out.push_str("\"} ");
        out.push_str(&value.to_string());
        out.push('\n');
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub fn reset_metrics_for_test() {
    CONNECTED_USERS.store(0, Ordering::Release);
    ROOM_COUNT.store(0, Ordering::Release);
    MESSAGES_TOTAL.store(0, Ordering::Release);
    CURRENT_SECOND.store(0, Ordering::Release);
    CURRENT_SECOND_MESSAGES.store(0, Ordering::Release);
    LAST_SECOND_MESSAGES.store(0, Ordering::Release);
    BROADCAST_DELIVERED.store(0, Ordering::Release);
    BROADCAST_NOT_CONNECTED.store(0, Ordering::Release);
    BROADCAST_DROPPED_FULL.store(0, Ordering::Release);
    BROADCAST_DROPPED_CLOSED.store(0, Ordering::Release);
    DELIVERY_TERMINAL_ERROR_DROP.store(0, Ordering::Release);
    DELIVERY_RETRY_EXHAUSTED_DROP.store(0, Ordering::Release);
    RESOLVER_AFFILIATION_SYNC_CAPACITY_DROP.store(0, Ordering::Release);
    USER_ACTOR_REAPED.store(0, Ordering::Release);
    SM_UNACKED_EVICTED.store(0, Ordering::Release);
    PENDING_DELIVERY_QUOTA_EXCEEDED.store(0, Ordering::Release);
    PENDING_DELIVERY_ORPHAN_CLAIMS_RELEASED.store(0, Ordering::Release);
    PENDING_DELIVERY_AGED_OUT.store(0, Ordering::Release);
    PENDING_DELIVERY_UNRESOLVED_POISON_PILL.store(0, Ordering::Release);
    PENDING_DELIVERY_ARCHIVE_LOOKUP_TRANSIENT_FAILURE.store(0, Ordering::Release);
    SM_PROMOTION_STORAGE_FAILED.store(0, Ordering::Release);
    SM_PROMOTION_NOT_PROMOTABLE.store(0, Ordering::Release);
    SM_PROMOTION_BLOCKLIST_FAILED.store(0, Ordering::Release);
    SM_PROMOTION_DEAD_LETTERED.store(0, Ordering::Release);
    SM_DRAIN_TIMEOUT.store(0, Ordering::Release);
    SM_RESUME_WINDOW_CLAMPED.store(0, Ordering::Release);
    SM_SEND_WINDOW_PAUSES.store(0, Ordering::Release);
    SM_SEND_WINDOW_PAUSE_TIMEOUTS.store(0, Ordering::Release);
    SM_DETACHED_UNACKED_EVICTED.store(0, Ordering::Release);
    PENDING_FLUSH_BATCHES.store(0, Ordering::Release);
    PENDING_FLUSH_ROWS_PUSHED.store(0, Ordering::Release);
    for counter in PUSH_SUPPRESSED_COUNTERS.iter() {
        counter.store(0, Ordering::Release);
    }
    PUSH_CANDIDATE_CREATED.store(0, Ordering::Release);
    PUSH_CANDIDATE_COALESCED.store(0, Ordering::Release);
    PUSH_OUTBOX_PUBLISHED.store(0, Ordering::Release);
    PUSH_OUTBOX_RETRY_SCHEDULED.store(0, Ordering::Release);
    PUSH_OUTBOX_DEAD_LETTERED.store(0, Ordering::Release);
    DND_PROJECTION_READ_ERRORED.store(0, Ordering::Release);
}

pub fn render_metrics() -> String {
    let now = unix_timestamp_secs();
    rotate_second_bucket(now);

    let connected_users = CONNECTED_USERS.load(Ordering::Acquire);
    let room_count = ROOM_COUNT.load(Ordering::Acquire);
    let messages_total = MESSAGES_TOTAL.load(Ordering::Acquire);
    let messages_per_second = LAST_SECOND_MESSAGES.load(Ordering::Acquire);
    let broadcast_delivered = BROADCAST_DELIVERED.load(Ordering::Relaxed);
    let broadcast_not_connected = BROADCAST_NOT_CONNECTED.load(Ordering::Relaxed);
    let broadcast_dropped_full = BROADCAST_DROPPED_FULL.load(Ordering::Relaxed);
    let broadcast_dropped_closed = BROADCAST_DROPPED_CLOSED.load(Ordering::Relaxed);
    let delivery_terminal_error_drop = DELIVERY_TERMINAL_ERROR_DROP.load(Ordering::Relaxed);
    let delivery_retry_exhausted_drop = DELIVERY_RETRY_EXHAUSTED_DROP.load(Ordering::Relaxed);
    let resolver_affiliation_sync_capacity_drop =
        RESOLVER_AFFILIATION_SYNC_CAPACITY_DROP.load(Ordering::Relaxed);
    let user_actor_reaped = USER_ACTOR_REAPED.load(Ordering::Relaxed);
    let sm_unacked_evicted = SM_UNACKED_EVICTED.load(Ordering::Relaxed);
    let pending_quota_exceeded = PENDING_DELIVERY_QUOTA_EXCEEDED.load(Ordering::Relaxed);
    let pending_orphan_released = PENDING_DELIVERY_ORPHAN_CLAIMS_RELEASED.load(Ordering::Relaxed);
    let pending_aged_out = PENDING_DELIVERY_AGED_OUT.load(Ordering::Relaxed);
    let pending_poison_pill = PENDING_DELIVERY_UNRESOLVED_POISON_PILL.load(Ordering::Relaxed);
    let pending_archive_lookup_transient =
        PENDING_DELIVERY_ARCHIVE_LOOKUP_TRANSIENT_FAILURE.load(Ordering::Relaxed);
    let sm_promotion_storage_failed = SM_PROMOTION_STORAGE_FAILED.load(Ordering::Relaxed);
    let sm_promotion_not_promotable = SM_PROMOTION_NOT_PROMOTABLE.load(Ordering::Relaxed);
    let sm_promotion_blocklist_failed = SM_PROMOTION_BLOCKLIST_FAILED.load(Ordering::Relaxed);
    let sm_promotion_dead_lettered = SM_PROMOTION_DEAD_LETTERED.load(Ordering::Relaxed);
    let sm_drain_timeout = SM_DRAIN_TIMEOUT.load(Ordering::Relaxed);
    let sm_resume_window_clamped = SM_RESUME_WINDOW_CLAMPED.load(Ordering::Relaxed);
    let sm_send_window_pauses = SM_SEND_WINDOW_PAUSES.load(Ordering::Relaxed);
    let sm_send_window_pause_timeouts = SM_SEND_WINDOW_PAUSE_TIMEOUTS.load(Ordering::Relaxed);
    let sm_detached_unacked_evicted = SM_DETACHED_UNACKED_EVICTED.load(Ordering::Relaxed);
    let pending_flush_batches = PENDING_FLUSH_BATCHES.load(Ordering::Relaxed);
    let pending_flush_rows_pushed = PENDING_FLUSH_ROWS_PUSHED.load(Ordering::Relaxed);
    let push_candidate_created = PUSH_CANDIDATE_CREATED.load(Ordering::Relaxed);
    let push_candidate_coalesced = PUSH_CANDIDATE_COALESCED.load(Ordering::Relaxed);
    let push_outbox_published = PUSH_OUTBOX_PUBLISHED.load(Ordering::Relaxed);
    let push_outbox_retry_scheduled = PUSH_OUTBOX_RETRY_SCHEDULED.load(Ordering::Relaxed);
    let push_outbox_dead_lettered = PUSH_OUTBOX_DEAD_LETTERED.load(Ordering::Relaxed);
    let dnd_projection_read_errored = DND_PROJECTION_READ_ERRORED.load(Ordering::Relaxed);

    format!(
        concat!(
            "# HELP waddle_connected_users Currently connected users.\n",
            "# TYPE waddle_connected_users gauge\n",
            "waddle_connected_users {connected_users}\n",
            "# HELP waddle_room_count Active MUC room count.\n",
            "# TYPE waddle_room_count gauge\n",
            "waddle_room_count {room_count}\n",
            "# HELP waddle_messages_total Total processed message stanzas.\n",
            "# TYPE waddle_messages_total counter\n",
            "waddle_messages_total {messages_total}\n",
            "# HELP waddle_messages_per_second Processed message stanzas in the last full second.\n",
            "# TYPE waddle_messages_per_second gauge\n",
            "waddle_messages_per_second {messages_per_second}\n",
            "# HELP waddle_broadcast_delivered_total Non-blocking broadcast attempts that enqueued on the recipient's outbound channel.\n",
            "# TYPE waddle_broadcast_delivered_total counter\n",
            "waddle_broadcast_delivered_total {broadcast_delivered}\n",
            "# HELP waddle_broadcast_not_connected_total Non-blocking broadcast attempts that found no registry entry for the recipient.\n",
            "# TYPE waddle_broadcast_not_connected_total counter\n",
            "waddle_broadcast_not_connected_total {broadcast_not_connected}\n",
            "# HELP waddle_broadcast_dropped_full_total Non-blocking broadcast attempts dropped because the recipient's outbound channel was full.\n",
            "# TYPE waddle_broadcast_dropped_full_total counter\n",
            "waddle_broadcast_dropped_full_total {broadcast_dropped_full}\n",
            "# HELP waddle_broadcast_dropped_closed_total Non-blocking broadcast attempts dropped because the recipient's outbound channel was closed.\n",
            "# TYPE waddle_broadcast_dropped_closed_total counter\n",
            "waddle_broadcast_dropped_closed_total {broadcast_dropped_closed}\n",
            "# HELP waddle_delivery_terminal_error_drop_total Actor-path deliveries dropped after a terminal ask failure whose message may have been enqueued (ActorStopped / reply Timeout(None)); dropped instead of routed to the XEP-0198 detached buffer to avoid double-delivery.\n",
            "# TYPE waddle_delivery_terminal_error_drop_total counter\n",
            "waddle_delivery_terminal_error_drop_total {delivery_terminal_error_drop}\n",
            "# HELP waddle_delivery_retry_exhausted_drop_total Frames dropped because the recipient's outbound channel was STILL full after the bounded in-line DroppedFull retries (#1263) — groupchat reflections, MUC presence fan-out, and actor-path full-JID deliveries. A non-zero value means a live recipient missed a stanza under sustained backpressure.\n",
            "# TYPE waddle_delivery_retry_exhausted_drop_total counter\n",
            "waddle_delivery_retry_exhausted_drop_total {delivery_retry_exhausted_drop}\n",
            "# HELP waddle_resolver_affiliation_sync_capacity_drop_total Rejected-join resolver-affiliation repairs dropped because the bounded scheduler had no free worker slot. The join decision remains authoritative, but the live room affiliation projection may stay stale until a later repair or eviction.\n",
            "# TYPE waddle_resolver_affiliation_sync_capacity_drop_total counter\n",
            "waddle_resolver_affiliation_sync_capacity_drop_total {resolver_affiliation_sync_capacity_drop}\n",
            "# HELP waddle_user_actor_reaped_total Empty UserActors reaped by the periodic reaper after delivery-path closed-channel eviction removed their last resource without the explicit unregister-prune path running.\n",
            "# TYPE waddle_user_actor_reaped_total counter\n",
            "waddle_user_actor_reaped_total {user_actor_reaped}\n",
            "# HELP waddle_sm_unacked_evicted_total XEP-0198 unacked-queue entries evicted because the queue hit capacity; older resume h values must fail instead of receiving an incomplete replay.\n",
            "# TYPE waddle_sm_unacked_evicted_total counter\n",
            "waddle_sm_unacked_evicted_total {sm_unacked_evicted}\n",
            "# HELP waddle_pending_delivery_quota_exceeded_total Inserts rejected because the per-recipient pending_delivery quota was full (XEP-0160 §3 step 3 bounce path).\n",
            "# TYPE waddle_pending_delivery_quota_exceeded_total counter\n",
            "waddle_pending_delivery_quota_exceeded_total {pending_quota_exceeded}\n",
            "# HELP waddle_pending_delivery_orphan_claims_released_total Pending_delivery rows the claim-expiry janitor released because their session was no longer live.\n",
            "# TYPE waddle_pending_delivery_orphan_claims_released_total counter\n",
            "waddle_pending_delivery_orphan_claims_released_total {pending_orphan_released}\n",
            "# HELP waddle_pending_delivery_aged_out_total Pending_delivery rows the aging janitor dropped because they exceeded WADDLE_PENDING_DELIVERY_MAX_AGE_DAYS.\n",
            "# TYPE waddle_pending_delivery_aged_out_total counter\n",
            "waddle_pending_delivery_aged_out_total {pending_aged_out}\n",
            "# HELP waddle_pending_delivery_unresolved_poison_pill_total Pending_delivery flushes that dropped a row because its MAM payload could not be resolved (corruption signal).\n",
            "# TYPE waddle_pending_delivery_unresolved_poison_pill_total counter\n",
            "waddle_pending_delivery_unresolved_poison_pill_total {pending_poison_pill}\n",
            "# HELP waddle_pending_delivery_archive_lookup_transient_failure_total Pending_delivery flushes aborted by a transient MAM storage error resolving an Archived row: the failing row and the rest of the claimed batch are released (FIFO preserved) and the client's next presence update retries (issue #1122). Signals MAM storage availability problems, not corruption; no mail is lost.\n",
            "# TYPE waddle_pending_delivery_archive_lookup_transient_failure_total counter\n",
            "waddle_pending_delivery_archive_lookup_transient_failure_total {pending_archive_lookup_transient}\n",
            "# HELP waddle_sm_promotion_storage_failed_total Q6 promotion encountered a transient pending_delivery insert error; durable SM row preserved for retry.\n",
            "# TYPE waddle_sm_promotion_storage_failed_total counter\n",
            "waddle_sm_promotion_storage_failed_total {sm_promotion_storage_failed}\n",
            "# HELP waddle_sm_promotion_not_promotable_total Q6 promotion skipped a valid stanza that must not enter XEP-0160 offline storage, such as XEP-0313 MAM result/fin frames.\n",
            "# TYPE waddle_sm_promotion_not_promotable_total counter\n",
            "waddle_sm_promotion_not_promotable_total {sm_promotion_not_promotable}\n",
            "# HELP waddle_sm_promotion_blocklist_failed_total Q6 promotion skipped a session because its blocklist load failed (fail-closed XEP-0191 policy).\n",
            "# TYPE waddle_sm_promotion_blocklist_failed_total counter\n",
            "waddle_sm_promotion_blocklist_failed_total {sm_promotion_blocklist_failed}\n",
            "# HELP waddle_sm_promotion_dead_lettered_total Q6 promotion failed WADDLE_SM_PROMOTION_MAX_ATTEMPTS times in a row for a session; durable row deleted to break the retry loop. Each event is a permanent loss of unacked stanzas.\n",
            "# TYPE waddle_sm_promotion_dead_lettered_total counter\n",
            "waddle_sm_promotion_dead_lettered_total {sm_promotion_dead_lettered}\n",
            "# HELP waddle_sm_drain_timeout_total Graceful-shutdown drain hit WADDLE_DRAIN_TIMEOUT_SECS with sessions still pending; remaining durable rows survive for restart-time retry.\n",
            "# TYPE waddle_sm_drain_timeout_total counter\n",
            "waddle_sm_drain_timeout_total {sm_drain_timeout}\n",
            "# HELP waddle_sm_resume_window_clamped_total Client-requested XEP-0198 resume window exceeded WADDLE_SM_MAX_RESUME_SECS and was silently lowered.\n",
            "# TYPE waddle_sm_resume_window_clamped_total counter\n",
            "waddle_sm_resume_window_clamped_total {sm_resume_window_clamped}\n",
            "# HELP waddle_sm_send_window_pauses_total XEP-0198 send-window pauses engaged by a wire-write path to avoid overflowing the unacked queue (issue #1219). Healthy non-zero under burst — pacing INSTEAD of evicting.\n",
            "# TYPE waddle_sm_send_window_pauses_total counter\n",
            "waddle_sm_send_window_pauses_total {sm_send_window_pauses}\n",
            "# HELP waddle_sm_send_window_pause_timeouts_total Send-window pauses that outlived their deadline with no recovering ack; the connection closed into detach-for-resume with a capped replay queue (issue #1219).\n",
            "# TYPE waddle_sm_send_window_pause_timeouts_total counter\n",
            "waddle_sm_send_window_pause_timeouts_total {sm_send_window_pause_timeouts}\n",
            "# HELP waddle_sm_detached_unacked_evicted_total Entries evicted from a DETACHED session's unacked queue at capacity while awaiting resume; a resume with an older h for that session must fail rather than replay an incomplete window (issue #1219).\n",
            "# TYPE waddle_sm_detached_unacked_evicted_total counter\n",
            "waddle_sm_detached_unacked_evicted_total {sm_detached_unacked_evicted}\n",
            "# HELP waddle_pending_flush_batches_total XEP-0160 offline-flush claim_batch_for_session batches drained across all flushes (issue #1220).\n",
            "# TYPE waddle_pending_flush_batches_total counter\n",
            "waddle_pending_flush_batches_total {pending_flush_batches}\n",
            "# HELP waddle_pending_flush_rows_pushed_total XEP-0160 offline-flush replay stanzas pushed to recovering resources (issue #1220).\n",
            "# TYPE waddle_pending_flush_rows_pushed_total counter\n",
            "waddle_pending_flush_rows_pushed_total {pending_flush_rows_pushed}\n",
            "# HELP waddle_push_candidate_created_total XEP-0357 notification candidate rows inserted into `notification_candidates` (the `Inserted` arm of `insert_candidate`). T0-suppressed candidates never reach insert_candidate, so they do NOT bump this counter — only `waddle_push_suppressed_total{{reason}}`. T1-suppressed candidates (the race-window guard re-evaluation in `drain_pending_candidates_into_outbox`) DO bump this counter at T0 AND `waddle_push_suppressed_total` at T1. Reconcile against published_total + suppressed_total over a window.\n",
            "# TYPE waddle_push_candidate_created_total counter\n",
            "waddle_push_candidate_created_total {push_candidate_created}\n",
            "# HELP waddle_push_candidate_coalesced_total XEP-0357 notification candidate insertions that hit the existing PRIMARY KEY (the `Duplicate` arm of `insert_candidate`). A sustained non-zero is normal retry/replay traffic; a spike often signals an upstream retry loop.\n",
            "# TYPE waddle_push_candidate_coalesced_total counter\n",
            "waddle_push_candidate_coalesced_total {push_candidate_coalesced}\n",
            "# HELP waddle_push_outbox_published_total XEP-0357 outbox jobs whose `<iq type='set'><pubsub><publish/></pubsub></iq>` to the Push Service node was accepted (the corresponding `push_publish_jobs` row is created — XEP-0060 §7.1 publish success at the XMPP boundary). Per-provider fanout (Web/APNs/FCM) and provider acknowledgement are observed separately by the counters landing alongside #528/#529/#530; this counter stops at the XMPP layer.\n",
            "# TYPE waddle_push_outbox_published_total counter\n",
            "waddle_push_outbox_published_total {push_outbox_published}\n",
            "# HELP waddle_push_outbox_retry_scheduled_total XEP-0357 outbox jobs that failed transiently and were requeued with a backoff. Sustained non-zero with flat published_total indicates the Push Service boundary is wedged. Labeled by the typed transient-failure reason; the closed-set values land alongside the provider slices in #528/#529/#530 (`5xx`, `timeout`, `auth`, `unknown`) — today the bucket is single `reason=\"unknown\"` so PromQL alerts written now match all future variants.\n",
            "# TYPE waddle_push_outbox_retry_scheduled_total counter\n",
            "waddle_push_outbox_retry_scheduled_total{{reason=\"unknown\"}} {push_outbox_retry_scheduled}\n",
            "# HELP waddle_push_outbox_dead_lettered_total XEP-0357 outbox jobs that transitioned to the terminal `failed` status — `NotificationOutboxPublishOutcome::Failed`. Covers both retry-budget exhaustion AND immediate hard-failure branches (non-first-party Push Service target, XEP-0191 blocked sender, missing XEP-0357 registration). Investigate sustained non-zero rate; isolated dead-letters are expected during provider-side device revocation (APNs `Unregistered` / FCM `UNREGISTERED` device flows). The outbox row stays for post-mortem and no further retry will run.\n",
            "# TYPE waddle_push_outbox_dead_lettered_total counter\n",
            "waddle_push_outbox_dead_lettered_total {push_outbox_dead_lettered}\n",
            "# HELP waddle_dnd_projection_read_errored_total Recipient DND-projection reads that errored at the T1 push gate and silently defaulted the recipient to Inactive — a DND-active user receiving push notifications they explicitly opted out of. Alert-worthy on any sustained non-zero rate. Source: `dnd_reader::PepDndReader::dnd_state` failure path.\n",
            "# TYPE waddle_dnd_projection_read_errored_total counter\n",
            "waddle_dnd_projection_read_errored_total {dnd_projection_read_errored}\n",
            "{push_suppressed_lines}",
        ),
        connected_users = connected_users,
        room_count = room_count,
        messages_total = messages_total,
        messages_per_second = messages_per_second,
        broadcast_delivered = broadcast_delivered,
        broadcast_not_connected = broadcast_not_connected,
        broadcast_dropped_full = broadcast_dropped_full,
        broadcast_dropped_closed = broadcast_dropped_closed,
        delivery_terminal_error_drop = delivery_terminal_error_drop,
        delivery_retry_exhausted_drop = delivery_retry_exhausted_drop,
        resolver_affiliation_sync_capacity_drop = resolver_affiliation_sync_capacity_drop,
        user_actor_reaped = user_actor_reaped,
        sm_unacked_evicted = sm_unacked_evicted,
        pending_quota_exceeded = pending_quota_exceeded,
        pending_orphan_released = pending_orphan_released,
        pending_aged_out = pending_aged_out,
        pending_poison_pill = pending_poison_pill,
        pending_archive_lookup_transient = pending_archive_lookup_transient,
        sm_promotion_storage_failed = sm_promotion_storage_failed,
        sm_promotion_not_promotable = sm_promotion_not_promotable,
        sm_promotion_blocklist_failed = sm_promotion_blocklist_failed,
        sm_promotion_dead_lettered = sm_promotion_dead_lettered,
        sm_drain_timeout = sm_drain_timeout,
        sm_resume_window_clamped = sm_resume_window_clamped,
        sm_send_window_pauses = sm_send_window_pauses,
        sm_send_window_pause_timeouts = sm_send_window_pause_timeouts,
        sm_detached_unacked_evicted = sm_detached_unacked_evicted,
        pending_flush_batches = pending_flush_batches,
        pending_flush_rows_pushed = pending_flush_rows_pushed,
        push_candidate_created = push_candidate_created,
        push_candidate_coalesced = push_candidate_coalesced,
        push_outbox_published = push_outbox_published,
        push_outbox_retry_scheduled = push_outbox_retry_scheduled,
        push_outbox_dead_lettered = push_outbox_dead_lettered,
        dnd_projection_read_errored = dnd_projection_read_errored,
        push_suppressed_lines = {
            let mut buf = String::new();
            render_push_suppressed_lines(&mut buf);
            buf
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_decrement_saturates_at_zero() {
        let _guard = metrics_test_lock().lock().await;
        reset_metrics_for_test();

        decrement_connected_users();
        decrement_room_count();

        assert_eq!(CONNECTED_USERS.load(Ordering::Acquire), 0);
        assert_eq!(ROOM_COUNT.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn test_increment_and_decrement_round_trip() {
        let _guard = metrics_test_lock().lock().await;
        reset_metrics_for_test();

        increment_connected_users();
        increment_connected_users();
        decrement_connected_users();

        increment_room_count();
        decrement_room_count();

        assert_eq!(CONNECTED_USERS.load(Ordering::Acquire), 1);
        assert_eq!(ROOM_COUNT.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn test_rotate_second_bucket_moves_current_to_last() {
        let _guard = metrics_test_lock().lock().await;
        reset_metrics_for_test();

        CURRENT_SECOND.store(100, Ordering::Release);
        CURRENT_SECOND_MESSAGES.store(7, Ordering::Release);

        rotate_second_bucket(101);

        assert_eq!(CURRENT_SECOND.load(Ordering::Acquire), 101);
        assert_eq!(CURRENT_SECOND_MESSAGES.load(Ordering::Acquire), 0);
        assert_eq!(LAST_SECOND_MESSAGES.load(Ordering::Acquire), 7);
    }

    #[tokio::test]
    async fn test_render_metrics_contains_expected_families() {
        let _guard = metrics_test_lock().lock().await;
        reset_metrics_for_test();

        increment_connected_users();
        increment_room_count();
        record_message_processed();

        let rendered = render_metrics();

        assert!(rendered.contains("# HELP waddle_connected_users"));
        assert!(rendered.contains("# TYPE waddle_connected_users gauge"));
        assert!(rendered.contains("# HELP waddle_room_count"));
        assert!(rendered.contains("# TYPE waddle_room_count gauge"));
        assert!(rendered.contains("# HELP waddle_messages_total"));
        assert!(rendered.contains("# TYPE waddle_messages_total counter"));
        assert!(rendered.contains("# HELP waddle_messages_per_second"));
        assert!(rendered.contains("# TYPE waddle_messages_per_second gauge"));
        assert!(rendered.contains("waddle_connected_users 1"));
        assert!(rendered.contains("waddle_room_count 1"));
        assert!(rendered.contains("waddle_messages_total 1"));
    }

    #[tokio::test]
    async fn test_broadcast_counters_increment_and_render() {
        let _guard = metrics_test_lock().lock().await;
        reset_metrics_for_test();

        increment_broadcast_delivered();
        increment_broadcast_delivered();
        increment_broadcast_not_connected();
        increment_broadcast_dropped_full();
        increment_broadcast_dropped_full();
        increment_broadcast_dropped_full();
        increment_broadcast_dropped_closed();

        let rendered = render_metrics();

        assert!(rendered.contains("# TYPE waddle_broadcast_delivered_total counter"));
        assert!(rendered.contains("waddle_broadcast_delivered_total 2"));
        assert!(rendered.contains("waddle_broadcast_not_connected_total 1"));
        assert!(rendered.contains("waddle_broadcast_dropped_full_total 3"));
        assert!(rendered.contains("waddle_broadcast_dropped_closed_total 1"));
    }

    /// Issue #209 finding #11: every metric introduced for the
    /// offline-DM / SM-expiry surface MUST appear in the rendered
    /// output with HELP+TYPE headers. Without these headers, a
    /// scraper accepts the line but dashboards lose the metric type.
    #[tokio::test]
    async fn test_issue_209_finding_11_metric_families_render() {
        let _guard = metrics_test_lock().lock().await;
        reset_metrics_for_test();

        increment_pending_delivery_quota_exceeded();
        add_pending_delivery_orphan_claims_released(7);
        add_pending_delivery_aged_out(3);
        increment_pending_delivery_unresolved_poison_pill();
        increment_pending_delivery_archive_lookup_transient_failure();
        add_sm_promotion_storage_failed(2);
        increment_sm_promotion_not_promotable();
        increment_sm_promotion_blocklist_failed();
        increment_sm_promotion_dead_lettered();
        increment_sm_drain_timeout();
        increment_sm_resume_window_clamped();

        let rendered = render_metrics();

        for family in [
            "waddle_pending_delivery_quota_exceeded_total",
            "waddle_pending_delivery_orphan_claims_released_total",
            "waddle_pending_delivery_aged_out_total",
            "waddle_pending_delivery_unresolved_poison_pill_total",
            "waddle_pending_delivery_archive_lookup_transient_failure_total",
            "waddle_sm_promotion_storage_failed_total",
            "waddle_sm_promotion_not_promotable_total",
            "waddle_sm_promotion_blocklist_failed_total",
            "waddle_sm_promotion_dead_lettered_total",
            "waddle_sm_drain_timeout_total",
            "waddle_sm_resume_window_clamped_total",
        ] {
            assert!(
                rendered.contains(&format!("# HELP {family}")),
                "missing HELP header for {family}"
            );
            assert!(
                rendered.contains(&format!("# TYPE {family} counter")),
                "missing TYPE header for {family}"
            );
        }
        assert!(rendered.contains("waddle_pending_delivery_quota_exceeded_total 1"));
        assert!(rendered.contains("waddle_pending_delivery_orphan_claims_released_total 7"));
        assert!(rendered.contains("waddle_pending_delivery_aged_out_total 3"));
        assert!(
            rendered.contains("waddle_pending_delivery_archive_lookup_transient_failure_total 1")
        );
        assert!(rendered.contains("waddle_sm_promotion_storage_failed_total 2"));
        assert!(rendered.contains("waddle_sm_promotion_not_promotable_total 1"));
        assert!(rendered.contains("waddle_sm_resume_window_clamped_total 1"));
    }

    #[tokio::test]
    async fn test_send_window_and_pending_flush_counters_render() {
        // Issue #1219 / #1220 observability families.
        let _guard = metrics_test_lock().lock().await;
        reset_metrics_for_test();

        increment_sm_send_window_pause();
        increment_sm_send_window_pause();
        increment_sm_send_window_pause_timeout();
        increment_sm_detached_unacked_evicted();
        add_pending_flush_batches(3);
        add_pending_flush_rows_pushed(42);

        let rendered = render_metrics();
        for family in [
            "waddle_sm_send_window_pauses_total",
            "waddle_sm_send_window_pause_timeouts_total",
            "waddle_sm_detached_unacked_evicted_total",
            "waddle_pending_flush_batches_total",
            "waddle_pending_flush_rows_pushed_total",
        ] {
            assert!(
                rendered.contains(&format!("# TYPE {family} counter")),
                "missing TYPE line for {family}"
            );
        }
        assert!(rendered.contains("waddle_sm_send_window_pauses_total 2"));
        assert!(rendered.contains("waddle_sm_send_window_pause_timeouts_total 1"));
        assert!(rendered.contains("waddle_sm_detached_unacked_evicted_total 1"));
        assert!(rendered.contains("waddle_pending_flush_batches_total 3"));
        assert!(rendered.contains("waddle_pending_flush_rows_pushed_total 42"));

        // reset clears them.
        reset_metrics_for_test();
        let cleared = render_metrics();
        assert!(cleared.contains("waddle_sm_send_window_pauses_total 0"));
        assert!(cleared.contains("waddle_pending_flush_rows_pushed_total 0"));
    }

    #[tokio::test]
    async fn test_reset_metrics_for_test_clears_sm_promotion_not_promotable() {
        let _guard = metrics_test_lock().lock().await;
        reset_metrics_for_test();

        increment_sm_promotion_not_promotable();
        reset_metrics_for_test();

        let rendered = render_metrics();
        assert!(rendered.contains("waddle_sm_promotion_not_promotable_total 0"));
    }

    #[tokio::test]
    async fn test_sm_unacked_evicted_counter_increments_and_renders() {
        let _guard = metrics_test_lock().lock().await;
        reset_metrics_for_test();

        increment_sm_unacked_evicted();
        increment_sm_unacked_evicted();

        let rendered = render_metrics();
        assert!(rendered.contains("# TYPE waddle_sm_unacked_evicted_total counter"));
        assert!(rendered.contains("waddle_sm_unacked_evicted_total 2"));
    }

    /// #531 push-pipeline observability: each of the five non-
    /// provider counters MUST surface a HELP+TYPE header and the
    /// running total in the metrics render. The constants in
    /// `notification_outbox.rs` increment these at the exact
    /// pipeline boundary names the HELP text claims (`insert_candidate`
    /// Inserted/Duplicate arms, `drain_due_outbox_jobs` outcome
    /// arms); this test pins the render side.
    #[tokio::test]
    async fn test_push_pipeline_counters_increment_and_render() {
        let _guard = metrics_test_lock().lock().await;
        reset_metrics_for_test();

        increment_push_candidate_created();
        increment_push_candidate_created();
        increment_push_candidate_created();
        increment_push_candidate_coalesced();
        increment_push_outbox_published();
        increment_push_outbox_published();
        increment_push_outbox_retry_scheduled();
        increment_push_outbox_dead_lettered();

        let rendered = render_metrics();

        // HELP + TYPE headers — without these a Prometheus scraper
        // accepts the lines but dashboards drop the metric type.
        for header in [
            "# HELP waddle_push_candidate_created_total",
            "# TYPE waddle_push_candidate_created_total counter",
            "# HELP waddle_push_candidate_coalesced_total",
            "# TYPE waddle_push_candidate_coalesced_total counter",
            "# HELP waddle_push_outbox_published_total",
            "# TYPE waddle_push_outbox_published_total counter",
            "# HELP waddle_push_outbox_retry_scheduled_total",
            "# TYPE waddle_push_outbox_retry_scheduled_total counter",
            "# HELP waddle_push_outbox_dead_lettered_total",
            "# TYPE waddle_push_outbox_dead_lettered_total counter",
        ] {
            assert!(
                rendered.contains(header),
                "metrics render missing `{header}`: {rendered}"
            );
        }

        // Running totals — three created, one coalesced, two
        // published, one retry-scheduled (labeled today as
        // `reason="unknown"` to forward-compat the closed-set
        // labeling planned alongside #528/#529/#530), one
        // dead-lettered.
        for line in [
            "waddle_push_candidate_created_total 3",
            "waddle_push_candidate_coalesced_total 1",
            "waddle_push_outbox_published_total 2",
            "waddle_push_outbox_retry_scheduled_total{reason=\"unknown\"} 1",
            "waddle_push_outbox_dead_lettered_total 1",
        ] {
            assert!(
                rendered.contains(line),
                "metrics render missing line `{line}`: {rendered}"
            );
        }
    }
}
