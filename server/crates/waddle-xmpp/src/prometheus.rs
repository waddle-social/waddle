//! Lightweight Prometheus exporter for core XMPP runtime metrics.
//!
//! This module tracks a small set of process-level metrics required for
//! operational health dashboards and exposes them in Prometheus text format.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

static CONNECTED_USERS: AtomicU64 = AtomicU64::new(0);
static ROOM_COUNT: AtomicU64 = AtomicU64::new(0);
static MESSAGES_TOTAL: AtomicU64 = AtomicU64::new(0);
static CURRENT_SECOND: AtomicU64 = AtomicU64::new(0);
static CURRENT_SECOND_MESSAGES: AtomicU64 = AtomicU64::new(0);
static LAST_SECOND_MESSAGES: AtomicU64 = AtomicU64::new(0);
static CALL_STARTS_TOTAL: AtomicU64 = AtomicU64::new(0);
static CALL_JOINS_TOTAL: AtomicU64 = AtomicU64::new(0);
static CALL_LEAVES_TOTAL: AtomicU64 = AtomicU64::new(0);
static ACTIVE_CALLS: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct DurationSummary {
    count: u64,
    sum_seconds: f64,
}

type FailureMap = std::collections::BTreeMap<(String, String), u64>;
type DurationMap = std::collections::BTreeMap<String, DurationSummary>;

fn call_failures() -> &'static Mutex<FailureMap> {
    static CALL_FAILURES: OnceLock<Mutex<FailureMap>> = OnceLock::new();
    CALL_FAILURES.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

fn call_durations() -> &'static Mutex<DurationMap> {
    static CALL_DURATIONS: OnceLock<Mutex<DurationMap>> = OnceLock::new();
    CALL_DURATIONS.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
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

pub fn record_call_started() {
    CALL_STARTS_TOTAL.fetch_add(1, Ordering::AcqRel);
}

pub fn record_call_joined() {
    CALL_JOINS_TOTAL.fetch_add(1, Ordering::AcqRel);
}

pub fn record_call_left() {
    CALL_LEAVES_TOTAL.fetch_add(1, Ordering::AcqRel);
}

pub fn set_active_calls(active_calls: u64) {
    ACTIVE_CALLS.store(active_calls, Ordering::Release);
}

pub fn record_call_failure(operation: &str, reason: &str) {
    let mut failures = call_failures()
        .lock()
        .expect("call failure metrics mutex should not be poisoned");
    *failures
        .entry((operation.to_string(), reason.to_string()))
        .or_insert(0) += 1;
}

pub fn record_call_operation_duration(operation: &str, duration_seconds: f64) {
    let mut durations = call_durations()
        .lock()
        .expect("call duration metrics mutex should not be poisoned");
    let summary = durations.entry(operation.to_string()).or_default();
    summary.count += 1;
    summary.sum_seconds += duration_seconds.max(0.0);
}

fn escaped_label_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

