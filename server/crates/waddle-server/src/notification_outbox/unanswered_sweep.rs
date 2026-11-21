//! Timed missed-call emission backed by the callee's durable MAM evidence.

use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, HashMap};
use tracing::{debug, warn};
use waddle_xmpp::mam::MamStorage;

use super::{
    call_outcome_recorded, evaluate_push_gate_at_dispatch, DueCallKey, DueUnansweredCall,
    NoopDndReader, NoopRoomPolicy, NotificationCandidateInsertOutcome, NotificationOutboxStore,
    OutcomeAuthor, PushEvalCaches, PushEvalDeps, PushEvalStage, T1PushDispatchOutcome,
    CALL_UNANSWERED_DEADLINE_SECONDS, DEFAULT_ACTIVE_MENTION_TTL_SECONDS,
};
use crate::notification_activity::NoopActivityReader;
use crate::notification_settings_projection::NotificationSettingsProjectionStore;

/// How far back a sweep looks for calls that rang out. An invitation older
/// than this, for example one that rang out during a long outage, gets no
/// alert.
pub(crate) const UNANSWERED_CALL_LOOKBACK_MS: i64 = 5 * 60 * 1_000;

/// What one process has already decided, so a sweep does not read the archive
/// again for a call that needed no alert. Every sweep scans the whole
/// lookback, so an invitation whose candidate is written late (deferred
/// insert, recovery) is still found.
#[derive(Debug, Default)]
pub struct UnansweredCallScan {
    /// Settled invitations and their `created_at_ms`.
    settled: HashMap<DueCallKey, i64>,
}

impl UnansweredCallScan {
    pub fn settled_calls(&self) -> usize {
        self.settled.len()
    }
}

/// Returns whether the scan and all its evidence, policy and writes succeeded.
/// A call that could not be settled is read again by the next sweep.
pub(crate) async fn sweep_unanswered_calls(
    outbox: &NotificationOutboxStore,
    mam: &dyn MamStorage,
    settings: &NotificationSettingsProjectionStore,
    scan: &mut UnansweredCallScan,
    now: DateTime<Utc>,
    batch_size: usize,
) -> bool {
    let after_ms = now.timestamp_millis() - UNANSWERED_CALL_LOOKBACK_MS;
    let due_ms = now.timestamp_millis() - CALL_UNANSWERED_DEADLINE_SECONDS * 1_000;
    scan.settled
        .retain(|_, created_at_ms| *created_at_ms > after_ms);
    if batch_size == 0 {
        return true;
    }
    let mut succeeded = true;
    let mut inserted = 0;
    let mut previous = None;
    loop {
        let page = match &previous {
            None => {
                outbox
                    .due_unanswered_calls(after_ms, due_ms, batch_size)
                    .await
            }
            Some(previous) => {
                outbox
                    .due_unanswered_calls_after(after_ms, previous, due_ms, batch_size)
                    .await
            }
        };
        let calls = match page {
            Ok(calls) => calls,
            Err(error) => {
                warn!(%error, "Unanswered-call scan failed");
                return false;
            }
        };
        let full_page = calls.len() == batch_size;
        for due in &calls {
            if scan.settled.contains_key(due.key()) {
                continue;
            }
            match alert_if_unanswered(outbox, mam, settings, due, now).await {
                Ok(alerted) => {
                    inserted += usize::from(alerted);
                    scan.settled.insert(due.key().clone(), due.created_at_ms);
                }
                Err(()) => succeeded = false,
            }
        }
        previous = calls.into_iter().next_back();
        if !full_page {
            break;
        }
    }
    debug!(
        inserted,
        succeeded, "Notification outbox janitor checked unanswered calls"
    );
    succeeded
}

/// Whether an alert was inserted. `Err` means the call could not be settled
/// and must be read again.
async fn alert_if_unanswered(
    outbox: &NotificationOutboxStore,
    mam: &dyn MamStorage,
    settings: &NotificationSettingsProjectionStore,
    due: &DueUnansweredCall,
    now: DateTime<Utc>,
) -> Result<bool, ()> {
    let call = &due.candidate;
    let sid = call.call_session_id().ok_or(())?;
    // The archive timestamp is the app clock; allow a minute either side of
    // the ring.
    let since = DateTime::from_timestamp_millis(due.created_at_ms.saturating_sub(60_000));
    let until = DateTime::from_timestamp_millis(
        due.created_at_ms
            .saturating_add((CALL_UNANSWERED_DEADLINE_SECONDS + 60) * 1_000),
    );
    let (Some(since), Some(until)) = (since, until) else {
        return Err(());
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
        Ok(true) => return Ok(false),
        Ok(false) => {}
        Err(error) => {
            warn!(%error, "Unanswered-call evidence lookup failed; skipping alert");
            return Err(());
        }
    }
    let candidate = call.unanswered_call().map_err(|error| {
        warn!(%error, "Unanswered-call candidate construction failed");
    })?;
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
            return Ok(false);
        }
        Ok(T1PushDispatchOutcome::DeferUnknownRoomPolicy) => return Err(()),
        Err(error) => {
            warn!(?error, "Unanswered-call T0 gate failed; skipping alert");
            return Err(());
        }
    }
    match outbox
        .insert_unanswered_call(call, now.timestamp_millis())
        .await
    {
        Ok(NotificationCandidateInsertOutcome::Inserted) => Ok(true),
        Ok(NotificationCandidateInsertOutcome::Duplicate) => Ok(false),
        Err(error) => {
            warn!(%error, "Unanswered-call insertion failed");
            Err(())
        }
    }
}
