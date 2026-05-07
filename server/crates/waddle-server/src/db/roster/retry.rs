use std::time::Duration;

use super::RosterStorageError;

/// Maximum retries for transient `SQLITE_BUSY` lock contention before
/// surfacing the error.
pub(super) const MAX_LOCK_RETRIES: usize = 6;

/// Format a UTC timestamp matching SQLite's `datetime('now')` output
/// (`YYYY-MM-DD HH:MM:SS`). Bound as a parameter rather than embedded as a
/// SQL function call so the same statements work against Postgres too —
/// `datetime('now')` is SQLite-only and would error on Postgres.
pub(super) fn now_utc_text() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub(super) fn is_sqlite_lock_error(error: &RosterStorageError) -> bool {
    let msg = error.to_string().to_ascii_lowercase();
    msg.contains("database is locked")
        || msg.contains("sqlite_busy")
        || msg.contains("database busy")
}

pub(super) fn retry_delay(attempt: usize) -> Duration {
    // Exponential backoff with a short ceiling for local sqlite contention.
    let base_ms = 10_u64;
    let max_ms = 320_u64;
    let delay_ms = (base_ms << attempt.min(5)).min(max_ms);
    Duration::from_millis(delay_ms)
}
