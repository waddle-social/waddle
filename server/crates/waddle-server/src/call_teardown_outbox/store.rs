use jid::FullJid;
use waddle_sfu::{CallGeneration, CallId, ParticipantSid, RoomSid};
use waddle_xmpp::ownership::{CurrentNodeIdentityGuard, NodeIdentity, SharedNodeIdentity};

use super::store_rows::decode_job;
use super::{
    schema, CallTeardownIntent, CallTeardownIntentId, CallTeardownJob, CallTeardownOutboxError,
    CallTeardownProducingNode, CallTeardownQueueStats, TeardownTarget,
};
use crate::db::Database;

pub const MAX_ATTEMPTS: i64 = 20;
pub const BASE_RETRY_DELAY_MS: i64 = 5_000;
pub const MAX_RETRY_DELAY_MS: i64 = 10 * 60 * 1_000;
pub const CLAIM_TIMEOUT_MS: i64 = 5 * 60 * 1_000;
pub const FAILED_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
const PRUNE_BATCH_SIZE: i64 = 128;
pub(super) const OWNERSHIP_RETRY_DELAY_MS: i64 = 15_000;

pub(super) const STATUS_QUEUED: &str = "queued";
pub(super) const STATUS_IN_PROGRESS: &str = "in-progress";
pub(super) const STATUS_DONE: &str = "done";
pub(super) const STATUS_FAILED: &str = "failed";

#[derive(Clone)]
pub struct CallTeardownOutboxStore {
    pub(super) db: Database,
    pub(super) node_identity: SharedNodeIdentity,
}

impl CallTeardownOutboxStore {
    pub async fn new(db: Database) -> Result<Self, CallTeardownOutboxError> {
        Self::new_with_node_identity(db, SharedNodeIdentity::new(NodeIdentity::local())).await
    }

    pub async fn new_with_node_identity(
        db: Database,
        node_identity: SharedNodeIdentity,
    ) -> Result<Self, CallTeardownOutboxError> {
        schema::initialize(&db).await?;
        Ok(Self { db, node_identity })
    }

    pub(super) async fn producing_node_guard(
        &self,
        required: bool,
    ) -> Result<Option<CurrentNodeIdentityGuard>, CallTeardownOutboxError> {
        if !required {
            return Ok(None);
        }
        let expected = self.node_identity.current();
        self.node_identity
            .guard_if_current(&expected)
            .await
            .map(Some)
            .ok_or(CallTeardownOutboxError::ProducingNodeIdentityChanged)
    }

    pub(crate) async fn guard_if_current_producer(
        &self,
        producer: &CallTeardownProducingNode,
    ) -> Option<CurrentNodeIdentityGuard> {
        self.node_identity
            .guard_if_current(producer.node_identity())
            .await
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
        let producing_node_guard = self
            .producing_node_guard(intents.iter().any(|intent| intent.room_scope().is_none()))
            .await?;
        let mut transaction = self.db.begin().await?;
        let mut intent_ids = Vec::with_capacity(intents.len());
        for intent in intents {
            let intent_id = CallTeardownIntentId::new();
            let (action, identity, room_jid, participant_sid) = encode_target(&intent.target);
            let generation = encode_generation(intent.generation)?;
            let producing_node =
                Self::encode_producing_node(intent, producing_node_guard.as_ref())?;
            transaction
                .execute(
                    "INSERT INTO call_teardown_outbox (\
                        intent_id, call_id, identity, room_jid, action, generation, \
                        room_sid, participant_sid, producing_node, status, attempt_count, last_error, \
                        next_attempt_at_ms, claimed_at_ms, claim_token, created_at_ms, updated_at_ms\
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, ?, NULL, NULL, ?, ?)",
                    crate::db_params![
                        intent_id.as_str(),
                        intent.call_id.as_str(),
                        identity,
                        room_jid,
                        action,
                        generation,
                        intent.room_sid.as_ref().map(RoomSid::as_str),
                        participant_sid,
                        producing_node,
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
        drop(producing_node_guard);
        Ok(intent_ids)
    }

    pub(super) async fn enqueue_at(
        &self,
        intent: CallTeardownIntent,
        now_ms: i64,
    ) -> Result<CallTeardownIntentId, CallTeardownOutboxError> {
        let producing_node_guard = self
            .producing_node_guard(intent.room_scope().is_none())
            .await?;
        let intent_id = CallTeardownIntentId::new();
        let (action, identity, room_jid, participant_sid) = encode_target(&intent.target);
        let generation = encode_generation(intent.generation)?;
        let producing_node = Self::encode_producing_node(&intent, producing_node_guard.as_ref())?;
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
                   AND producing_node IS NOT DISTINCT FROM ? \
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
                    producing_node.clone(),
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
                    room_sid, participant_sid, producing_node, status, attempt_count, last_error, \
                    next_attempt_at_ms, claimed_at_ms, claim_token, created_at_ms, updated_at_ms\
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, ?, NULL, NULL, ?, ?)",
                crate::db_params![
                    intent_id.as_str(),
                    intent.call_id.as_str(),
                    identity,
                    room_jid,
                    action,
                    generation,
                    intent.room_sid.as_ref().map(RoomSid::as_str),
                    participant_sid,
                    producing_node,
                    STATUS_QUEUED,
                    now_ms,
                    now_ms,
                    now_ms,
                ],
            )
            .await?;
        drop(producing_node_guard);
        Ok(intent_id)
    }

    fn encode_producing_node(
        intent: &CallTeardownIntent,
        guard: Option<&CurrentNodeIdentityGuard>,
    ) -> Result<Option<String>, CallTeardownOutboxError> {
        intent
            .room_scope()
            .is_none()
            .then(|| {
                let guard = guard.ok_or(CallTeardownOutboxError::ProducingNodeIdentityChanged)?;
                CallTeardownProducingNode::from_node_identity(guard.identity().clone())
                    .as_db_value()
            })
            .transpose()
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

pub(super) fn select_columns() -> &'static str {
    "SELECT intent_id, call_id, identity, room_jid, action, generation, \
            room_sid, participant_sid, producing_node, status, attempt_count, last_error, \
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
