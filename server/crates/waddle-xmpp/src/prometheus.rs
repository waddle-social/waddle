//! Lightweight Prometheus exporter for core XMPP runtime metrics.
//!
//! This module tracks a small set of process-level metrics required for
//! operational health dashboards and exposes them in Prometheus text format.

use std::sync::atomic::{AtomicU64, Ordering};

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

// XEP-0198 unacked-queue evictions (see `stream_management::UnackedQueue`).
// A non-zero counter means at least one stanza was evicted from an SM
// session's replay buffer while that session was still resumable — a
// later `<resumed/>` will silently drop that stanza.
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

pub fn increment_broadcast_delivered() {
    BROADCAST_DELIVERED.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_broadcast_not_connected() {
    BROADCAST_NOT_CONNECTED.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_broadcast_dropped_full() {
    BROADCAST_DROPPED_FULL.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_broadcast_dropped_closed() {
    BROADCAST_DROPPED_CLOSED.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_sm_unacked_evicted() {
    SM_UNACKED_EVICTED.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_pending_delivery_quota_exceeded() {
    PENDING_DELIVERY_QUOTA_EXCEEDED.fetch_add(1, Ordering::Relaxed);
}

pub fn add_pending_delivery_orphan_claims_released(n: u64) {
    PENDING_DELIVERY_ORPHAN_CLAIMS_RELEASED.fetch_add(n, Ordering::Relaxed);
}

pub fn add_pending_delivery_aged_out(n: u64) {
    PENDING_DELIVERY_AGED_OUT.fetch_add(n, Ordering::Relaxed);
}

pub fn increment_pending_delivery_unresolved_poison_pill() {
    PENDING_DELIVERY_UNRESOLVED_POISON_PILL.fetch_add(1, Ordering::Relaxed);
}

pub fn add_sm_promotion_storage_failed(n: u64) {
    SM_PROMOTION_STORAGE_FAILED.fetch_add(n, Ordering::Relaxed);
}

pub fn increment_sm_promotion_not_promotable() {
    SM_PROMOTION_NOT_PROMOTABLE.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_sm_promotion_blocklist_failed() {
    SM_PROMOTION_BLOCKLIST_FAILED.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_sm_promotion_dead_lettered() {
    SM_PROMOTION_DEAD_LETTERED.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_sm_drain_timeout() {
    SM_DRAIN_TIMEOUT.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_sm_resume_window_clamped() {
    SM_RESUME_WINDOW_CLAMPED.fetch_add(1, Ordering::Relaxed);
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
    let sm_unacked_evicted = SM_UNACKED_EVICTED.load(Ordering::Relaxed);
    let pending_quota_exceeded = PENDING_DELIVERY_QUOTA_EXCEEDED.load(Ordering::Relaxed);
    let pending_orphan_released = PENDING_DELIVERY_ORPHAN_CLAIMS_RELEASED.load(Ordering::Relaxed);
    let pending_aged_out = PENDING_DELIVERY_AGED_OUT.load(Ordering::Relaxed);
    let pending_poison_pill = PENDING_DELIVERY_UNRESOLVED_POISON_PILL.load(Ordering::Relaxed);
    let sm_promotion_storage_failed = SM_PROMOTION_STORAGE_FAILED.load(Ordering::Relaxed);
    let sm_promotion_not_promotable = SM_PROMOTION_NOT_PROMOTABLE.load(Ordering::Relaxed);
    let sm_promotion_blocklist_failed = SM_PROMOTION_BLOCKLIST_FAILED.load(Ordering::Relaxed);
    let sm_promotion_dead_lettered = SM_PROMOTION_DEAD_LETTERED.load(Ordering::Relaxed);
    let sm_drain_timeout = SM_DRAIN_TIMEOUT.load(Ordering::Relaxed);
    let sm_resume_window_clamped = SM_RESUME_WINDOW_CLAMPED.load(Ordering::Relaxed);

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
            "# HELP waddle_sm_unacked_evicted_total XEP-0198 unacked-queue entries evicted because the queue hit capacity; each eviction will be missing from a later <resumed/> replay.\n",
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
        ),
        connected_users = connected_users,
        room_count = room_count,
        messages_total = messages_total,
        messages_per_second = messages_per_second,
        broadcast_delivered = broadcast_delivered,
        broadcast_not_connected = broadcast_not_connected,
        broadcast_dropped_full = broadcast_dropped_full,
        broadcast_dropped_closed = broadcast_dropped_closed,
        sm_unacked_evicted = sm_unacked_evicted,
        pending_quota_exceeded = pending_quota_exceeded,
        pending_orphan_released = pending_orphan_released,
        pending_aged_out = pending_aged_out,
        pending_poison_pill = pending_poison_pill,
        sm_promotion_storage_failed = sm_promotion_storage_failed,
        sm_promotion_not_promotable = sm_promotion_not_promotable,
        sm_promotion_blocklist_failed = sm_promotion_blocklist_failed,
        sm_promotion_dead_lettered = sm_promotion_dead_lettered,
        sm_drain_timeout = sm_drain_timeout,
        sm_resume_window_clamped = sm_resume_window_clamped,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn reset_metrics_for_test() {
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
        SM_UNACKED_EVICTED.store(0, Ordering::Release);
        PENDING_DELIVERY_QUOTA_EXCEEDED.store(0, Ordering::Release);
        PENDING_DELIVERY_ORPHAN_CLAIMS_RELEASED.store(0, Ordering::Release);
        PENDING_DELIVERY_AGED_OUT.store(0, Ordering::Release);
        PENDING_DELIVERY_UNRESOLVED_POISON_PILL.store(0, Ordering::Release);
        SM_PROMOTION_STORAGE_FAILED.store(0, Ordering::Release);
        SM_PROMOTION_BLOCKLIST_FAILED.store(0, Ordering::Release);
        SM_PROMOTION_DEAD_LETTERED.store(0, Ordering::Release);
        SM_DRAIN_TIMEOUT.store(0, Ordering::Release);
        SM_RESUME_WINDOW_CLAMPED.store(0, Ordering::Release);
    }

    #[test]
    fn test_decrement_saturates_at_zero() {
        let _guard = test_lock().lock().unwrap();
        reset_metrics_for_test();

        decrement_connected_users();
        decrement_room_count();

        assert_eq!(CONNECTED_USERS.load(Ordering::Acquire), 0);
        assert_eq!(ROOM_COUNT.load(Ordering::Acquire), 0);
    }

    #[test]
    fn test_increment_and_decrement_round_trip() {
        let _guard = test_lock().lock().unwrap();
        reset_metrics_for_test();

        increment_connected_users();
        increment_connected_users();
        decrement_connected_users();

        increment_room_count();
        decrement_room_count();

        assert_eq!(CONNECTED_USERS.load(Ordering::Acquire), 1);
        assert_eq!(ROOM_COUNT.load(Ordering::Acquire), 0);
    }

    #[test]
    fn test_rotate_second_bucket_moves_current_to_last() {
        let _guard = test_lock().lock().unwrap();
        reset_metrics_for_test();

        CURRENT_SECOND.store(100, Ordering::Release);
        CURRENT_SECOND_MESSAGES.store(7, Ordering::Release);

        rotate_second_bucket(101);

        assert_eq!(CURRENT_SECOND.load(Ordering::Acquire), 101);
        assert_eq!(CURRENT_SECOND_MESSAGES.load(Ordering::Acquire), 0);
        assert_eq!(LAST_SECOND_MESSAGES.load(Ordering::Acquire), 7);
    }

    #[test]
    fn test_render_metrics_contains_expected_families() {
        let _guard = test_lock().lock().unwrap();
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

    #[test]
    fn test_broadcast_counters_increment_and_render() {
        let _guard = test_lock().lock().unwrap();
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
    #[test]
    fn test_issue_209_finding_11_metric_families_render() {
        let _guard = test_lock().lock().unwrap();
        reset_metrics_for_test();

        increment_pending_delivery_quota_exceeded();
        add_pending_delivery_orphan_claims_released(7);
        add_pending_delivery_aged_out(3);
        increment_pending_delivery_unresolved_poison_pill();
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
        assert!(rendered.contains("waddle_sm_promotion_storage_failed_total 2"));
        assert!(rendered.contains("waddle_sm_promotion_not_promotable_total 1"));
        assert!(rendered.contains("waddle_sm_resume_window_clamped_total 1"));
    }

    #[test]
    fn test_sm_unacked_evicted_counter_increments_and_renders() {
        let _guard = test_lock().lock().unwrap();
        reset_metrics_for_test();

        increment_sm_unacked_evicted();
        increment_sm_unacked_evicted();

        let rendered = render_metrics();
        assert!(rendered.contains("# TYPE waddle_sm_unacked_evicted_total counter"));
        assert!(rendered.contains("waddle_sm_unacked_evicted_total 2"));
    }
}
