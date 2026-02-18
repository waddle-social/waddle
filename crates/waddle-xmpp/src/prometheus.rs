//! Lightweight Prometheus exporter for core XMPP runtime metrics.
//!
//! This module tracks a small set of process-level metrics required for
//! operational health dashboards and exposes them in Prometheus text format.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

static CONNECTED_USERS: AtomicI64 = AtomicI64::new(0);
static ROOM_COUNT: AtomicI64 = AtomicI64::new(0);
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
    loop {
        let current = CONNECTED_USERS.load(Ordering::Acquire);
        let next = if current > 0 { current - 1 } else { 0 };
        if CONNECTED_USERS
            .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            break;
        }
    }
}

pub fn increment_room_count() {
    ROOM_COUNT.fetch_add(1, Ordering::AcqRel);
}

pub fn decrement_room_count() {
    loop {
        let current = ROOM_COUNT.load(Ordering::Acquire);
        let next = if current > 0 { current - 1 } else { 0 };
        if ROOM_COUNT
            .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            break;
        }
    }
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
