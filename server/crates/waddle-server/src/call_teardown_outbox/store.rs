use std::str::FromStr;

use jid::{BareJid, FullJid};
use waddle_sfu::{CallGeneration, CallId, ParticipantSid, RoomSid};

use super::{
    schema, CallTeardownIntent, CallTeardownIntentId, CallTeardownJob, CallTeardownLastError,
    CallTeardownOutboxError, CallTeardownQueueStats, CallTeardownRetryOutcome,
    CallTeardownRetryReason, CallTeardownStatus, ClaimToken, TeardownTarget,
};
use crate::db::{Database, Row};

pub const MAX_ATTEMPTS: i64 = 20;
pub const BASE_RETRY_DELAY_MS: i64 = 5_000;
pub const MAX_RETRY_DELAY_MS: i64 = 10 * 60 * 1_000;
pub const CLAIM_TIMEOUT_MS: i64 = 5 * 60 * 1_000;
pub const FAILED_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
const PRUNE_BATCH_SIZE: i64 = 128;
const OWNERSHIP_RETRY_DELAY_MS: i64 = 15_000;

const STATUS_QUEUED: &str = "queued";
const STATUS_IN_PROGRESS: &str = "in-progress";
const STATUS_DONE: &str = "done";
const STATUS_FAILED: &str = "failed";

#[derive(Clone)]
pub struct CallTeardownOutboxStore {
    db: Database,
}

impl CallTeardownOutboxStore {
    pub async fn new(db: Database) -> Result<Self, CallTeardownOutboxError> {
        schema::initialize(&db).await?;
        Ok(Self { db })
    }

    pub async fn enqueue(
        &self,
        intent: CallTeardownIntent,
    ) -> Result<CallTeardownIntentId, CallTeardownOutboxError> {
        self.enqueue_at(intent, crate::time::now_ms()).await
    }

    /// Atomically persist a related set of teardown effects. Muji owner
    /// cleanup uses this so the presence clear cannot be lost while only the
    /// participant removal survives a partial write.
    pub async fn enqueue_batch(
        &self,
        intents: &[CallTeardownIntent],
    ) -> Result<Vec<CallTeardownIntentId>, CallTeardownOutboxError> {
        let now_ms = crate::time::now_ms();
        let mut transaction = self.db.begin().await?;
        let mut intent_ids = Vec::with_capacity(intents.len());
        for intent in intents {
            let intent_id = CallTeardownIntentId::new();
            let (action, identity, room_jid, participant_sid) = encode_target(&intent.target);
            let generation = encode_generation(intent.generation)?;
            transaction
                .execute(
                    "INSERT INTO call_teardown_outbox (\
                        intent_id, call_id, identity, room_jid, action, generation, \
                        room_sid, participant_sid, status, attempt_count, last_error, \
                        next_attempt_at_ms, claimed_at_ms, claim_token, created_at_ms, updated_at_ms\
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, ?, NULL, NULL, ?, ?)",
                    crate::db_params![
                        intent_id.as_str(),
                        intent.call_id.as_str(),
                        identity,
                        room_jid,
                        action,
                        generation,
                        intent.room_sid.as_ref().map(RoomSid::as_str),
                        participant_sid,
                        STATUS_QUEUED,
                        now_ms,
                        now_ms,
                        now_ms,
                    ],
                )
                .await?;
            intent_ids.push(intent_id);
        }
        transaction.commit().await?;
        Ok(intent_ids)
    }

