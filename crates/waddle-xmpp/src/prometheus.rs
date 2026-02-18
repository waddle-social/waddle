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

pub fn render_metrics() -> String {
    let now = unix_timestamp_secs();
    rotate_second_bucket(now);

    let connected_users = CONNECTED_USERS.load(Ordering::Acquire);
    let room_count = ROOM_COUNT.load(Ordering::Acquire);
    let messages_total = MESSAGES_TOTAL.load(Ordering::Acquire);
    let messages_per_second = LAST_SECOND_MESSAGES.load(Ordering::Acquire);

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
            "waddle_messages_per_second {messages_per_second}\n"
        ),
        connected_users = connected_users,
        room_count = room_count,
        messages_total = messages_total,
        messages_per_second = messages_per_second
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
}
