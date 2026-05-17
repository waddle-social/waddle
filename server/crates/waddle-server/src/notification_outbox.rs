//! Durable user-server notification candidates and XEP-0357 outbox.
//!
//! This is scheduling/coalescing state. The canonical first-party XEP-0357
//! payload still becomes a XEP-0060 PubSub item on the Push Service boundary.

use jid::{BareJid, Jid};
use minidom::Element;
use thiserror::Error;
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::push::PushSubscriptionStore;
use waddle_xmpp::xep::{NS_DATA_FORMS, NS_PUBSUB_PUBLISH_OPTIONS};
use waddle_xmpp_core::xep0359::StanzaId;

use crate::db::{Database, DatabaseError, IntoParams, Row};

pub const WADDLE_PUSH_CONTEXT_NS: &str = "urn:waddle:push:context:0";
pub const XEP0357_SUMMARY_FORM_TYPE: &str = "urn:xmpp:push:summary";

const STATUS_QUEUED: &str = "queued";
const STATUS_IN_PROGRESS: &str = "in-progress";
const STATUS_PUBLISHED: &str = "published";
const STATUS_FAILED: &str = "failed";
const MAX_OUTBOX_ATTEMPTS: i64 = 5;
const BASE_RETRY_DELAY_MS: i64 = 5_000;
const MAX_RETRY_DELAY_MS: i64 = 300_000;
const OUTBOX_CLAIM_TIMEOUT_MS: i64 = 300_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationThreadId(String);