    pub(super) async fn enqueue_at(
        &self,
        intent: CallTeardownIntent,
        now_ms: i64,
    ) -> Result<CallTeardownIntentId, CallTeardownOutboxError> {
        let intent_id = CallTeardownIntentId::new();
        let (action, identity, room_jid, participant_sid) = encode_target(&intent.target);
        let generation = encode_generation(intent.generation)?;
        let connection = self.db.guard().await?;
        // Dedupe queued work (#1449 review N2): the relay-failure
        // fallback and the subsequent local presence-clear both enqueue
        // for the same departure, and the drained effects are
        // idempotent, so an identical still-queued intent makes a new
        // row pure noise. A racing duplicate insert is harmless.
        let mut existing = connection
            .query(
                "SELECT intent_id FROM call_teardown_outbox \
                 WHERE status = ? AND call_id = ? AND action = ? \
                   AND identity IS NOT DISTINCT FROM ? \
                   AND room_jid IS NOT DISTINCT FROM ? \
                   AND generation IS NOT DISTINCT FROM ? \
                   AND room_sid IS NOT DISTINCT FROM ? \
                   AND participant_sid IS NOT DISTINCT FROM ? \
                 LIMIT 1",
                crate::db_params![
                    STATUS_QUEUED,
                    intent.call_id.as_str(),
                    action,
                    identity.clone(),
                    room_jid.clone(),
                    generation,
                    intent.room_sid.as_ref().map(RoomSid::as_str),
                    participant_sid,
                ],
            )
            .await?;
        if let Some(row) = existing.next().await? {
            let existing_id = row.get::<String>(0)?;
            return Ok(CallTeardownIntentId::from_stored(existing_id));
        }
        connection
            .execute(
                "INSERT INTO call_teardown_outbox (\
                    intent_id, call_id, identity, room_jid, action, generation, \
                    room_sid, participant_sid, status, attempt_count, last_error, \
                    next_attempt_at_ms, claimed_at_ms, claim_token, created_at_ms, updated_at_ms\
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, ?, NULL, NULL, ?, ?)",
                crate::db_params![
                    intent_id.as_str(),
                    intent.call_id.as_str(),
                    identity,
                    room_jid,
                    action,
                    generation,
                    intent.room_sid.as_ref().map(RoomSid::as_str),
                    participant_sid,
                    STATUS_QUEUED,
                    now_ms,
                    now_ms,
                    now_ms,
                ],
            )
            .await?;
        Ok(intent_id)
    }

    pub async fn claim_due(
        &self,
        batch_size: usize,
    ) -> Result<Vec<CallTeardownJob>, CallTeardownOutboxError> {
        self.claim_due_at(batch_size, crate::time::now_ms()).await
    }

    pub(super) async fn claim_due_at(
        &self,
        batch_size: usize,
        now_ms: i64,
    ) -> Result<Vec<CallTeardownJob>, CallTeardownOutboxError> {
        let batch_size = batch_size.clamp(1, 1_000);
        let stale_before_ms = now_ms.saturating_sub(CLAIM_TIMEOUT_MS);
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                &format!(
                    "{} WHERE (\
                        status = ? AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)\
                     ) OR (\
                        status = ? AND claimed_at_ms IS NOT NULL AND claimed_at_ms <= ?\
                     ) ORDER BY created_at_ms ASC, intent_id ASC LIMIT ?",
                    select_columns()
                ),
                crate::db_params![
                    STATUS_QUEUED,
                    now_ms,
                    STATUS_IN_PROGRESS,
                    stale_before_ms,
                    batch_size,
                ],
            )
            .await?;
        let mut selected = Vec::new();
        while let Some(row) = rows.next().await? {
            selected.push(decode_job(&row)?);
        }
        drop(rows);
        drop(connection);