pub fn render_metrics() -> String {
    let now = unix_timestamp_secs();
    rotate_second_bucket(now);

    let connected_users = CONNECTED_USERS.load(Ordering::Acquire);
    let room_count = ROOM_COUNT.load(Ordering::Acquire);
    let messages_total = MESSAGES_TOTAL.load(Ordering::Acquire);
    let messages_per_second = LAST_SECOND_MESSAGES.load(Ordering::Acquire);
    let call_starts_total = CALL_STARTS_TOTAL.load(Ordering::Acquire);
    let call_joins_total = CALL_JOINS_TOTAL.load(Ordering::Acquire);
    let call_leaves_total = CALL_LEAVES_TOTAL.load(Ordering::Acquire);
    let active_calls = ACTIVE_CALLS.load(Ordering::Acquire);
    let call_failures = call_failures()
        .lock()
        .expect("call failure metrics mutex should not be poisoned");
    let call_durations = call_durations()
        .lock()
        .expect("call duration metrics mutex should not be poisoned");

    let mut rendered = format!(
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
            "# HELP waddle_call_starts_total Total number of started calls.\n",
            "# TYPE waddle_call_starts_total counter\n",
            "waddle_call_starts_total {call_starts_total}\n",
            "# HELP waddle_call_joins_total Total number of call participant joins.\n",
            "# TYPE waddle_call_joins_total counter\n",
            "waddle_call_joins_total {call_joins_total}\n",
            "# HELP waddle_call_leaves_total Total number of call participant leaves.\n",
            "# TYPE waddle_call_leaves_total counter\n",
            "waddle_call_leaves_total {call_leaves_total}\n",
            "# HELP waddle_active_calls Current number of active calls.\n",
            "# TYPE waddle_active_calls gauge\n",
            "waddle_active_calls {active_calls}\n",
            "# HELP waddle_call_failures_total Call lifecycle failures by operation and reason.\n",
            "# TYPE waddle_call_failures_total counter\n",
            "# HELP waddle_call_operation_duration_seconds Call lifecycle operation duration summary.\n",
            "# TYPE waddle_call_operation_duration_seconds summary\n",
            "# ALERTING_NOTE waddle_call_failures_total should page on sustained growth by operation/reason.\n",
            "# ALERTING_NOTE waddle_active_calls can be used for sudden drop detection.\n"
        ),
        connected_users = connected_users,
        room_count = room_count,
        messages_total = messages_total,
        messages_per_second = messages_per_second,
        call_starts_total = call_starts_total,
        call_joins_total = call_joins_total,
        call_leaves_total = call_leaves_total,
        active_calls = active_calls
    );

    for ((operation, reason), count) in call_failures.iter() {
        rendered.push_str(&format!(
            "waddle_call_failures_total{{operation=\"{}\",reason=\"{}\"}} {}\n",
            escaped_label_value(operation),
            escaped_label_value(reason),
            count
        ));
    }

    for (operation, summary) in call_durations.iter() {
        rendered.push_str(&format!(
            "waddle_call_operation_duration_seconds_sum{{operation=\"{}\"}} {:.9}\n",
            escaped_label_value(operation),
            summary.sum_seconds
        ));
        rendered.push_str(&format!(
            "waddle_call_operation_duration_seconds_count{{operation=\"{}\"}} {}\n",
            escaped_label_value(operation),
            summary.count
        ));
    }

    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

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
        CALL_STARTS_TOTAL.store(0, Ordering::Release);
        CALL_JOINS_TOTAL.store(0, Ordering::Release);
        CALL_LEAVES_TOTAL.store(0, Ordering::Release);
        ACTIVE_CALLS.store(0, Ordering::Release);
        call_failures().lock().unwrap().clear();
        call_durations().lock().unwrap().clear();
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
        assert!(rendered.contains("# HELP waddle_call_starts_total"));
        assert!(rendered.contains("# HELP waddle_call_failures_total"));
        assert!(rendered.contains("# HELP waddle_call_operation_duration_seconds"));
    }

    #[test]
    fn test_call_failure_labels_render() {
        let _guard = test_lock().lock().unwrap();
        reset_metrics_for_test();

        record_call_failure("bootstrap", "call_not_found");
        record_call_failure("bootstrap", "call_not_found");
        record_call_failure("create", "media_disabled");

        let rendered = render_metrics();
        assert!(rendered.contains(
            "waddle_call_failures_total{operation=\"bootstrap\",reason=\"call_not_found\"} 2"
        ));
        assert!(rendered.contains(
            "waddle_call_failures_total{operation=\"create\",reason=\"media_disabled\"} 1"
        ));
    }

    #[test]
    fn test_call_duration_summary_render() {
        let _guard = test_lock().lock().unwrap();
        reset_metrics_for_test();

        record_call_operation_duration("create", 0.100);
        record_call_operation_duration("create", 0.250);

        let rendered = render_metrics();
        assert!(rendered.contains(
            "waddle_call_operation_duration_seconds_sum{operation=\"create\"} 0.350000000"
        ));
        assert!(rendered
            .contains("waddle_call_operation_duration_seconds_count{operation=\"create\"} 2"));
    }
}
