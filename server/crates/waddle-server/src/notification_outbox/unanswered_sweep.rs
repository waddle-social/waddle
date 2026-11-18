//! Timed missed-call emission backed by the callee's durable MAM evidence.

use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use tracing::{debug, warn};
use waddle_xmpp::mam::MamStorage;

use super::{
    call_outcome_recorded, evaluate_push_gate_at_dispatch, NoopDndReader, NoopRoomPolicy,
    NotificationOutboxStore, OutcomeAuthor, PushEvalCaches, PushEvalDeps, PushEvalStage,
    T1PushDispatchOutcome, CALL_UNANSWERED_DEADLINE_SECONDS, DEFAULT_ACTIVE_MENTION_TTL_SECONDS,
};
use crate::notification_activity::NoopActivityReader;
use crate::notification_settings_projection::NotificationSettingsProjectionStore;

/// How far back a sweep looks for calls that rang out: after a restart, and
/// for rows it could not settle.
pub(crate) const UNANSWERED_CALL_LOOKBACK_MS: i64 = 5 * 60 * 1_000;

/// Returns whether the bounded scan and all its evidence/policy/writes succeeded.
/// A failed row keeps the watermark, up to the lookback bound.
pub(crate) async fn sweep_unanswered_calls(
    outbox: &NotificationOutboxStore,
    mam: &dyn MamStorage,
    settings: &NotificationSettingsProjectionStore,
    watermark_ms: &mut i64,
    now: DateTime<Utc>,
    batch_size: usize,
) -> bool {
    let due_ms = now.timestamp_millis() - CALL_UNANSWERED_DEADLINE_SECONDS * 1_000;
    // A row that keeps failing, or a full page of calls that need no alert,
    // must not hold later calls back for longer than the restart horizon.
    *watermark_ms = (*watermark_ms).max(now.timestamp_millis() - UNANSWERED_CALL_LOOKBACK_MS);
    if due_ms <= *watermark_ms || batch_size == 0 {
        return true;
    }
    let calls = match outbox
        .due_unanswered_calls(*watermark_ms, due_ms, batch_size)
        .await
    {
        Ok(calls) => calls,
        Err(error) => {
            warn!(%error, "Unanswered-call scan failed; preserving watermark");
            return false;
        }
    };
    // A full page moves the watermark to just before its last row, so rows
    // sharing that timestamp are read again and later calls are not held back.
    let page_end_ms = (calls.len() == batch_size)
        .then(|| calls.last().map(|call| call.created_at_ms - 1))
        .flatten();
    let mut succeeded = true;
    let mut inserted = 0;
    for due in calls {
        let call = &due.candidate;
        let Some(sid) = call.call_session_id() else {
            succeeded = false;
            continue;
        };
        // The archive timestamp is the app clock; allow a minute either side
        // of the ring.
        let (Some(since), Some(until)) = (
            DateTime::from_timestamp_millis(due.created_at_ms.saturating_sub(60_000)),
            DateTime::from_timestamp_millis(
                due.created_at_ms
                    .saturating_add((CALL_UNANSWERED_DEADLINE_SECONDS + 60) * 1_000),
            ),
        ) else {
            succeeded = false;
            continue;
        };
        match call_outcome_recorded(
            mam,
            call.recipient_bare_jid(),
            &call.sender_jid().to_bare(),
            sid,
            since..until,
            OutcomeAuthor::CalleeOrCallerGlare,
        )
        .await
        {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                succeeded = false;
                warn!(%error, "Unanswered-call evidence lookup failed; skipping alert");
                continue;
            }
        }
        let candidate = match call.unanswered_call() {
            Ok(candidate) => candidate,
            Err(error) => {
                succeeded = false;
                warn!(%error, "Unanswered-call candidate construction failed");
                continue;
            }
        };
        let mut room_policy = BTreeMap::new();
        let mut dnd = BTreeMap::new();
        let mut activity = BTreeMap::new();
        let gate = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            PushEvalDeps {
                settings_projection: settings,
                room_policy: &NoopRoomPolicy,
                dnd_reader: &NoopDndReader,
                activity_reader: &NoopActivityReader,
                active_mention_ttl_ms: DEFAULT_ACTIVE_MENTION_TTL_SECONDS as i64 * 1_000,
            },
            &candidate,
            &mut PushEvalCaches {
                room_policy: &mut room_policy,
                dnd: &mut dnd,
                activity: &mut activity,
            },
        )
        .await;
        match gate {
            Ok(T1PushDispatchOutcome::Deliver { .. }) => {}
            Ok(T1PushDispatchOutcome::Suppressed { reason }) => {
                waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                    reason.telemetry_reason(),
                );
                continue;
            }
            Ok(T1PushDispatchOutcome::DeferUnknownRoomPolicy) => {
                succeeded = false;
                continue;
            }
            Err(error) => {
                succeeded = false;
                warn!(?error, "Unanswered-call T0 gate failed; skipping alert");
                continue;
            }
        }
        match outbox
            .insert_unanswered_call(call, now.timestamp_millis())
            .await
        {
            Ok(super::NotificationCandidateInsertOutcome::Inserted) => inserted += 1,
            Ok(super::NotificationCandidateInsertOutcome::Duplicate) => {}
            Err(error) => {
                succeeded = false;
                warn!(%error, "Unanswered-call insertion failed; preserving watermark");
            }
        }
    }
    let truncated = page_end_ms.is_some();
    if succeeded {
        *watermark_ms = (*watermark_ms).max(page_end_ms.unwrap_or(due_ms));
    }
    debug!(
        inserted,
        succeeded, truncated, "Notification outbox janitor checked unanswered calls"
    );
    succeeded
}