        let mut claimed = Vec::with_capacity(selected.len());
        for mut job in selected {
            let claim_token = ClaimToken::new();
            let connection = self.db.guard().await?;
            let affected = connection
                .execute(
                    "UPDATE call_teardown_outbox \
                     SET status = ?, claimed_at_ms = ?, claim_token = ?, updated_at_ms = ? \
                     WHERE intent_id = ? AND (\
                        (status = ? AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)) \
                        OR (status = ? AND claimed_at_ms IS NOT NULL AND claimed_at_ms <= ?)\
                     )",
                    crate::db_params![
                        STATUS_IN_PROGRESS,
                        now_ms,
                        claim_token.as_str(),
                        now_ms,
                        job.intent_id.as_str(),
                        STATUS_QUEUED,
                        now_ms,
                        STATUS_IN_PROGRESS,
                        stale_before_ms,
                    ],
                )
                .await?;
            if affected == 1 {
                job.status = CallTeardownStatus::InProgress;
                job.claim_token = Some(claim_token);
                claimed.push(job);
            }
        }
        Ok(claimed)
    }

    pub async fn mark_done(&self, job: &CallTeardownJob) -> Result<bool, CallTeardownOutboxError> {
        self.mark_done_at(job, crate::time::now_ms()).await
    }

    pub(super) async fn mark_done_at(
        &self,
        job: &CallTeardownJob,
        now_ms: i64,
    ) -> Result<bool, CallTeardownOutboxError> {
        let connection = self.db.guard().await?;
        let affected = connection
            .execute(
                "UPDATE call_teardown_outbox \
                 SET status = ?, last_error = NULL, next_attempt_at_ms = NULL, \
                     claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = ? \
                 WHERE intent_id = ? AND status = ? AND claim_token = ?",
                crate::db_params![
                    STATUS_DONE,
                    now_ms,
                    job.intent_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_ref().map(ClaimToken::as_str),
                ],
            )
            .await?;
        Ok(affected == 1)
    }

    /// Whether a room-scoped participant removal still depends on its
    /// XEP-0272 presence clear. Both queued and claimed rows count: a
    /// participant selected earlier in the same batch must wait until the
    /// presence-clear terminal write commits.
    pub(crate) async fn has_pending_muji_presence_clear(
        &self,
        call_id: &CallId,
        departed: &FullJid,
    ) -> Result<bool, CallTeardownOutboxError> {
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                "SELECT 1 FROM call_teardown_outbox \
                 WHERE call_id = ? AND identity = ? AND action = ? \
                   AND status IN (?, ?) LIMIT 1",
                crate::db_params![
                    call_id.as_str(),
                    departed.to_string(),
                    "muji_presence_clear",
                    STATUS_QUEUED,
                    STATUS_IN_PROGRESS,
                ],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    /// Releases a claim without counting an execution attempt. The drain uses
    /// this when another node owns the intent's clustered room/user scope or
    /// when claim lookup itself is unavailable.
    pub async fn release_claim(
        &self,
        job: &CallTeardownJob,
    ) -> Result<bool, CallTeardownOutboxError> {
        self.release_claim_at(job, crate::time::now_ms()).await
    }

    pub(super) async fn release_claim_at(
        &self,
        job: &CallTeardownJob,
        now_ms: i64,
    ) -> Result<bool, CallTeardownOutboxError> {
        let next_attempt_at_ms = now_ms.saturating_add(OWNERSHIP_RETRY_DELAY_MS);
        let connection = self.db.guard().await?;
        let affected = connection
            .execute(
                "UPDATE call_teardown_outbox \
                 SET status = ?, next_attempt_at_ms = ?, claimed_at_ms = NULL, \
                     claim_token = NULL, updated_at_ms = ? \
                 WHERE intent_id = ? AND status = ? AND claim_token = ?",
                crate::db_params![
                    STATUS_QUEUED,
                    next_attempt_at_ms,
                    now_ms,
                    job.intent_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_ref().map(ClaimToken::as_str),
                ],
            )
            .await?;
        Ok(affected == 1)
    }

    pub async fn fail_claim(
        &self,
        job: &CallTeardownJob,
        error: CallTeardownLastError,
    ) -> Result<bool, CallTeardownOutboxError> {
        self.fail_claim_at(job, error, crate::time::now_ms()).await
    }

    pub(super) async fn fail_claim_at(
        &self,
        job: &CallTeardownJob,
        error: CallTeardownLastError,
        now_ms: i64,
    ) -> Result<bool, CallTeardownOutboxError> {
        let connection = self.db.guard().await?;
        let affected = connection
            .execute(
                "UPDATE call_teardown_outbox \
                 SET status = ?, last_error = ?, next_attempt_at_ms = NULL, \
                     claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = ? \
                 WHERE intent_id = ? AND status = ? AND claim_token = ?",
                crate::db_params![
                    STATUS_FAILED,
                    error.as_db_value(),
                    now_ms,
                    job.intent_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_ref().map(ClaimToken::as_str),
                ],
            )
            .await?;
        Ok(affected == 1)
    }

    pub async fn retry_or_fail(
        &self,
        job: &CallTeardownJob,
        error: CallTeardownRetryReason,
    ) -> Result<CallTeardownRetryOutcome, CallTeardownOutboxError> {
        self.retry_or_fail_at(job, error, crate::time::now_ms())
            .await
    }

    pub(super) async fn retry_or_fail_at(
        &self,
        job: &CallTeardownJob,
        error: CallTeardownRetryReason,
        now_ms: i64,
    ) -> Result<CallTeardownRetryOutcome, CallTeardownOutboxError> {
        let attempt_count = job.attempt_count.saturating_add(1);
        let failed = attempt_count >= MAX_ATTEMPTS;
        let status = if failed { STATUS_FAILED } else { STATUS_QUEUED };
        let next_attempt_at_ms =
            (!failed).then(|| now_ms.saturating_add(retry_delay_ms(attempt_count)));
        let connection = self.db.guard().await?;
        let affected = connection
            .execute(
                "UPDATE call_teardown_outbox \
                 SET status = ?, attempt_count = ?, last_error = ?, \
                     next_attempt_at_ms = ?, claimed_at_ms = NULL, claim_token = NULL, \
                     updated_at_ms = ? \
                 WHERE intent_id = ? AND status = ? AND claim_token = ?",
                crate::db_params![
                    status,
                    attempt_count,
                    error.as_db_value(),
                    next_attempt_at_ms,
                    now_ms,
                    job.intent_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_ref().map(ClaimToken::as_str),
                ],
            )
            .await?;
        if affected == 0 {
            return Ok(CallTeardownRetryOutcome::ClaimLost);
        }
        if failed {
            Ok(CallTeardownRetryOutcome::Failed { attempt_count })
        } else {
            Ok(CallTeardownRetryOutcome::Requeued { attempt_count })
        }
    }

    pub async fn queue_stats(&self) -> Result<CallTeardownQueueStats, CallTeardownOutboxError> {
        self.queue_stats_at(crate::time::now_ms()).await
    }

    async fn queue_stats_at(
        &self,
        now_ms: i64,
    ) -> Result<CallTeardownQueueStats, CallTeardownOutboxError> {
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                "SELECT COUNT(*), MIN(created_at_ms) FROM call_teardown_outbox WHERE status = ?",
                crate::db_params![STATUS_QUEUED],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(CallTeardownQueueStats::default());
        };
        let queued_count = u64::try_from(row.get::<i64>(0)?).unwrap_or_default();
        let oldest_created_at_ms = row.get::<Option<i64>>(1)?;
        let oldest_queued_age_ms = oldest_created_at_ms
            .map(|created_at_ms| now_ms.saturating_sub(created_at_ms))
            .and_then(|age| u64::try_from(age).ok())
            .unwrap_or_default();
        Ok(CallTeardownQueueStats {
            queued_count,
            oldest_queued_age_ms,
        })
    }

    pub async fn prune_failed(&self) -> Result<u64, CallTeardownOutboxError> {
        self.prune_failed_at(crate::time::now_ms()).await
    }

    pub(super) async fn prune_failed_at(
        &self,
        now_ms: i64,
    ) -> Result<u64, CallTeardownOutboxError> {
        let prune_before_ms = now_ms.saturating_sub(FAILED_RETENTION_MS);
        let connection = self.db.guard().await?;
        Ok(connection
            .execute(
                "DELETE FROM call_teardown_outbox WHERE intent_id IN (\
                    SELECT intent_id FROM call_teardown_outbox \
                    WHERE status IN (?, ?) AND updated_at_ms < ? \
                    ORDER BY updated_at_ms ASC, intent_id ASC LIMIT ?\
                 )",
                crate::db_params![
                    STATUS_DONE,
                    STATUS_FAILED,
                    prune_before_ms,
                    PRUNE_BATCH_SIZE
                ],
            )
            .await?)
    }

    pub async fn find(
        &self,
        intent_id: &CallTeardownIntentId,
    ) -> Result<Option<CallTeardownJob>, CallTeardownOutboxError> {
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                &format!("{} WHERE intent_id = ?", select_columns()),
                crate::db_params![intent_id.as_str()],
            )
            .await?;
        rows.next().await?.map(|row| decode_job(&row)).transpose()
    }
}

