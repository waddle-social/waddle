//! File-backed observer for raw call candidate effects, before semantic deduplication.
//!
//! Inert unless a spawned test server explicitly enables it. Recording the plan
//! boundary makes per-resource double planning visible even when candidate and
//! receipt uniqueness would hide the second execution.

use std::fs::OpenOptions;
use std::io::Write;

use crate::notification_outbox::{NotificationCandidate, NotificationClass};

pub(crate) fn record(candidate: &NotificationCandidate) {
    if !matches!(
        candidate.class(),
        NotificationClass::Call | NotificationClass::MissedCall
    ) {
        return;
    }
    let Some(path) = std::env::var_os("WADDLE_TEST_CALL_CANDIDATE_EFFECTS_FILE") else {
        return;
    };
    let mut event = serde_json::to_vec(&(
        candidate.recipient_bare_jid().as_str(),
        candidate.archive_stanza_id().as_str(),
    ))
    .expect("encode call candidate observation");
    event.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open call candidate observations")
        .write_all(&event)
        .expect("record call candidate observation");
}