impl NotificationThreadId {
    pub fn root() -> Self {
        Self(String::new())
    }

    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushServiceNodeName(String);

impl PushServiceNodeName {
    pub fn new(value: impl Into<String>) -> Result<Self, NotificationOutboxError> {
        let value = value.into();
        if value.is_empty() {
            return Err(NotificationOutboxError::InvalidNode);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationOutboxJobId(String);

impl NotificationOutboxJobId {
    fn fresh() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for NotificationOutboxJobId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationClass {
    DirectMessage,
    PersonalMention,
    ChannelMention,
    ActiveChannelMention,
    NotifyAll,
}

impl NotificationClass {
    fn as_db_value(self) -> &'static str {
        match self {
            Self::DirectMessage => "dm",
            Self::PersonalMention => "personal_mention",
            Self::ChannelMention => "channel_mention",
            Self::ActiveChannelMention => "active_channel_mention",
            Self::NotifyAll => "notify_all",
        }
    }

    fn from_db_value(value: &str) -> Result<Self, NotificationOutboxError> {
        match value {
            "dm" => Ok(Self::DirectMessage),
            "personal_mention" => Ok(Self::PersonalMention),
            "channel_mention" => Ok(Self::ChannelMention),
            "active_channel_mention" => Ok(Self::ActiveChannelMention),
            "notify_all" => Ok(Self::NotifyAll),
            _ => Err(NotificationOutboxError::InvalidClass(value.to_string())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationReason {
    OfflineDirectMessage,
}

impl NotificationReason {
    fn as_db_value(self) -> &'static str {
        match self {
            Self::OfflineDirectMessage => "offline_dm",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationOutboxStatus {
    Queued,
    InProgress,
    Published,
    Failed,
}

impl NotificationOutboxStatus {
    fn from_db_value(value: &str) -> Result<Self, NotificationOutboxError> {
        match value {
            STATUS_QUEUED => Ok(Self::Queued),
            STATUS_IN_PROGRESS => Ok(Self::InProgress),
            STATUS_PUBLISHED => Ok(Self::Published),
            STATUS_FAILED => Ok(Self::Failed),
            _ => Err(NotificationOutboxError::InvalidStatus(value.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationCandidate {
    recipient_bare_jid: BareJid,
    conversation_jid: BareJid,
    thread_id: NotificationThreadId,
    archive_stanza_id: StanzaId,
    class: NotificationClass,
    reason: NotificationReason,
}

impl NotificationCandidate {
    pub fn direct_message(
        recipient_bare_jid: BareJid,
        sender_bare_jid: BareJid,
        archive_stanza_id: StanzaId,
    ) -> Result<Self, NotificationOutboxError> {
        let expected_by = Jid::from(recipient_bare_jid.clone());
        if archive_stanza_id.by != expected_by {
            return Err(NotificationOutboxError::ArchiveStanzaIdOwnerMismatch {
                expected: expected_by,
                actual: archive_stanza_id.by,
            });
        }
        Ok(Self {
            recipient_bare_jid,
            conversation_jid: sender_bare_jid,
            thread_id: NotificationThreadId::root(),
            archive_stanza_id,
            class: NotificationClass::DirectMessage,
            reason: NotificationReason::OfflineDirectMessage,
        })
    }

    pub fn recipient_bare_jid(&self) -> &BareJid {
        &self.recipient_bare_jid
    }

    pub fn conversation_jid(&self) -> &BareJid {
        &self.conversation_jid
    }

    pub fn thread_id(&self) -> &NotificationThreadId {
        &self.thread_id
    }

    pub fn archive_stanza_id(&self) -> &StanzaId {
        &self.archive_stanza_id
    }

    pub fn class(&self) -> NotificationClass {
        self.class
    }

    pub fn reason(&self) -> NotificationReason {
        self.reason
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationOutboxTarget {
    push_service_jid: BareJid,
    node: PushServiceNodeName,
}

impl NotificationOutboxTarget {
    pub fn new(push_service_jid: BareJid, node: PushServiceNodeName) -> NotificationOutboxTarget {
        Self {
            push_service_jid,
            node,
        }
    }

    pub fn push_service_jid(&self) -> &BareJid {
        &self.push_service_jid
    }

    pub fn node(&self) -> &PushServiceNodeName {
        &self.node
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationOutboxJob {
    job_id: NotificationOutboxJobId,
    recipient_bare_jid: BareJid,
    push_service_jid: BareJid,
    node: PushServiceNodeName,
    conversation_jid: BareJid,
    thread_id: NotificationThreadId,
    class: NotificationClass,
    message_count: u32,
    context: Element,
    status: NotificationOutboxStatus,
    attempt_count: i64,
}

impl NotificationOutboxJob {
    pub fn job_id(&self) -> &NotificationOutboxJobId {
        &self.job_id
    }

    pub fn recipient_bare_jid(&self) -> &BareJid {
        &self.recipient_bare_jid
    }

    pub fn push_service_jid(&self) -> &BareJid {
        &self.push_service_jid
    }

    pub fn node(&self) -> &PushServiceNodeName {
        &self.node
    }

    pub fn conversation_jid(&self) -> &BareJid {
        &self.conversation_jid
    }

    pub fn thread_id(&self) -> &NotificationThreadId {
        &self.thread_id
    }

    pub fn class(&self) -> NotificationClass {
        self.class
    }

    pub fn message_count(&self) -> u32 {
        self.message_count
    }

    pub fn context(&self) -> &Element {
        &self.context
    }

    pub fn status(&self) -> NotificationOutboxStatus {
        self.status
    }

    pub fn attempt_count(&self) -> i64 {
        self.attempt_count
    }

    pub fn to_xep0357_pubsub_item(&self) -> PubSubItem {
        PubSubItem::new(
            Some(self.job_id.as_str().to_string()),
            Some(build_xep0357_notification_payload(
                self.message_count,
                &self.context,
            )),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationCandidateInsertOutcome {
    Inserted { enqueued_jobs: usize },
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationOutboxPublishOutcome {
    Published {
        job_id: NotificationOutboxJobId,
        item_id: String,
    },
    RetryScheduled {
        job_id: NotificationOutboxJobId,
    },
    Failed {
        job_id: NotificationOutboxJobId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationOutboxPruneOutcome {
    pub candidates_deleted: u64,
    pub jobs_deleted: u64,
}

impl NotificationOutboxPruneOutcome {
    pub fn total_deleted(self) -> u64 {
        self.candidates_deleted + self.jobs_deleted
    }
}

#[derive(Debug, Error)]
pub enum NotificationOutboxError {
    #[error("database error: {0}")]
    Database(#[from] DatabaseError),
    #[error("push error: {0}")]
    Push(String),
    #[error("XMPP error: {0}")]
    Xmpp(String),
    #[error("invalid push service node")]
    InvalidNode,
    #[error("invalid candidate class: {0}")]
    InvalidClass(String),
    #[error("invalid candidate reason: {0}")]
    InvalidReason(String),
    #[error("invalid outbox status: {0}")]
    InvalidStatus(String),
    #[error("invalid recipient bare JID in notification outbox: {0}")]
    InvalidRecipientBareJid(String),
    #[error("invalid push service bare JID in notification outbox: {0}")]
    InvalidPushServiceBareJid(String),
    #[error("invalid conversation JID in notification outbox: {0}")]
    InvalidConversationJid(String),
    #[error("invalid archive stanza-id by JID in notification candidate: {0}")]
    InvalidArchiveStanzaIdBy(String),
    #[error("invalid stored notification context XML: {0}")]
    InvalidContextXml(String),
    #[error("archive stanza-id by mismatch: expected {expected}, got {actual}")]
    ArchiveStanzaIdOwnerMismatch { expected: Jid, actual: Jid },
    #[error("message count is out of range: {0}")]
    InvalidMessageCount(i64),
}

#[derive(Clone)]
pub struct NotificationOutboxStore {
    db: Database,
}

impl NotificationOutboxStore {
    pub async fn new(db: Database) -> Result<Self, NotificationOutboxError> {
        let store = Self { db };
        store.initialize().await?;
        Ok(store)
    }

    async fn initialize(&self) -> Result<(), NotificationOutboxError> {
        let i64_type = crate::db::i64_sql_type(self.db.driver());
        self.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS notification_candidates (
                    recipient_bare_jid TEXT NOT NULL,
                    conversation_jid TEXT NOT NULL,
                    thread_id TEXT NOT NULL DEFAULT '',
                    stanza_id_by TEXT NOT NULL,
                    stanza_id TEXT NOT NULL,
                    class TEXT NOT NULL CHECK (class IN ('dm', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
                    reason TEXT NOT NULL CHECK (reason IN ('offline_dm')),
                    created_at_ms {i64_type} NOT NULL,
                    outboxed_at_ms {i64_type},
                    PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
                )
                "#
            ),
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_candidates_recipient_created \
             ON notification_candidates (recipient_bare_jid, created_at_ms)",
            (),
        )
        .await?;
        self.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS notification_outbox (
                    job_id TEXT PRIMARY KEY,
                    recipient_bare_jid TEXT NOT NULL,
                    push_service_jid TEXT NOT NULL,
                    node TEXT NOT NULL,
                    conversation_jid TEXT NOT NULL,
                    thread_id TEXT NOT NULL DEFAULT '',
                    class TEXT NOT NULL CHECK (class IN ('dm', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
                    message_count INTEGER NOT NULL,
                    context_xml TEXT NOT NULL,
                    status TEXT NOT NULL CHECK (status IN ('queued', 'in-progress', 'published', 'failed')),
                    attempt_count INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    next_attempt_at_ms {i64_type},
                    claimed_at_ms {i64_type},
                    created_at_ms {i64_type} NOT NULL,
                    updated_at_ms {i64_type} NOT NULL,
                    published_at_ms {i64_type}
                )
                "#
            ),
            (),
        )
        .await?;
        self.execute(
            "DROP INDEX IF EXISTS idx_notification_outbox_active_coalesce",
            (),
        )
        .await?;
        self.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_notification_outbox_queued_coalesce \
             ON notification_outbox (recipient_bare_jid, push_service_jid, node, conversation_jid, thread_id, class) \
             WHERE status = 'queued'",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_outbox_status_next_attempt \
             ON notification_outbox (status, next_attempt_at_ms, created_at_ms)",
            (),
        )
        .await?;
        Ok(())
    }

    async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, NotificationOutboxError> {
        let conn = self.db.guard().await?;
        Ok(conn.execute(sql, params).await?)
    }

    async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, NotificationOutboxError> {
        let conn = self.db.guard().await?;
        Ok(conn.query(sql, params).await?)
    }

    pub async fn insert_candidate_and_enqueue(
        &self,
        candidate: &NotificationCandidate,
        targets: &[NotificationOutboxTarget],
    ) -> Result<NotificationCandidateInsertOutcome, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let mut tx = self.db.begin().await?;
        let inserted = tx
            .execute(
                r#"
                INSERT INTO notification_candidates (
                    recipient_bare_jid,
                    conversation_jid,
                    thread_id,
                    stanza_id_by,
                    stanza_id,
                    class,
                    reason,
                    created_at_ms,
                    outboxed_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT DO NOTHING
                "#,
                crate::db_params![
                    candidate.recipient_bare_jid.to_string(),
                    candidate.conversation_jid.to_string(),
                    candidate.thread_id.as_str(),
                    candidate.archive_stanza_id.by.to_string(),
                    candidate.archive_stanza_id.id.clone(),
                    candidate.class.as_db_value(),
                    candidate.reason.as_db_value(),
                    now_ms,
                    now_ms,
                ],
            )
            .await?;
        if inserted == 0 {
            tx.commit().await?;
            return Ok(NotificationCandidateInsertOutcome::Duplicate);
        }

        let context = build_waddle_context(candidate);
        let context_xml = String::from(&context);
        let mut enqueued_jobs = 0usize;
        for target in targets {
            enqueue_outbox_job_tx(&mut tx, candidate, target, &context_xml, now_ms).await?;
            enqueued_jobs += 1;
        }
        tx.commit().await?;
        Ok(NotificationCandidateInsertOutcome::Inserted { enqueued_jobs })
    }

    pub async fn pending_outbox_jobs(
        &self,
    ) -> Result<Vec<NotificationOutboxJob>, NotificationOutboxError> {
        let mut rows = self
            .query(
                r#"
                SELECT job_id,
                       recipient_bare_jid,
                       push_service_jid,
                       node,
                       conversation_jid,
                       thread_id,
                       class,
                       message_count,
                       context_xml,
                       status,
                       attempt_count
                FROM notification_outbox
                WHERE status IN (?, ?)
                ORDER BY created_at_ms ASC, job_id ASC
                "#,
                crate::db_params![STATUS_QUEUED, STATUS_IN_PROGRESS],
            )
            .await?;
        let mut jobs = Vec::new();
        while let Some(row) = rows.next().await? {
            jobs.push(decode_outbox_job(&row)?);
        }
        Ok(jobs)
    }

    pub async fn claim_due_outbox_jobs(
        &self,
        batch_size: usize,
    ) -> Result<Vec<NotificationOutboxJob>, NotificationOutboxError> {
        let batch_size = batch_size.clamp(1, 1_000);
        let now_ms = crate::time::now_ms();
        let stale_claimed_before_ms = now_ms.saturating_sub(OUTBOX_CLAIM_TIMEOUT_MS);
        let mut rows = self
            .query(
                r#"
                SELECT job_id,
                       recipient_bare_jid,
                       push_service_jid,
                       node,
                       conversation_jid,
                       thread_id,
                       class,
                       message_count,
                       context_xml,
                       status,
                       attempt_count
                FROM notification_outbox
                WHERE (
                    status = ?
                    AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)
                ) OR (
                    status = ?
                    AND claimed_at_ms IS NOT NULL
                    AND claimed_at_ms <= ?
                )
                ORDER BY created_at_ms ASC, job_id ASC
                LIMIT ?
                "#,
                crate::db_params![
                    STATUS_QUEUED,
                    now_ms,
                    STATUS_IN_PROGRESS,
                    stale_claimed_before_ms,
                    batch_size,
                ],
            )
            .await?;
        let mut selected = Vec::new();
        while let Some(row) = rows.next().await? {
            selected.push(decode_outbox_job(&row)?);
        }

        let mut claimed = Vec::new();
        for job in selected {
            let affected = self
                .execute(
                    r#"
                    UPDATE notification_outbox
                    SET status = ?,
                        claimed_at_ms = ?,
                        updated_at_ms = ?
                    WHERE job_id = ?
                      AND (
                        (
                            status = ?
                            AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)
                        ) OR (
                            status = ?
                            AND claimed_at_ms IS NOT NULL
                            AND claimed_at_ms <= ?
                        )
                      )
                    "#,
                    crate::db_params![
                        STATUS_IN_PROGRESS,
                        now_ms,
                        now_ms,
                        job.job_id.as_str(),
                        STATUS_QUEUED,
                        now_ms,
                        STATUS_IN_PROGRESS,
                        stale_claimed_before_ms,
                    ],
                )
                .await?;
            if affected > 0 {
                claimed.push(NotificationOutboxJob {
                    status: NotificationOutboxStatus::InProgress,
                    ..job
                });
            }
        }
        Ok(claimed)
    }

    pub async fn drain_due_outbox_jobs(
        &self,
        push_service: &crate::push_service::DatabasePushServiceStore,
        push_store: &dyn PushSubscriptionStore,
        first_party_service_jid: &BareJid,
        batch_size: usize,
    ) -> Result<Vec<NotificationOutboxPublishOutcome>, NotificationOutboxError> {
        let jobs = self.claim_due_outbox_jobs(batch_size).await?;
        let mut outcomes = Vec::with_capacity(jobs.len());
        for job in jobs {
            outcomes.push(
                self.publish_claimed_job(&job, push_service, push_store, first_party_service_jid)
                    .await?,
            );
        }
        Ok(outcomes)
    }

    pub async fn prune_completed_before(
        &self,
        cutoff_ms: i64,
        batch_size: usize,
    ) -> Result<NotificationOutboxPruneOutcome, NotificationOutboxError> {
        let batch_size = batch_size.clamp(1, 10_000);
        let candidates_deleted = self
            .prune_outboxed_candidates_before(cutoff_ms, batch_size)
            .await?;
        let jobs_deleted = self
            .execute(
                r#"
                DELETE FROM notification_outbox
                WHERE job_id IN (
                    SELECT job_id
                    FROM notification_outbox
                    WHERE status IN (?, ?)
                      AND updated_at_ms < ?
                    ORDER BY updated_at_ms ASC, job_id ASC
                    LIMIT ?
                )
                "#,
                crate::db_params![
                    STATUS_PUBLISHED,
                    STATUS_FAILED,
                    cutoff_ms,
                    batch_size as i64,
                ],
            )
            .await?;
        Ok(NotificationOutboxPruneOutcome {
            candidates_deleted,
            jobs_deleted,
        })
    }

    async fn prune_outboxed_candidates_before(
        &self,
        cutoff_ms: i64,
        batch_size: usize,
    ) -> Result<u64, NotificationOutboxError> {
        let mut rows = self
            .query(
                r#"
                SELECT recipient_bare_jid,
                       conversation_jid,
                       thread_id,
                       stanza_id_by,
                       stanza_id,
                       class
                FROM notification_candidates
                WHERE outboxed_at_ms IS NOT NULL
                  AND outboxed_at_ms < ?
                ORDER BY outboxed_at_ms ASC,
                         recipient_bare_jid ASC,
                         conversation_jid ASC,
                         thread_id ASC,
                         stanza_id_by ASC,
                         stanza_id ASC,
                         class ASC
                LIMIT ?
                "#,
                crate::db_params![cutoff_ms, batch_size as i64],
            )
            .await?;
        let mut keys = Vec::new();
        while let Some(row) = rows.next().await? {
            keys.push((
                row.get::<String>(0)?,
                row.get::<String>(1)?,
                row.get::<String>(2)?,
                row.get::<String>(3)?,
                row.get::<String>(4)?,
                row.get::<String>(5)?,
            ));
        }
        if keys.is_empty() {
            return Ok(0);
        }

        let mut tx = self.db.begin().await?;
        let mut deleted = 0u64;
        for (recipient, conversation, thread, stanza_by, stanza_id, class) in keys {
            deleted += tx
                .execute(
                    r#"
                    DELETE FROM notification_candidates
                    WHERE recipient_bare_jid = ?
                      AND conversation_jid = ?
                      AND thread_id = ?
                      AND stanza_id_by = ?
                      AND stanza_id = ?
                      AND class = ?
                    "#,
                    crate::db_params![
                        recipient,
                        conversation,
                        thread,
                        stanza_by,
                        stanza_id,
                        class,
                    ],
                )
                .await?;
        }
        tx.commit().await?;
        Ok(deleted)
    }

    async fn publish_claimed_job(
        &self,
        job: &NotificationOutboxJob,
        push_service: &crate::push_service::DatabasePushServiceStore,
        push_store: &dyn PushSubscriptionStore,
        first_party_service_jid: &BareJid,
    ) -> Result<NotificationOutboxPublishOutcome, NotificationOutboxError> {
        if job.push_service_jid() != first_party_service_jid {
            self.mark_job_failed(
                job,
                "notification outbox job targets a non-first-party XEP-0357 Push Service",
            )
            .await?;
            return Ok(NotificationOutboxPublishOutcome::Failed {
                job_id: job.job_id.clone(),
            });
        }

        let registrations = push_store
            .get_for_user(&job.recipient_bare_jid.to_string())
            .await
            .map_err(|error| NotificationOutboxError::Push(error.to_string()))?;
        let service = job.push_service_jid.to_string();
        let registration = registrations.into_iter().find(|registration| {
            registration.service_jid == service
                && registration.node.as_deref() == Some(job.node.as_str())
        });
        let Some(registration) = registration else {
            self.mark_job_failed(job, "first-party XEP-0357 registration is no longer active")
                .await?;
            return Ok(NotificationOutboxPublishOutcome::Failed {
                job_id: job.job_id.clone(),
            });
        };

        let item = job.to_xep0357_pubsub_item();
        let push_service_jid = job.push_service_jid.to_string();
        match push_service
            .enqueue_registered_notification_from_user_server_with_publish_options(
                push_service_jid.as_str(),
                job.node.as_str(),
                &item,
                &job.recipient_bare_jid,
                registration.publish_options.as_ref(),
            )
            .await
        {
            Ok(result) => {
                self.mark_job_published(job).await?;
                Ok(NotificationOutboxPublishOutcome::Published {
                    job_id: job.job_id.clone(),
                    item_id: result.item_id().to_string(),
                })
            }
            Err(error) => {
                let attempts = self.schedule_retry_or_fail(job, error.to_string()).await?;
                if attempts >= MAX_OUTBOX_ATTEMPTS {
                    Ok(NotificationOutboxPublishOutcome::Failed {
                        job_id: job.job_id.clone(),
                    })
                } else {
                    Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                        job_id: job.job_id.clone(),
                    })
                }
            }
        }
    }

    async fn mark_job_published(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<(), NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        self.execute(
            r#"
            UPDATE notification_outbox
            SET status = ?,
                last_error = NULL,
                next_attempt_at_ms = NULL,
                claimed_at_ms = NULL,
                updated_at_ms = ?,
                published_at_ms = ?
            WHERE job_id = ?
            "#,
            crate::db_params![STATUS_PUBLISHED, now_ms, now_ms, job.job_id.as_str()],
        )
        .await?;
        Ok(())
    }

    async fn mark_job_failed(
        &self,
        job: &NotificationOutboxJob,
        error: &str,
    ) -> Result<(), NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        self.execute(
            r#"
            UPDATE notification_outbox
            SET status = ?,
                last_error = ?,
                next_attempt_at_ms = NULL,
                claimed_at_ms = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
            "#,
            crate::db_params![STATUS_FAILED, error, now_ms, job.job_id.as_str()],
        )
        .await?;
        Ok(())
    }

    async fn schedule_retry_or_fail(
        &self,
        job: &NotificationOutboxJob,
        error: String,
    ) -> Result<i64, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let next_attempt_count = job.attempt_count + 1;
        let (status, next_attempt_at_ms) = if next_attempt_count >= MAX_OUTBOX_ATTEMPTS {
            (STATUS_FAILED, None)
        } else {
            (
                STATUS_QUEUED,
                Some(now_ms + retry_delay_ms(next_attempt_count)),
            )
        };
        self.execute(
            r#"
            UPDATE notification_outbox
            SET status = ?,
                attempt_count = ?,
                last_error = ?,
                next_attempt_at_ms = ?,
                claimed_at_ms = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
            "#,
            crate::db_params![
                status,
                next_attempt_count,
                error,
                next_attempt_at_ms,
                now_ms,
                job.job_id.as_str(),
            ],
        )
        .await?;
        Ok(next_attempt_count)
    }
}

async fn enqueue_outbox_job_tx(
    tx: &mut crate::db::Transaction<'_>,
    candidate: &NotificationCandidate,
    target: &NotificationOutboxTarget,
    context_xml: &str,
    now_ms: i64,
) -> Result<(), NotificationOutboxError> {
    let job_id = NotificationOutboxJobId::fresh();
    let inserted = tx
        .execute(
            r#"
            INSERT INTO notification_outbox (
                job_id,
                recipient_bare_jid,
                push_service_jid,
                node,
                conversation_jid,
                thread_id,
                class,
                message_count,
                context_xml,
                status,
                attempt_count,
                last_error,
                next_attempt_at_ms,
                claimed_at_ms,
                created_at_ms,
                updated_at_ms,
                published_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, 0, NULL, NULL, NULL, ?, ?, NULL)
            ON CONFLICT DO NOTHING
            "#,
            crate::db_params![
                job_id.as_str(),
                candidate.recipient_bare_jid.to_string(),
                target.push_service_jid.to_string(),
                target.node.as_str(),
                candidate.conversation_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.class.as_db_value(),
                context_xml,
                STATUS_QUEUED,
                now_ms,
                now_ms,
            ],
        )
        .await?;
    if inserted == 0 {
        tx.execute(
            r#"
            UPDATE notification_outbox
            SET message_count = message_count + 1,
                context_xml = ?,
                updated_at_ms = ?
            WHERE recipient_bare_jid = ?
              AND push_service_jid = ?
              AND node = ?
              AND conversation_jid = ?
              AND thread_id = ?
              AND class = ?
              AND status = ?
            "#,
            crate::db_params![
                context_xml,
                now_ms,
                candidate.recipient_bare_jid.to_string(),
                target.push_service_jid.to_string(),
                target.node.as_str(),
                candidate.conversation_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.class.as_db_value(),
                STATUS_QUEUED,
            ],
        )
        .await?;
    }
    Ok(())
}

fn decode_outbox_job(row: &Row) -> Result<NotificationOutboxJob, NotificationOutboxError> {
    let recipient_raw: String = row.get(1)?;
    let push_service_raw: String = row.get(2)?;
    let conversation_raw: String = row.get(4)?;
    let message_count: i64 = row.get(7)?;
    let context_xml: String = row.get(8)?;
    Ok(NotificationOutboxJob {
        job_id: NotificationOutboxJobId::from(row.get::<String>(0)?),
        recipient_bare_jid: recipient_raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidRecipientBareJid(recipient_raw))?,
        push_service_jid: push_service_raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidPushServiceBareJid(push_service_raw))?,
        node: PushServiceNodeName::new(row.get::<String>(3)?)?,
        conversation_jid: conversation_raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidConversationJid(conversation_raw))?,
        thread_id: NotificationThreadId::new(row.get::<String>(5)?),
        class: NotificationClass::from_db_value(&row.get::<String>(6)?)?,
        message_count: u32::try_from(message_count)
            .map_err(|_| NotificationOutboxError::InvalidMessageCount(message_count))?,
        context: context_xml
            .parse::<Element>()
            .map_err(|error| NotificationOutboxError::InvalidContextXml(error.to_string()))?,
        status: NotificationOutboxStatus::from_db_value(&row.get::<String>(9)?)?,
        attempt_count: row.get(10)?,
    })
}

fn build_waddle_context(candidate: &NotificationCandidate) -> Element {
    Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
        .attr("conversation", candidate.conversation_jid.to_string())
        .attr("thread", candidate.thread_id.as_str())
        .attr("class", candidate.class.as_db_value())
        .build()
}

pub fn build_xep0357_notification_payload(message_count: u32, context: &Element) -> Element {
    Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH)
        .append(build_xep0357_summary_form(message_count))
        .append(context.clone())
        .build()
}

fn build_xep0357_summary_form(message_count: u32) -> Element {
    Element::builder("x", NS_DATA_FORMS)
        .append(xdata_field("FORM_TYPE", XEP0357_SUMMARY_FORM_TYPE))
        .append(xdata_field("message-count", &message_count.to_string()))
        .build()
}

fn xdata_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr("var", var)
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(value)
                .build(),
        )
        .build()
}

fn retry_delay_ms(attempt_count: i64) -> i64 {
    let exponent = (attempt_count - 1).clamp(0, 10) as u32;
    BASE_RETRY_DELAY_MS
        .saturating_mul(2_i64.saturating_pow(exponent))
        .min(MAX_RETRY_DELAY_MS)
}

pub fn target_from_subscription(
    subscription: &waddle_xmpp::push::PushSubscription,
) -> Result<Option<NotificationOutboxTarget>, NotificationOutboxError> {
    let Some(node) = subscription.node.as_ref() else {
        return Ok(None);
    };
    let push_service_jid = subscription.service_jid.parse::<BareJid>().map_err(|_| {
        NotificationOutboxError::InvalidPushServiceBareJid(subscription.service_jid.clone())
    })?;
    Ok(Some(NotificationOutboxTarget::new(
        push_service_jid,
        PushServiceNodeName::new(node.clone())?,
    )))
}

pub fn publish_options_form_type_is_xep0060(publish_options: &Element) -> bool {
    publish_options.children().any(|child| {
        child.is("field", NS_DATA_FORMS)
            && child.attr("var") == Some("FORM_TYPE")
            && child.children().any(|value| {
                value.is("value", NS_DATA_FORMS) && value.text() == NS_PUBSUB_PUBLISH_OPTIONS
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn bare(raw: &str) -> BareJid {
        raw.parse().expect("bare jid")
    }

    fn candidate(id: &str) -> NotificationCandidate {
        let recipient = bare("alice@example.com");
        NotificationCandidate::direct_message(
            recipient.clone(),
            bare("bob@example.com"),
            StanzaId::new(id, Jid::from(recipient)),
        )
        .expect("candidate")
    }

    fn target() -> NotificationOutboxTarget {
        NotificationOutboxTarget::new(
            bare("push.example.com"),
            PushServiceNodeName::new("web-node").expect("node"),
        )
    }

    fn foreign_target() -> NotificationOutboxTarget {
        NotificationOutboxTarget::new(
            bare("push-provider.example.com"),
            PushServiceNodeName::new("web-node").expect("node"),
        )
    }

    async fn store() -> NotificationOutboxStore {
        NotificationOutboxStore::new(Database::in_memory("notification-outbox").await.unwrap())
            .await
            .expect("store")
    }

    #[tokio::test]
    async fn candidate_insert_is_idempotent_and_coalesces_distinct_messages() {
        let store = store().await;
        let target = target();
        let first = candidate("archive-1");
        let duplicate = candidate("archive-1");
        let second = candidate("archive-2");

        assert_eq!(
            store
                .insert_candidate_and_enqueue(&first, std::slice::from_ref(&target))
                .await
                .expect("first insert"),
            NotificationCandidateInsertOutcome::Inserted { enqueued_jobs: 1 }
        );
        assert_eq!(
            store
                .insert_candidate_and_enqueue(&duplicate, std::slice::from_ref(&target))
                .await
                .expect("duplicate insert"),
            NotificationCandidateInsertOutcome::Duplicate
        );
        assert_eq!(
            store
                .insert_candidate_and_enqueue(&second, std::slice::from_ref(&target))
                .await
                .expect("second insert"),
            NotificationCandidateInsertOutcome::Inserted { enqueued_jobs: 1 }
        );

        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].message_count(), 2);
        assert_eq!(jobs[0].conversation_jid(), &bare("bob@example.com"));
        assert_eq!(jobs[0].class(), NotificationClass::DirectMessage);
    }

    #[test]
    fn xep0357_payload_uses_summary_form_and_waddle_context_only() {
        let context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
            .attr("conversation", "bob@example.com")
            .attr("thread", "")
            .attr("class", "dm")
            .build();
        let payload = build_xep0357_notification_payload(3, &context);

        assert!(payload.is("notification", waddle_xmpp::xep::xep0357::NS_PUSH));
        let summary = payload
            .children()
            .find(|child| child.is("x", NS_DATA_FORMS))
            .expect("summary form");
        assert!(summary.children().any(|field| {
            field.is("field", NS_DATA_FORMS)
                && field.attr("var") == Some("FORM_TYPE")
                && field.children().any(|value| {
                    value.is("value", NS_DATA_FORMS) && value.text() == XEP0357_SUMMARY_FORM_TYPE
                })
        }));
        assert!(summary.children().any(|field| {
            field.is("field", NS_DATA_FORMS)
                && field.attr("var") == Some("message-count")
                && field
                    .children()
                    .any(|value| value.is("value", NS_DATA_FORMS) && value.text() == "3")
        }));
        assert!(!summary.children().any(|field| {
            matches!(
                field.attr("var"),
                Some("last-message-body" | "last-message-sender")
            )
        }));
        let context = payload
            .children()
            .find(|child| child.is("context", WADDLE_PUSH_CONTEXT_NS))
            .expect("waddle context");
        assert_eq!(context.attr("conversation"), Some("bob@example.com"));
        assert_eq!(context.attr("class"), Some("dm"));
    }

    #[tokio::test]
    async fn claimed_outbox_job_builds_stable_xep0357_pubsub_item() {
        let store = store().await;
        let target = target();
        let candidate = candidate("archive-1");
        store
            .insert_candidate_and_enqueue(&candidate, &[target])
            .await
            .expect("insert");

        let jobs = store.claim_due_outbox_jobs(16).await.expect("claim");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status(), NotificationOutboxStatus::InProgress);
        let item = jobs[0].to_xep0357_pubsub_item();
        assert_eq!(item.id.as_deref(), Some(jobs[0].job_id().as_str()));
        let payload = item.payload.expect("payload");
        assert!(payload.is("notification", waddle_xmpp::xep::xep0357::NS_PUSH));
    }

    #[tokio::test]
    async fn stale_in_progress_outbox_job_is_claimable_again() {
        let store = store().await;
        let target = target();
        let candidate = candidate("archive-1");
        store
            .insert_candidate_and_enqueue(&candidate, &[target])
            .await
            .expect("insert");

        let first_claim = store.claim_due_outbox_jobs(16).await.expect("first claim");
        assert_eq!(first_claim.len(), 1);
        let immediate_claim = store
            .claim_due_outbox_jobs(16)
            .await
            .expect("immediate claim");
        assert!(immediate_claim.is_empty());

        let stale_claimed_at_ms = crate::time::now_ms()
            .saturating_sub(OUTBOX_CLAIM_TIMEOUT_MS)
            .saturating_sub(1);
        store
            .execute(
                "UPDATE notification_outbox SET claimed_at_ms = ? WHERE job_id = ?",
                crate::db_params![stale_claimed_at_ms, first_claim[0].job_id().as_str()],
            )
            .await
            .expect("make claim stale");

        let reclaimed = store.claim_due_outbox_jobs(16).await.expect("reclaim");
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].job_id(), first_claim[0].job_id());
        assert_eq!(reclaimed[0].status(), NotificationOutboxStatus::InProgress);
    }

    #[tokio::test]
    async fn new_candidate_after_claim_creates_fresh_queued_job() {
        let store = store().await;
        let target = target();
        store
            .insert_candidate_and_enqueue(&candidate("archive-1"), std::slice::from_ref(&target))
            .await
            .expect("first insert");

        let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].message_count(), 1);
        store
            .insert_candidate_and_enqueue(&candidate("archive-2"), &[target])
            .await
            .expect("second insert");

        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 2);
        assert!(jobs
            .iter()
            .any(|job| job.status() == NotificationOutboxStatus::InProgress));
        let queued = jobs
            .iter()
            .find(|job| job.status() == NotificationOutboxStatus::Queued)
            .expect("fresh queued job");
        assert_eq!(queued.message_count(), 1);
        assert_ne!(queued.job_id(), claimed[0].job_id());
    }

    #[tokio::test]
    async fn publish_rejects_non_first_party_outbox_target() {
        let store = store().await;
        store
            .insert_candidate_and_enqueue(&candidate("archive-1"), &[foreign_target()])
            .await
            .expect("insert foreign target job");
        let push_service = crate::push_service::DatabasePushServiceStore::new(
            Database::in_memory("push-service").await.unwrap(),
        )
        .await
        .expect("push service");
        let push_store = crate::push_registrations::DatabasePushRegistrationStore::new(
            Database::in_memory("push-regs").await.unwrap(),
        )
        .await
        .expect("push registrations");

        let outcomes = store
            .drain_due_outbox_jobs(&push_service, &push_store, &bare("push.example.com"), 16)
            .await
            .expect("drain outbox");

        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            outcomes[0],
            NotificationOutboxPublishOutcome::Failed { .. }
        ));
        assert!(
            push_service
                .queued_publish_jobs()
                .await
                .expect("queued push jobs")
                .is_empty(),
            "foreign outbox target must not enqueue a first-party Push Service job"
        );
    }

    #[tokio::test]
    async fn prune_completed_removes_only_finished_jobs_and_outboxed_candidates() {
        let store = store().await;
        store
            .insert_candidate_and_enqueue(&candidate("archive-old"), &[target()])
            .await
            .expect("old insert");
        let old_job = store
            .claim_due_outbox_jobs(16)
            .await
            .expect("claim old job")
            .into_iter()
            .next()
            .expect("old job");
        store.mark_job_published(&old_job).await.expect("published");

        store
            .insert_candidate_and_enqueue(&candidate("archive-live"), &[target()])
            .await
            .expect("live insert");
        let cutoff_ms = crate::time::now_ms().saturating_sub(1_000);
        let old_ms = cutoff_ms.saturating_sub(1);
        store
            .execute(
                "UPDATE notification_candidates SET outboxed_at_ms = ? WHERE stanza_id = ?",
                crate::db_params![old_ms, "archive-old"],
            )
            .await
            .expect("age old candidate");
        store
            .execute(
                "UPDATE notification_outbox SET updated_at_ms = ? WHERE job_id = ?",
                crate::db_params![old_ms, old_job.job_id().as_str()],
            )
            .await
            .expect("age old job");

        let pruned = store
            .prune_completed_before(cutoff_ms, 100)
            .await
            .expect("prune");

        assert_eq!(pruned.candidates_deleted, 1);
        assert_eq!(pruned.jobs_deleted, 1);
        let mut candidate_count = store
            .query("SELECT COUNT(*) FROM notification_candidates", ())
            .await
            .expect("candidate count query");
        let candidate_row = candidate_count
            .next()
            .await
            .expect("candidate count row")
            .expect("candidate count");
        assert_eq!(candidate_row.get::<i64>(0).expect("candidate count"), 1);
        let pending_jobs = store.pending_outbox_jobs().await.expect("pending jobs");
        assert_eq!(pending_jobs.len(), 1);
        assert_eq!(pending_jobs[0].status(), NotificationOutboxStatus::Queued);
    }
}