fn encode_generation(
    generation: Option<CallGeneration>,
) -> Result<Option<i64>, CallTeardownOutboxError> {
    generation
        .map(|value| {
            if value.as_u64() == 0 {
                return Err(CallTeardownOutboxError::InvalidGeneration(0));
            }
            i64::try_from(value.as_u64())
                .map_err(|_| CallTeardownOutboxError::GenerationOverflow(value.as_u64()))
        })
        .transpose()
}

pub fn retry_delay_ms(attempt_count: i64) -> i64 {
    let exponent = (attempt_count - 1).clamp(0, 10) as u32;
    BASE_RETRY_DELAY_MS
        .saturating_mul(2_i64.saturating_pow(exponent))
        .min(MAX_RETRY_DELAY_MS)
}

fn select_columns() -> &'static str {
    "SELECT intent_id, call_id, identity, room_jid, action, generation, \
            room_sid, participant_sid, status, attempt_count, last_error, \
            next_attempt_at_ms, claim_token, created_at_ms \
     FROM call_teardown_outbox"
}

fn encode_target(
    target: &TeardownTarget,
) -> (&'static str, Option<String>, Option<String>, Option<&str>) {
    match target {
        TeardownTarget::Participant {
            identity,
            participant_sid,
        } => (
            "remove_participant",
            Some(identity.to_string()),
            None,
            participant_sid.as_ref().map(ParticipantSid::as_str),
        ),
        TeardownTarget::Room => ("delete_room", None, None, None),
        TeardownTarget::MujiPresenceClear {
            room_jid,
            departed,
            participant_sid,
        } => (
            "muji_presence_clear",
            Some(departed.to_string()),
            Some(room_jid.to_string()),
            participant_sid.as_ref().map(ParticipantSid::as_str),
        ),
        TeardownTarget::MujiRoomSweep { room_jid } => {
            ("muji_room_sweep", None, Some(room_jid.to_string()), None)
        }
    }
}

fn decode_job(row: &Row) -> Result<CallTeardownJob, CallTeardownOutboxError> {
    let action = row.get::<String>(4)?;
    let identity = row.get::<Option<String>>(2)?;
    let room_jid = row.get::<Option<String>>(3)?;
    let participant_sid = row.get::<Option<String>>(7)?;
    let target = decode_target(&action, identity, room_jid, participant_sid)?;
    let generation = row
        .get::<Option<i64>>(5)?
        .map(|value| {
            if value <= 0 {
                return Err(CallTeardownOutboxError::InvalidGeneration(value));
            }
            let value = u64::try_from(value)
                .map_err(|_| CallTeardownOutboxError::InvalidGeneration(value))?;
            Ok(CallGeneration::try_from(value)?)
        })
        .transpose()?;
    Ok(CallTeardownJob {
        intent_id: CallTeardownIntentId::from_stored(row.get(0)?),
        intent: CallTeardownIntent {
            call_id: CallId::new(row.get::<String>(1)?)?,
            target,
            generation,
            room_sid: row
                .get::<Option<String>>(6)?
                .map(RoomSid::new)
                .transpose()?,
        },
        status: CallTeardownStatus::from_db_value(row.get(8)?)?,
        attempt_count: row.get(9)?,
        last_error: row
            .get::<Option<String>>(10)?
            .map(CallTeardownLastError::from_db_value),
        next_attempt_at_ms: row.get(11)?,
        claim_token: row.get::<Option<String>>(12)?.map(ClaimToken::from_stored),
        created_at_ms: row.get(13)?,
    })
}

fn decode_target(
    action: &str,
    identity: Option<String>,
    room_jid: Option<String>,
    participant_sid: Option<String>,
) -> Result<TeardownTarget, CallTeardownOutboxError> {
    match action {
        "remove_participant" => match (identity, room_jid) {
            (Some(identity), None) if !identity.is_empty() => Ok(TeardownTarget::Participant {
                identity: FullJid::from_str(&identity)
                    .map_err(|_| CallTeardownOutboxError::InvalidFullJid(identity))?,
                participant_sid: participant_sid.map(ParticipantSid::new).transpose()?,
            }),
            _ => Err(CallTeardownOutboxError::InvalidTargetShape(
                action.to_owned(),
            )),
        },
        "delete_room" if identity.is_none() && room_jid.is_none() && participant_sid.is_none() => {
            Ok(TeardownTarget::Room)
        }
        "muji_presence_clear" => match (identity, room_jid) {
            (Some(departed), Some(room_jid)) if !departed.is_empty() => {
                Ok(TeardownTarget::MujiPresenceClear {
                    departed: FullJid::from_str(&departed)
                        .map_err(|_| CallTeardownOutboxError::InvalidFullJid(departed))?,
                    room_jid: BareJid::from_str(&room_jid)
                        .map_err(|_| CallTeardownOutboxError::InvalidBareJid(room_jid))?,
                    participant_sid: participant_sid.map(ParticipantSid::new).transpose()?,
                })
            }
            _ => Err(CallTeardownOutboxError::InvalidTargetShape(
                action.to_owned(),
            )),
        },
        "muji_room_sweep" => match (identity, room_jid, participant_sid) {
            (None, Some(room_jid), None) => Ok(TeardownTarget::MujiRoomSweep {
                room_jid: BareJid::from_str(&room_jid)
                    .map_err(|_| CallTeardownOutboxError::InvalidBareJid(room_jid))?,
            }),
            _ => Err(CallTeardownOutboxError::InvalidTargetShape(
                action.to_owned(),
            )),
        },
        "delete_room" => Err(CallTeardownOutboxError::InvalidTargetShape(
            action.to_owned(),
        )),
        _ => Err(CallTeardownOutboxError::InvalidAction(action.to_owned())),
    }
}
