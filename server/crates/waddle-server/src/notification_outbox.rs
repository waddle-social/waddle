//! Durable user-server notification candidates and XEP-0357 outbox.
//!
//! This is scheduling/coalescing state. The canonical first-party XEP-0357
//! payload still becomes a XEP-0060 PubSub item on the Push Service boundary.

use jid::{BareJid, Jid};
use minidom::Element;
use thiserror::Error;
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::push::PushSubscriptionStore;
use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};
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
const BASE_POLICY_RETRY_DELAY_MS: i64 = 60_000;
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
    GroupchatPersonalMention,
    GroupchatChannelMention,
    GroupchatActiveChannelMention,
    GroupchatNotifyAll,
}

impl NotificationReason {
    fn as_db_value(self) -> &'static str {
        match self {
            Self::OfflineDirectMessage => "offline_dm",
            Self::GroupchatPersonalMention => "groupchat_personal_mention",
            Self::GroupchatChannelMention => "groupchat_channel_mention",
            Self::GroupchatActiveChannelMention => "groupchat_active_channel_mention",
            Self::GroupchatNotifyAll => "groupchat_notify_all",
        }
    }

    fn from_db_value(value: &str) -> Result<Self, NotificationOutboxError> {
        match value {
            "offline_dm" => Ok(Self::OfflineDirectMessage),
            "groupchat_personal_mention" => Ok(Self::GroupchatPersonalMention),
            "groupchat_channel_mention" => Ok(Self::GroupchatChannelMention),
            "groupchat_active_channel_mention" => Ok(Self::GroupchatActiveChannelMention),
            "groupchat_notify_all" => Ok(Self::GroupchatNotifyAll),
            _ => Err(NotificationOutboxError::InvalidReason(value.to_string())),
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
    sender_jid: Jid,
    thread_id: NotificationThreadId,
    archive_stanza_id: StanzaId,
    class: NotificationClass,
    reason: NotificationReason,
    policy_error_count: i64,
}

impl NotificationCandidate {
    pub fn direct_message(
        recipient_bare_jid: BareJid,
        sender_jid: Jid,
        archive_stanza_id: StanzaId,
    ) -> Result<Self, NotificationOutboxError> {
        require_full_sender_jid(&sender_jid)?;
        let expected_by = Jid::from(recipient_bare_jid.clone());
        if archive_stanza_id.by != expected_by {
            return Err(NotificationOutboxError::ArchiveStanzaIdOwnerMismatch {
                expected: expected_by,
                actual: archive_stanza_id.by,
            });
        }
        Ok(Self {
            recipient_bare_jid,
            conversation_jid: sender_jid.to_bare(),
            sender_jid,
            thread_id: NotificationThreadId::root(),
            archive_stanza_id,
            class: NotificationClass::DirectMessage,
            reason: NotificationReason::OfflineDirectMessage,
            policy_error_count: 0,
        })
    }

    pub fn groupchat(
        recipient_bare_jid: BareJid,
        conversation_jid: BareJid,
        sender_jid: Jid,
        thread_id: NotificationThreadId,
        archive_stanza_id: StanzaId,
        class: NotificationClass,
    ) -> Result<Self, NotificationOutboxError> {
        require_full_sender_jid(&sender_jid)?;
        require_sender_matches_conversation(&sender_jid, &conversation_jid)?;
        let expected_by = Jid::from(conversation_jid.clone());
        if archive_stanza_id.by != expected_by {
            return Err(NotificationOutboxError::ArchiveStanzaIdOwnerMismatch {
                expected: expected_by,
                actual: archive_stanza_id.by,
            });
        }
        let reason = match class {
            NotificationClass::PersonalMention => NotificationReason::GroupchatPersonalMention,
            NotificationClass::ChannelMention => NotificationReason::GroupchatChannelMention,
            NotificationClass::ActiveChannelMention => {
                NotificationReason::GroupchatActiveChannelMention
            }
            NotificationClass::NotifyAll => NotificationReason::GroupchatNotifyAll,
            NotificationClass::DirectMessage => {
                return Err(NotificationOutboxError::InvalidClass(
                    class.as_db_value().to_string(),
                ));
            }
        };
        Ok(Self {
            recipient_bare_jid,
            conversation_jid,
            sender_jid,
            thread_id,
            archive_stanza_id,
            class,
            reason,
            policy_error_count: 0,
        })
    }

    pub fn recipient_bare_jid(&self) -> &BareJid {
        &self.recipient_bare_jid
    }

    pub fn conversation_jid(&self) -> &BareJid {
        &self.conversation_jid
    }

    pub fn sender_jid(&self) -> &Jid {
        &self.sender_jid
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
    sender_jid: Jid,
    sender_jids: Vec<Jid>,
    thread_id: NotificationThreadId,
    class: NotificationClass,
    message_count: u32,
    context: Element,
    status: NotificationOutboxStatus,
    attempt_count: i64,
    policy_error_count: i64,
    claim_token: Option<String>,
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

    pub fn sender_jid(&self) -> &Jid {
        &self.sender_jid
    }

    pub fn sender_jids(&self) -> &[Jid] {
        &self.sender_jids
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

    pub fn policy_error_count(&self) -> i64 {
        self.policy_error_count
    }

    pub fn claim_token(&self) -> Option<&str> {
        self.claim_token.as_deref()
    }

    pub fn to_xep0357_pubsub_item_with_count(&self, message_count: u32) -> PubSubItem {
        PubSubItem::new(
            Some(self.job_id.as_str().to_string()),
            Some(build_xep0357_notification_payload(
                message_count,
                &self.context,
            )),
        )
    }

    pub fn to_xep0357_pubsub_item(&self) -> PubSubItem {
        self.to_xep0357_pubsub_item_with_count(self.message_count)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationCandidateInsertOutcome {
    Inserted,
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
    #[error("inbox error: {0}")]
    Inbox(String),
    #[error("blocking storage error: {0}")]
    Blocking(#[from] BlockingStorageError),
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
    #[error("invalid sender JID in notification outbox: {0}")]
    InvalidSenderJid(String),
    #[error("invalid sender JID set in notification outbox: {0}")]
    InvalidSenderJids(String),
    #[error("notification sender JID must include a resource: {0}")]
    SenderJidMissingResource(Jid),
    #[error("notification sender JID set must not be empty")]
    MissingSenderJidSet,
    #[error("notification sender JID {sender} does not match conversation JID {conversation}")]
    SenderConversationMismatch { sender: Jid, conversation: BareJid },
    #[error("notification sender JID set does not include scalar sender JID {0}")]
    SenderJidSetMissingScalar(Jid),
    #[error("invalid archive stanza-id by JID in notification candidate: {0}")]
    InvalidArchiveStanzaIdBy(String),
    #[error("invalid stored notification context XML: {0}")]
    InvalidContextXml(String),
    #[error("archive stanza-id by mismatch: expected {expected}, got {actual}")]
    ArchiveStanzaIdOwnerMismatch { expected: Jid, actual: Jid },
    #[error("message count is out of range: {0}")]
    InvalidMessageCount(i64),
    #[error("notification outbox coalesce contention persisted after retry")]
    OutboxCoalesceContention,
}

fn require_full_sender_jid(sender_jid: &Jid) -> Result<(), NotificationOutboxError> {
    if sender_jid.resource().is_some() {
        Ok(())
    } else {
        Err(NotificationOutboxError::SenderJidMissingResource(
            sender_jid.clone(),
        ))
    }
}

fn require_full_sender_jid_set(sender_jids: &[Jid]) -> Result<(), NotificationOutboxError> {
    if sender_jids.is_empty() {
        return Err(NotificationOutboxError::MissingSenderJidSet);
    }
    sender_jids.iter().try_for_each(require_full_sender_jid)
}

fn require_sender_matches_conversation(
    sender_jid: &Jid,
    conversation_jid: &BareJid,
) -> Result<(), NotificationOutboxError> {
    if sender_jid.to_bare() == *conversation_jid {
        Ok(())
    } else {
        Err(NotificationOutboxError::SenderConversationMismatch {
            sender: sender_jid.clone(),
            conversation: conversation_jid.clone(),
        })
    }
}

fn require_sender_set_matches_conversation(
    sender_jids: &[Jid],
    conversation_jid: &BareJid,
) -> Result<(), NotificationOutboxError> {
    sender_jids.iter().try_for_each(|sender_jid| {
        require_sender_matches_conversation(sender_jid, conversation_jid)
    })
}

fn require_sender_set_contains_scalar(
    sender_jids: &[Jid],
    sender_jid: &Jid,
) -> Result<(), NotificationOutboxError> {
    if sender_jids.iter().any(|candidate| candidate == sender_jid) {
        Ok(())
    } else {
        Err(NotificationOutboxError::SenderJidSetMissingScalar(
            sender_jid.clone(),
        ))
    }
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
                    sender_jid TEXT NOT NULL,
                    thread_id TEXT NOT NULL DEFAULT '',
                    stanza_id_by TEXT NOT NULL,
                    stanza_id TEXT NOT NULL,
                    class TEXT NOT NULL CHECK (class IN ('dm', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
                    reason TEXT NOT NULL CHECK (reason IN ('offline_dm', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all')),
                    created_at_ms {i64_type} NOT NULL,
                    policy_error_count INTEGER NOT NULL DEFAULT 0,
                    next_attempt_at_ms {i64_type},
                    outboxed_at_ms {i64_type},
                    PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
                )
                "#
            ),
            (),
        )
        .await?;
        self.query("SELECT sender_jid FROM notification_candidates LIMIT 0", ())
            .await?;
        self.add_column_if_missing(
            "notification_candidates",
            "policy_error_count INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        let candidate_next_attempt_column = format!("next_attempt_at_ms {i64_type}");
        self.add_column_if_missing("notification_candidates", &candidate_next_attempt_column)
            .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_candidates_recipient_created \
             ON notification_candidates (recipient_bare_jid, created_at_ms)",
            (),
        )
        .await?;
        self.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_notification_candidates_identity \
             ON notification_candidates (recipient_bare_jid, conversation_jid, thread_id, stanza_id, class)",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_candidates_pending_worker \
             ON notification_candidates (created_at_ms, recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class) \
             WHERE outboxed_at_ms IS NULL",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_candidates_outboxed_prune \
             ON notification_candidates (outboxed_at_ms, recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class) \
             WHERE outboxed_at_ms IS NOT NULL",
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
                    sender_jid TEXT NOT NULL,
                    sender_jids TEXT NOT NULL,
                    thread_id TEXT NOT NULL DEFAULT '',
                    class TEXT NOT NULL CHECK (class IN ('dm', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
                    message_count INTEGER NOT NULL,
                    context_xml TEXT NOT NULL,
                    status TEXT NOT NULL CHECK (status IN ('queued', 'in-progress', 'published', 'failed')),
                    attempt_count INTEGER NOT NULL DEFAULT 0,
                    policy_error_count INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    next_attempt_at_ms {i64_type},
                    claimed_at_ms {i64_type},
                    claim_token TEXT,
                    created_at_ms {i64_type} NOT NULL,
                    updated_at_ms {i64_type} NOT NULL,
                    published_at_ms {i64_type}
                )
                "#
            ),
            (),
        )
        .await?;
        self.query(
            "SELECT sender_jid, sender_jids FROM notification_outbox LIMIT 0",
            (),
        )
        .await?;
        self.add_column_if_missing("notification_outbox", "claim_token TEXT")
            .await?;
        self.add_column_if_missing(
            "notification_outbox",
            "policy_error_count INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        self.execute(
            "DROP INDEX IF EXISTS idx_notification_outbox_queued_coalesce",
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
            "CREATE INDEX IF NOT EXISTS idx_notification_outbox_conversation_status \
             ON notification_outbox (recipient_bare_jid, conversation_jid, thread_id, status)",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_outbox_status_next_attempt \
             ON notification_outbox (status, next_attempt_at_ms, created_at_ms)",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_notification_outbox_retention_prune \
             ON notification_outbox (status, updated_at_ms, job_id) \
             WHERE status IN ('published', 'failed')",
            (),
        )
        .await?;
        Ok(())
    }

    async fn add_column_if_missing(
        &self,
        table: &str,
        column_def: &str,
    ) -> Result<(), NotificationOutboxError> {
        let alter_sql = match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => {
                format!("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS {column_def}")
            }
            crate::db::DatabaseDriver::Sqlite => {
                format!("ALTER TABLE {table} ADD COLUMN {column_def}")
            }
        };
        if let Err(error) = self.execute(&alter_sql, ()).await {
            let msg = error.to_string().to_lowercase();
            if msg.contains("duplicate column") || msg.contains("already exists") {
                return Ok(());
            }
            return Err(error);
        }
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

    pub async fn insert_candidate(
        &self,
        candidate: &NotificationCandidate,
    ) -> Result<NotificationCandidateInsertOutcome, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let inserted = self
            .execute(
                r#"
                INSERT INTO notification_candidates (
                    recipient_bare_jid,
                    conversation_jid,
                    sender_jid,
                    thread_id,
                    stanza_id_by,
                    stanza_id,
                    class,
                    reason,
                    created_at_ms,
                    policy_error_count,
                    next_attempt_at_ms,
                    outboxed_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL)
                ON CONFLICT DO NOTHING
                "#,
                crate::db_params![
                    candidate.recipient_bare_jid.to_string(),
                    candidate.conversation_jid.to_string(),
                    candidate.sender_jid.to_string(),
                    candidate.thread_id.as_str(),
                    candidate.archive_stanza_id.by.to_string(),
                    candidate.archive_stanza_id.id.clone(),
                    candidate.class.as_db_value(),
                    candidate.reason.as_db_value(),
                    now_ms,
                    0_i64,
                ],
            )
            .await?;
        if inserted == 0 {
            return Ok(NotificationCandidateInsertOutcome::Duplicate);
        }
        Ok(NotificationCandidateInsertOutcome::Inserted)
    }

    pub async fn drain_pending_candidates_into_outbox(
        &self,
        push_store: &dyn PushSubscriptionStore,
        blocking_storage: &dyn BlockingStorage,
        first_party_service_jid: &BareJid,
        batch_size: usize,
    ) -> Result<usize, NotificationOutboxError> {
        let candidates = self.pending_candidates(batch_size).await?;
        let mut target_cache =
            std::collections::BTreeMap::<BareJid, Vec<NotificationOutboxTarget>>::new();
        let mut processed = 0usize;
        for candidate in candidates {
            match xep0191_blocks_notification_candidate(&candidate, blocking_storage).await {
                Ok(true) => {
                    let now_ms = crate::time::now_ms();
                    let mut tx = self.db.begin().await?;
                    let claimed = mark_candidate_outboxed_tx(&mut tx, &candidate, now_ms).await?;
                    tx.commit().await?;
                    if claimed > 0 {
                        processed += 1;
                    }
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        recipient = %candidate.recipient_bare_jid(),
                        sender = %candidate.sender_jid(),
                        %error,
                        "XEP-0191 blocklist load failed; deferring notification candidate fail-closed"
                    );
                    self.defer_candidate_policy_error(&candidate).await?;
                    continue;
                }
            }
            let recipient_key = candidate.recipient_bare_jid.clone();
            if !target_cache.contains_key(&recipient_key) {
                let resolved = resolve_first_party_targets(
                    push_store,
                    &candidate.recipient_bare_jid,
                    first_party_service_jid,
                )
                .await?;
                target_cache.insert(recipient_key.clone(), resolved);
            }
            let targets = target_cache
                .get(&recipient_key)
                .expect("target cache populated")
                .clone();
            let context = build_waddle_context(&candidate);
            let now_ms = crate::time::now_ms();
            let mut tx = self.db.begin().await?;
            let claimed = mark_candidate_outboxed_tx(&mut tx, &candidate, now_ms).await?;
            if claimed == 0 {
                tx.commit().await?;
                continue;
            }
            for target in &targets {
                enqueue_outbox_job_tx(&mut tx, &candidate, target, &context, now_ms).await?;
            }
            tx.commit().await?;
            processed += 1;
        }
        Ok(processed)
    }

    async fn defer_candidate_policy_error(
        &self,
        candidate: &NotificationCandidate,
    ) -> Result<(), NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let next_policy_error_count = candidate.policy_error_count + 1;
        self.execute(
            r#"
            UPDATE notification_candidates
            SET policy_error_count = ?,
                next_attempt_at_ms = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND sender_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
            "#,
            crate::db_params![
                next_policy_error_count,
                now_ms.saturating_add(policy_retry_delay_ms(next_policy_error_count)),
                candidate.recipient_bare_jid.to_string(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.archive_stanza_id.by.to_string(),
                candidate.archive_stanza_id.id.clone(),
                candidate.class.as_db_value(),
            ],
        )
        .await?;
        Ok(())
    }

    async fn pending_candidates(
        &self,
        batch_size: usize,
    ) -> Result<Vec<NotificationCandidate>, NotificationOutboxError> {
        let batch_size = batch_size.clamp(1, 1_000);
        let mut rows = self
            .query(
                r#"
                SELECT recipient_bare_jid,
                       conversation_jid,
                       sender_jid,
                       thread_id,
                       stanza_id_by,
                       stanza_id,
                       class,
                       reason,
                       policy_error_count
                FROM notification_candidates
                WHERE outboxed_at_ms IS NULL
                  AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)
                ORDER BY created_at_ms ASC,
                         recipient_bare_jid ASC,
                         conversation_jid ASC,
                         sender_jid ASC,
                         thread_id ASC,
                         stanza_id_by ASC,
                         stanza_id ASC,
                         class ASC
                LIMIT ?
                "#,
                crate::db_params![crate::time::now_ms(), batch_size as i64],
            )
            .await?;
        let mut candidates = Vec::new();
        while let Some(row) = rows.next().await? {
            match decode_candidate(&row) {
                Ok(candidate) => candidates.push(candidate),
                Err(error) => {
                    self.mark_malformed_candidate_outboxed(&row, &error).await?;
                }
            }
        }
        Ok(candidates)
    }

    async fn mark_malformed_candidate_outboxed(
        &self,
        row: &Row,
        error: &NotificationOutboxError,
    ) -> Result<(), NotificationOutboxError> {
        let recipient_raw: String = row.get(0)?;
        let conversation_raw: String = row.get(1)?;
        let sender_raw = row
            .get::<Option<String>>(2)?
            .unwrap_or_else(|| "<null>".to_string());
        let thread_id: String = row.get(3)?;
        let stanza_id_by_raw: String = row.get(4)?;
        let stanza_id: String = row.get(5)?;
        let class: String = row.get(6)?;
        tracing::warn!(
            recipient = %recipient_raw,
            conversation = %conversation_raw,
            sender = %sender_raw,
            stanza_id = %stanza_id,
            %error,
            "dropping malformed XEP-0357 notification candidate fail-closed"
        );
        self.execute(
            r#"
            UPDATE notification_candidates
            SET outboxed_at_ms = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
            "#,
            crate::db_params![
                crate::time::now_ms(),
                recipient_raw,
                conversation_raw,
                thread_id,
                stanza_id_by_raw,
                stanza_id,
                class,
            ],
        )
        .await?;
        Ok(())
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
                       sender_jid,
                       sender_jids,
                       thread_id,
                       class,
                       message_count,
                       context_xml,
                       status,
                       attempt_count,
                       policy_error_count,
                       claim_token
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
                       sender_jid,
                       sender_jids,
                       thread_id,
                       class,
                       message_count,
                       context_xml,
                       status,
                       attempt_count,
                       policy_error_count,
                       claim_token
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
            let job_id_raw: String = row.get(0)?;
            match decode_outbox_job(&row) {
                Ok(job) => selected.push(job),
                Err(error) => {
                    tracing::warn!(
                        job_id = %job_id_raw,
                        %error,
                        "failing malformed XEP-0357 notification outbox job fail-closed"
                    );
                    self.mark_malformed_outbox_job_failed(job_id_raw.as_str(), &error.to_string())
                        .await?;
                }
            }
        }

        let mut claimed = Vec::new();
        for job in selected {
            let claim_token = uuid::Uuid::new_v4().to_string();
            let affected = self
                .execute(
                    r#"
                    UPDATE notification_outbox
                    SET status = ?,
                        claimed_at_ms = ?,
                        claim_token = ?,
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
                        claim_token.as_str(),
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
                    claim_token: Some(claim_token),
                    ..job
                });
            }
        }
        Ok(claimed)
    }

    async fn mark_malformed_outbox_job_failed(
        &self,
        job_id: &str,
        error: &str,
    ) -> Result<(), NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        self.execute(
            r#"
            UPDATE notification_outbox
            SET status = ?,
                policy_error_count = 0,
                last_error = ?,
                next_attempt_at_ms = NULL,
                claimed_at_ms = NULL,
                claim_token = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status IN (?, ?)
            "#,
            crate::db_params![
                STATUS_FAILED,
                format!("malformed notification outbox job: {error}"),
                now_ms,
                job_id,
                STATUS_QUEUED,
                STATUS_IN_PROGRESS,
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn drain_due_outbox_jobs(
        &self,
        push_service: &crate::push_service::DatabasePushServiceStore,
        push_store: &dyn PushSubscriptionStore,
        inbox_storage: &dyn InboxStorage,
        blocking_storage: &dyn BlockingStorage,
        first_party_service_jid: &BareJid,
        batch_size: usize,
    ) -> Result<Vec<NotificationOutboxPublishOutcome>, NotificationOutboxError> {
        let jobs = self.claim_due_outbox_jobs(batch_size).await?;
        let mut outcomes = Vec::with_capacity(jobs.len());
        for job in jobs {
            match self
                .publish_claimed_job(
                    &job,
                    push_service,
                    push_store,
                    inbox_storage,
                    blocking_storage,
                    first_party_service_jid,
                )
                .await
            {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) => {
                    outcomes.push(
                        self.retry_or_fail_outcome_for_claimed_job(&job, error.to_string())
                            .await?,
                    );
                }
            }
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
        self.execute(
            r#"
                DELETE FROM notification_candidates
                WHERE (
                    recipient_bare_jid,
                    conversation_jid,
                    sender_jid,
                    thread_id,
                    stanza_id_by,
                    stanza_id,
                    class
                ) IN (
                    SELECT recipient_bare_jid,
                           conversation_jid,
                           sender_jid,
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
                             sender_jid ASC,
                             thread_id ASC,
                             stanza_id_by ASC,
                             stanza_id ASC,
                             class ASC
                    LIMIT ?
                )
                "#,
            crate::db_params![cutoff_ms, batch_size as i64],
        )
        .await
    }

    async fn publish_claimed_job(
        &self,
        job: &NotificationOutboxJob,
        push_service: &crate::push_service::DatabasePushServiceStore,
        push_store: &dyn PushSubscriptionStore,
        inbox_storage: &dyn InboxStorage,
        blocking_storage: &dyn BlockingStorage,
        first_party_service_jid: &BareJid,
    ) -> Result<NotificationOutboxPublishOutcome, NotificationOutboxError> {
        if job.push_service_jid() != first_party_service_jid {
            if self
                .mark_job_failed(
                    job,
                    "notification outbox job targets a non-first-party XEP-0357 Push Service",
                )
                .await?
            {
                return Ok(NotificationOutboxPublishOutcome::Failed {
                    job_id: job.job_id.clone(),
                });
            }
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        }

        match xep0191_blocks_notification_job(job, blocking_storage).await {
            Ok(true) => {
                if self
                    .mark_job_failed(job, "recipient blocked sender before XEP-0357 publish")
                    .await?
                {
                    return Ok(NotificationOutboxPublishOutcome::Failed {
                        job_id: job.job_id.clone(),
                    });
                }
                return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                    job_id: job.job_id.clone(),
                });
            }
            Ok(false) => {}
            Err(error) => {
                return self
                    .defer_claimed_job_without_attempt(
                        job,
                        format!("XEP-0191 blocklist load failed: {error}"),
                    )
                    .await;
            }
        }

        let registrations = push_store
            .get_for_user(&job.recipient_bare_jid.to_string())
            .await
            .map_err(|error| error.to_string());
        let registrations = match registrations {
            Ok(registrations) => registrations,
            Err(error) => {
                return self.retry_or_fail_outcome_for_claimed_job(job, error).await;
            }
        };
        let service = job.push_service_jid.to_string();
        let registration = registrations.into_iter().find(|registration| {
            registration.service_jid == service
                && registration.node.as_deref() == Some(job.node.as_str())
        });
        let Some(registration) = registration else {
            if self
                .mark_job_failed(job, "first-party XEP-0357 registration is no longer active")
                .await?
            {
                return Ok(NotificationOutboxPublishOutcome::Failed {
                    job_id: job.job_id.clone(),
                });
            }
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        };

        if !self.claimed_job_is_current(job).await? {
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        }

        let message_count = current_unread_count_for_job(job, inbox_storage).await?;
        let item = job.to_xep0357_pubsub_item_with_count(message_count);
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
                if !self.mark_job_published(job).await? {
                    return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                        job_id: job.job_id.clone(),
                    });
                }
                Ok(NotificationOutboxPublishOutcome::Published {
                    job_id: job.job_id.clone(),
                    item_id: result.item_id().to_string(),
                })
            }
            Err(error) => {
                self.retry_or_fail_outcome_for_claimed_job(job, error.to_string())
                    .await
            }
        }
    }

    async fn claimed_job_is_current(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<bool, NotificationOutboxError> {
        let mut rows = self
            .query(
                r#"
                SELECT 1
                FROM notification_outbox
                WHERE job_id = ?
                  AND status = ?
                  AND claim_token = ?
                LIMIT 1
                "#,
                crate::db_params![
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                ],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    async fn retry_or_fail_outcome_for_claimed_job(
        &self,
        job: &NotificationOutboxJob,
        error: String,
    ) -> Result<NotificationOutboxPublishOutcome, NotificationOutboxError> {
        let Some(attempts) = self.schedule_retry_or_fail(job, error).await? else {
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        };
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

    async fn defer_claimed_job_without_attempt(
        &self,
        job: &NotificationOutboxJob,
        error: String,
    ) -> Result<NotificationOutboxPublishOutcome, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let next_policy_error_count = job.policy_error_count + 1;
        self.execute(
            r#"
            UPDATE notification_outbox
            SET status = ?,
                policy_error_count = ?,
                last_error = ?,
                next_attempt_at_ms = ?,
                claimed_at_ms = NULL,
                claim_token = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND claim_token = ?
            "#,
            crate::db_params![
                STATUS_QUEUED,
                next_policy_error_count,
                error,
                now_ms.saturating_add(policy_retry_delay_ms(next_policy_error_count)),
                now_ms,
                job.job_id.as_str(),
                STATUS_IN_PROGRESS,
                job.claim_token.as_deref(),
            ],
        )
        .await?;
        Ok(NotificationOutboxPublishOutcome::RetryScheduled {
            job_id: job.job_id.clone(),
        })
    }

    async fn mark_job_published(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<bool, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let affected = self
            .execute(
                r#"
            UPDATE notification_outbox
            SET status = ?,
                policy_error_count = 0,
                last_error = NULL,
                next_attempt_at_ms = NULL,
                claimed_at_ms = NULL,
                claim_token = NULL,
                updated_at_ms = ?,
                published_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND claim_token = ?
            "#,
                crate::db_params![
                    STATUS_PUBLISHED,
                    now_ms,
                    now_ms,
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                ],
            )
            .await?;
        Ok(affected > 0)
    }

    async fn mark_job_failed(
        &self,
        job: &NotificationOutboxJob,
        error: &str,
    ) -> Result<bool, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let affected = self
            .execute(
                r#"
            UPDATE notification_outbox
            SET status = ?,
                policy_error_count = 0,
                last_error = ?,
                next_attempt_at_ms = NULL,
                claimed_at_ms = NULL,
                claim_token = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND claim_token = ?
            "#,
                crate::db_params![
                    STATUS_FAILED,
                    error,
                    now_ms,
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                ],
            )
            .await?;
        Ok(affected > 0)
    }

    async fn schedule_retry_or_fail(
        &self,
        job: &NotificationOutboxJob,
        error: String,
    ) -> Result<Option<i64>, NotificationOutboxError> {
        let next_attempt_count = job.attempt_count + 1;
        let now_ms = crate::time::now_ms();
        let (status, next_attempt_at_ms) = if next_attempt_count >= MAX_OUTBOX_ATTEMPTS {
            (STATUS_FAILED, None)
        } else {
            (
                STATUS_QUEUED,
                Some(now_ms.saturating_add(retry_delay_ms(next_attempt_count))),
            )
        };
        let affected = self
            .execute(
                r#"
            UPDATE notification_outbox
            SET status = ?,
                attempt_count = ?,
                policy_error_count = 0,
                last_error = ?,
                next_attempt_at_ms = ?,
                claimed_at_ms = NULL,
                claim_token = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND claim_token = ?
            "#,
                crate::db_params![
                    status,
                    next_attempt_count,
                    error,
                    next_attempt_at_ms,
                    now_ms,
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                ],
            )
            .await?;
        if affected == 0 {
            return Ok(None);
        }
        Ok(Some(next_attempt_count))
    }
}

async fn xep0191_blocks_notification_job(
    job: &NotificationOutboxJob,
    blocking_storage: &dyn BlockingStorage,
) -> Result<bool, BlockingStorageError> {
    let blocked = blocking_storage
        .list_blocked_jid_entries(job.recipient_bare_jid())
        .await?;
    Ok(blocked
        .into_iter()
        .any(|blocked_jid| xep0191_block_entry_matches_outbox_job(&blocked_jid, job)))
}

async fn xep0191_blocks_notification_candidate(
    candidate: &NotificationCandidate,
    blocking_storage: &dyn BlockingStorage,
) -> Result<bool, BlockingStorageError> {
    let blocked = blocking_storage
        .list_blocked_jid_entries(candidate.recipient_bare_jid())
        .await?;
    Ok(blocked.into_iter().any(|blocked_jid| {
        xep0191_block_entry_matches_sender(&blocked_jid, candidate.sender_jid())
    }))
}

fn xep0191_block_entry_matches_outbox_job(blocked_jid: &Jid, job: &NotificationOutboxJob) -> bool {
    if blocked_jid.resource().is_some() {
        job.sender_jids()
            .iter()
            .any(|sender_jid| blocked_jid == sender_jid)
    } else if blocked_jid.node().is_some() {
        blocked_jid.to_bare() == *job.conversation_jid()
    } else {
        blocked_jid.domain() == job.conversation_jid().domain()
    }
}

fn xep0191_block_entry_matches_sender(blocked_jid: &Jid, sender_jid: &Jid) -> bool {
    if blocked_jid.resource().is_some() {
        blocked_jid == sender_jid
    } else if blocked_jid.node().is_some() {
        blocked_jid.to_bare() == sender_jid.to_bare()
    } else {
        blocked_jid.domain() == sender_jid.domain()
    }
}

fn encode_sender_jids(sender_jids: &[Jid]) -> Result<String, NotificationOutboxError> {
    let values: Vec<String> = sender_jids.iter().map(ToString::to_string).collect();
    serde_json::to_string(&values)
        .map_err(|error| NotificationOutboxError::InvalidSenderJids(error.to_string()))
}

fn decode_sender_jids(raw: &str) -> Result<Vec<Jid>, NotificationOutboxError> {
    let values: Vec<String> = serde_json::from_str(raw)
        .map_err(|error| NotificationOutboxError::InvalidSenderJids(error.to_string()))?;
    values
        .into_iter()
        .map(|value| {
            value
                .parse()
                .map_err(|_| NotificationOutboxError::InvalidSenderJid(value))
        })
        .collect()
}

async fn enqueue_outbox_job_tx(
    tx: &mut crate::db::Transaction<'_>,
    candidate: &NotificationCandidate,
    target: &NotificationOutboxTarget,
    context: &Element,
    now_ms: i64,
) -> Result<(), NotificationOutboxError> {
    // The durable schema stores XML as TEXT; keep protocol context typed until this DB write edge.
    let context_xml = String::from(context);
    for _ in 0..8 {
        let inserted =
            insert_outbox_job_tx(tx, candidate, target, context_xml.as_str(), now_ms).await?;
        if inserted > 0 {
            return Ok(());
        }
        match merge_outbox_job_tx(tx, candidate, target, context_xml.as_str(), now_ms).await? {
            OutboxMergeOutcome::Merged => return Ok(()),
            OutboxMergeOutcome::MalformedExistingJobFailed
            | OutboxMergeOutcome::QueuedJobNotFound
            | OutboxMergeOutcome::QueuedJobChanged => {}
        }
    }
    Err(NotificationOutboxError::OutboxCoalesceContention)
}

async fn insert_outbox_job_tx(
    tx: &mut crate::db::Transaction<'_>,
    candidate: &NotificationCandidate,
    target: &NotificationOutboxTarget,
    context_xml: &str,
    now_ms: i64,
) -> Result<u64, NotificationOutboxError> {
    let job_id = NotificationOutboxJobId::fresh();
    let sender_jids = encode_sender_jids(std::slice::from_ref(&candidate.sender_jid))?;
    Ok(tx
        .execute(
            r#"
            INSERT INTO notification_outbox (
                job_id,
                recipient_bare_jid,
                push_service_jid,
                node,
                conversation_jid,
                sender_jid,
                sender_jids,
                thread_id,
                class,
                message_count,
                context_xml,
                status,
                attempt_count,
                policy_error_count,
                last_error,
                next_attempt_at_ms,
                claimed_at_ms,
                claim_token,
                created_at_ms,
                updated_at_ms,
                published_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, 0, 0, NULL, NULL, NULL, NULL, ?, ?, NULL)
            ON CONFLICT DO NOTHING
            "#,
            crate::db_params![
                job_id.as_str(),
                candidate.recipient_bare_jid.to_string(),
                target.push_service_jid.to_string(),
                target.node.as_str(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                sender_jids,
                candidate.thread_id.as_str(),
                candidate.class.as_db_value(),
                context_xml,
                STATUS_QUEUED,
                now_ms,
                now_ms,
            ],
        )
        .await?)
}

enum OutboxMergeOutcome {
    Merged,
    MalformedExistingJobFailed,
    QueuedJobNotFound,
    QueuedJobChanged,
}

async fn merge_outbox_job_tx(
    tx: &mut crate::db::Transaction<'_>,
    candidate: &NotificationCandidate,
    target: &NotificationOutboxTarget,
    context_xml: &str,
    now_ms: i64,
) -> Result<OutboxMergeOutcome, NotificationOutboxError> {
    let mut rows = tx
        .query(
            r#"
            SELECT job_id, sender_jid, sender_jids
            FROM notification_outbox
            WHERE recipient_bare_jid = ?
              AND push_service_jid = ?
              AND node = ?
              AND conversation_jid = ?
              AND thread_id = ?
              AND class = ?
              AND status = ?
            LIMIT 1
            "#,
            crate::db_params![
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
    let Some(row) = rows.next().await? else {
        return Ok(OutboxMergeOutcome::QueuedJobNotFound);
    };
    let job_id_raw: String = row.get(0)?;
    let sender_raw = row
        .get::<Option<String>>(1)?
        .ok_or_else(|| NotificationOutboxError::InvalidSenderJid("<null>".to_string()));
    let sender_jids_raw = row
        .get::<Option<String>>(2)?
        .ok_or(NotificationOutboxError::MissingSenderJidSet);
    let existing_sender_jid = match sender_raw.and_then(|raw| {
        let sender_jid = raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidSenderJid(raw))?;
        require_full_sender_jid(&sender_jid)?;
        require_sender_matches_conversation(&sender_jid, &candidate.conversation_jid)?;
        Ok(sender_jid)
    }) {
        Ok(sender_jid) => sender_jid,
        Err(error) => {
            mark_malformed_outbox_job_failed_tx(
                tx,
                job_id_raw.as_str(),
                &error.to_string(),
                now_ms,
            )
            .await?;
            return Ok(OutboxMergeOutcome::MalformedExistingJobFailed);
        }
    };
    let mut sender_jids = match sender_jids_raw.and_then(|raw| {
        let sender_jids = decode_sender_jids(&raw)?;
        require_full_sender_jid_set(&sender_jids)?;
        require_sender_set_matches_conversation(&sender_jids, &candidate.conversation_jid)?;
        require_sender_set_contains_scalar(&sender_jids, &existing_sender_jid)?;
        Ok(sender_jids)
    }) {
        Ok(sender_jids) => sender_jids,
        Err(error) => {
            mark_malformed_outbox_job_failed_tx(
                tx,
                job_id_raw.as_str(),
                &error.to_string(),
                now_ms,
            )
            .await?;
            return Ok(OutboxMergeOutcome::MalformedExistingJobFailed);
        }
    };
    if !sender_jids
        .iter()
        .any(|sender_jid| sender_jid == &candidate.sender_jid)
    {
        sender_jids.push(candidate.sender_jid.clone());
    }
    let sender_jids = encode_sender_jids(&sender_jids)?;
    let affected = tx
        .execute(
            r#"
        UPDATE notification_outbox
        SET message_count = message_count + 1,
            context_xml = ?,
            sender_jid = ?,
            sender_jids = ?,
            policy_error_count = 0,
            last_error = NULL,
            next_attempt_at_ms = NULL,
            updated_at_ms = ?
        WHERE job_id = ?
          AND status = ?
        "#,
            crate::db_params![
                context_xml,
                candidate.sender_jid.to_string(),
                sender_jids,
                now_ms,
                job_id_raw,
                STATUS_QUEUED,
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(OutboxMergeOutcome::QueuedJobChanged);
    }
    Ok(OutboxMergeOutcome::Merged)
}

async fn mark_malformed_outbox_job_failed_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
    error: &str,
    now_ms: i64,
) -> Result<(), NotificationOutboxError> {
    tx.execute(
        r#"
        UPDATE notification_outbox
        SET status = ?,
            policy_error_count = 0,
            last_error = ?,
            next_attempt_at_ms = NULL,
            claimed_at_ms = NULL,
            claim_token = NULL,
            updated_at_ms = ?
        WHERE job_id = ?
          AND status = ?
        "#,
        crate::db_params![
            STATUS_FAILED,
            format!("malformed notification outbox job: {error}"),
            now_ms,
            job_id,
            STATUS_QUEUED,
        ],
    )
    .await?;
    Ok(())
}

async fn mark_candidate_outboxed_tx(
    tx: &mut crate::db::Transaction<'_>,
    candidate: &NotificationCandidate,
    now_ms: i64,
) -> Result<u64, NotificationOutboxError> {
    Ok(tx
        .execute(
            r#"
            UPDATE notification_candidates
            SET outboxed_at_ms = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND sender_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
            "#,
            crate::db_params![
                now_ms,
                candidate.recipient_bare_jid.to_string(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.archive_stanza_id.by.to_string(),
                candidate.archive_stanza_id.id.clone(),
                candidate.class.as_db_value(),
            ],
        )
        .await?)
}

async fn resolve_first_party_targets(
    push_store: &dyn PushSubscriptionStore,
    recipient: &BareJid,
    first_party_service_jid: &BareJid,
) -> Result<Vec<NotificationOutboxTarget>, NotificationOutboxError> {
    let registrations = push_store
        .get_for_user(&recipient.to_string())
        .await
        .map_err(|error| NotificationOutboxError::Push(error.to_string()))?;
    let first_party_service = first_party_service_jid.to_string();
    let mut targets = Vec::new();
    for registration in registrations {
        if registration.service_jid != first_party_service {
            continue;
        }
        match target_from_subscription(&registration) {
            Ok(Some(target)) if target.push_service_jid() == first_party_service_jid => {
                targets.push(target);
            }
            Ok(Some(target)) => {
                tracing::warn!(
                    recipient = %recipient,
                    registration_service = %registration.service_jid,
                    target_service = %target.push_service_jid(),
                    "first-party XEP-0357 registration target did not parse back to the configured service"
                );
            }
            Ok(None) => {
                tracing::warn!(
                    recipient = %recipient,
                    service = %registration.service_jid,
                    "first-party XEP-0357 registration missing node; skipping notification outbox target"
                );
            }
            Err(error) => {
                tracing::warn!(
                    recipient = %recipient,
                    error = %error,
                    "first-party XEP-0357 registration could not be converted into a notification outbox target"
                );
            }
        }
    }
    Ok(targets)
}

async fn current_unread_count_for_job(
    job: &NotificationOutboxJob,
    inbox_storage: &dyn InboxStorage,
) -> Result<u32, NotificationOutboxError> {
    let entries = if job.thread_id.as_str().is_empty() {
        inbox_storage
            .list(job.recipient_bare_jid())
            .await
            .map_err(|error| NotificationOutboxError::Inbox(error.to_string()))?
    } else {
        inbox_storage
            .list_threads(job.recipient_bare_jid(), job.conversation_jid())
            .await
            .map_err(|error| NotificationOutboxError::Inbox(error.to_string()))?
    };
    Ok(entries
        .into_iter()
        .find(|entry| {
            entry.partner == *job.conversation_jid()
                && entry.thread_id.as_deref().unwrap_or("") == job.thread_id.as_str()
        })
        .map(|entry| entry.unread)
        .unwrap_or(0))
}

fn decode_candidate(row: &Row) -> Result<NotificationCandidate, NotificationOutboxError> {
    let recipient_raw: String = row.get(0)?;
    let conversation_raw: String = row.get(1)?;
    let sender_raw = row
        .get::<Option<String>>(2)?
        .ok_or_else(|| NotificationOutboxError::InvalidSenderJid("<null>".to_string()))?;
    let sender_jid = sender_raw
        .parse()
        .map_err(|_| NotificationOutboxError::InvalidSenderJid(sender_raw))?;
    require_full_sender_jid(&sender_jid)?;
    let conversation_jid: BareJid = conversation_raw
        .parse()
        .map_err(|_| NotificationOutboxError::InvalidConversationJid(conversation_raw))?;
    require_sender_matches_conversation(&sender_jid, &conversation_jid)?;
    let stanza_id_by_raw: String = row.get(4)?;
    Ok(NotificationCandidate {
        recipient_bare_jid: recipient_raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidRecipientBareJid(recipient_raw))?,
        conversation_jid,
        sender_jid,
        thread_id: NotificationThreadId::new(row.get::<String>(3)?),
        archive_stanza_id: StanzaId::new(
            row.get::<String>(5)?,
            stanza_id_by_raw
                .parse()
                .map_err(|_| NotificationOutboxError::InvalidArchiveStanzaIdBy(stanza_id_by_raw))?,
        ),
        class: NotificationClass::from_db_value(&row.get::<String>(6)?)?,
        reason: NotificationReason::from_db_value(&row.get::<String>(7)?)?,
        policy_error_count: row.get(8)?,
    })
}

fn decode_outbox_job(row: &Row) -> Result<NotificationOutboxJob, NotificationOutboxError> {
    let recipient_raw: String = row.get(1)?;
    let push_service_raw: String = row.get(2)?;
    let conversation_raw: String = row.get(4)?;
    let sender_raw = row
        .get::<Option<String>>(5)?
        .ok_or_else(|| NotificationOutboxError::InvalidSenderJid("<null>".to_string()))?;
    let sender_jids_raw = row
        .get::<Option<String>>(6)?
        .ok_or(NotificationOutboxError::MissingSenderJidSet)?;
    let message_count: i64 = row.get(9)?;
    let context_xml: String = row.get(10)?;
    let sender_jid: Jid = sender_raw
        .parse()
        .map_err(|_| NotificationOutboxError::InvalidSenderJid(sender_raw))?;
    require_full_sender_jid(&sender_jid)?;
    let conversation_jid: BareJid = conversation_raw
        .parse()
        .map_err(|_| NotificationOutboxError::InvalidConversationJid(conversation_raw))?;
    require_sender_matches_conversation(&sender_jid, &conversation_jid)?;
    let sender_jids = decode_sender_jids(&sender_jids_raw)?;
    require_full_sender_jid_set(&sender_jids)?;
    require_sender_set_matches_conversation(&sender_jids, &conversation_jid)?;
    require_sender_set_contains_scalar(&sender_jids, &sender_jid)?;
    Ok(NotificationOutboxJob {
        job_id: NotificationOutboxJobId::from(row.get::<String>(0)?),
        recipient_bare_jid: recipient_raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidRecipientBareJid(recipient_raw))?,
        push_service_jid: push_service_raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidPushServiceBareJid(push_service_raw))?,
        node: PushServiceNodeName::new(row.get::<String>(3)?)?,
        conversation_jid,
        sender_jid,
        sender_jids,
        thread_id: NotificationThreadId::new(row.get::<String>(7)?),
        class: NotificationClass::from_db_value(&row.get::<String>(8)?)?,
        message_count: u32::try_from(message_count)
            .map_err(|_| NotificationOutboxError::InvalidMessageCount(message_count))?,
        context: context_xml
            .parse::<Element>()
            .map_err(|error| NotificationOutboxError::InvalidContextXml(error.to_string()))?,
        status: NotificationOutboxStatus::from_db_value(&row.get::<String>(11)?)?,
        attempt_count: row.get(12)?,
        policy_error_count: row.get(13)?,
        claim_token: row.get(14)?,
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
        .attr("type", "result")
        .append(xdata_hidden_field("FORM_TYPE", XEP0357_SUMMARY_FORM_TYPE))
        .append(xdata_field("message-count", &message_count.to_string()))
        .build()
}

fn xdata_hidden_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr("var", var)
        .attr("type", "hidden")
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(value)
                .build(),
        )
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

fn policy_retry_delay_ms(policy_error_count: i64) -> i64 {
    let exponent = (policy_error_count - 1).clamp(0, 10) as u32;
    BASE_POLICY_RETRY_DELAY_MS
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
        candidate_for(&bare("alice@example.com"), &bare("bob@example.com"), id)
    }

    fn candidate_for(recipient: &BareJid, sender: &BareJid, id: &str) -> NotificationCandidate {
        candidate_for_sender_jid(
            recipient,
            format!("{sender}/test-resource")
                .parse()
                .expect("full sender jid"),
            id,
        )
    }

    fn candidate_for_sender_jid(
        recipient: &BareJid,
        sender_jid: Jid,
        id: &str,
    ) -> NotificationCandidate {
        NotificationCandidate::direct_message(
            recipient.clone(),
            sender_jid,
            StanzaId::new(id, Jid::from(recipient.clone())),
        )
        .expect("candidate")
    }

    fn groupchat_candidate_for(
        recipient: &BareJid,
        room: &BareJid,
        sender_jid: Jid,
        id: &str,
        class: NotificationClass,
    ) -> NotificationCandidate {
        NotificationCandidate::groupchat(
            recipient.clone(),
            room.clone(),
            sender_jid,
            NotificationThreadId::root(),
            StanzaId::new(id, Jid::from(room.clone())),
            class,
        )
        .expect("groupchat candidate")
    }

    async fn failed_outbox_jobs_count(store: &NotificationOutboxStore) -> i64 {
        let mut rows = store
            .query(
                "SELECT COUNT(*) FROM notification_outbox WHERE status = ?",
                crate::db_params![STATUS_FAILED],
            )
            .await
            .expect("failed outbox count query");
        rows.next()
            .await
            .expect("failed outbox count row")
            .expect("failed outbox count")
            .get(0)
            .expect("failed outbox count")
    }

    fn target() -> NotificationOutboxTarget {
        target_named("web-node")
    }

    fn target_named(node: &str) -> NotificationOutboxTarget {
        NotificationOutboxTarget::new(
            bare("push.example.com"),
            PushServiceNodeName::new(node).expect("node"),
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
    async fn store_initialization_rejects_candidate_schema_without_sender_provenance_column() {
        let db = Database::in_memory("notification-outbox-missing-candidate-sender")
            .await
            .unwrap();
        let conn = db.guard().await.expect("db guard");
        conn.execute(
            r#"
            CREATE TABLE notification_candidates (
                recipient_bare_jid TEXT NOT NULL,
                conversation_jid TEXT NOT NULL,
                thread_id TEXT NOT NULL DEFAULT '',
                stanza_id_by TEXT NOT NULL,
                stanza_id TEXT NOT NULL,
                class TEXT NOT NULL,
                reason TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                policy_error_count INTEGER NOT NULL DEFAULT 0,
                next_attempt_at_ms INTEGER,
                outboxed_at_ms INTEGER,
                PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
            )
            "#,
            (),
        )
        .await
        .expect("create incompatible candidate table");
        drop(conn);

        match NotificationOutboxStore::new(db).await {
            Ok(_) => panic!("store must not add missing sender provenance candidate columns"),
            Err(error) => assert!(
                error.to_string().contains("sender_jid"),
                "unexpected schema error: {error}"
            ),
        }
    }

    #[tokio::test]
    async fn store_initialization_rejects_outbox_schema_without_sender_provenance_columns() {
        let db = Database::in_memory("notification-outbox-missing-job-senders")
            .await
            .unwrap();
        let conn = db.guard().await.expect("db guard");
        conn.execute(
            r#"
            CREATE TABLE notification_outbox (
                job_id TEXT PRIMARY KEY,
                recipient_bare_jid TEXT NOT NULL,
                push_service_jid TEXT NOT NULL,
                node TEXT NOT NULL,
                conversation_jid TEXT NOT NULL,
                thread_id TEXT NOT NULL DEFAULT '',
                class TEXT NOT NULL,
                message_count INTEGER NOT NULL,
                context_xml TEXT NOT NULL,
                status TEXT NOT NULL,
                attempt_count INTEGER NOT NULL DEFAULT 0,
                policy_error_count INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                next_attempt_at_ms INTEGER,
                claimed_at_ms INTEGER,
                claim_token TEXT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                published_at_ms INTEGER
            )
            "#,
            (),
        )
        .await
        .expect("create incompatible outbox table");
        drop(conn);

        match NotificationOutboxStore::new(db).await {
            Ok(_) => panic!("store must not add missing sender provenance outbox columns"),
            Err(error) => assert!(
                error.to_string().contains("sender_jid"),
                "unexpected schema error: {error}"
            ),
        }
    }

    async fn enqueue_jobs_for_test(
        store: &NotificationOutboxStore,
        candidate: &NotificationCandidate,
        targets: &[NotificationOutboxTarget],
    ) {
        let _ = store
            .insert_candidate(candidate)
            .await
            .expect("insert candidate");
        let now_ms = crate::time::now_ms();
        let context = build_waddle_context(candidate);
        let mut tx = store.db.begin().await.expect("begin tx");
        mark_candidate_outboxed_tx(&mut tx, candidate, now_ms)
            .await
            .expect("mark candidate outboxed");
        for target in targets {
            enqueue_outbox_job_tx(&mut tx, candidate, target, &context, now_ms)
                .await
                .expect("enqueue outbox job");
        }
        tx.commit().await.expect("commit tx");
    }

    #[test]
    fn direct_message_candidate_requires_full_sender_jid() {
        let recipient = bare("alice@example.com");
        let result = NotificationCandidate::direct_message(
            recipient.clone(),
            Jid::from(bare("bob@example.com")),
            StanzaId::new("archive-bare-sender", Jid::from(recipient)),
        );

        assert!(matches!(
            result,
            Err(NotificationOutboxError::SenderJidMissingResource(_))
        ));
    }

    async fn register_push_target(
        push_store: &waddle_xmpp::push::InMemoryPushStore,
        recipient: &BareJid,
        target: &NotificationOutboxTarget,
    ) {
        push_store
            .register(waddle_xmpp::push::PushSubscription {
                user_jid: recipient.to_string(),
                service_jid: target.push_service_jid().to_string(),
                node: Some(target.node().as_str().to_string()),
                publish_options: None,
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("register push target");
    }

    async fn inbox_with_unread(
        recipient: &BareJid,
        conversation: &BareJid,
        unread: u32,
    ) -> waddle_xmpp::inbox::storage::InMemoryInboxStorage {
        let inbox = waddle_xmpp::inbox::storage::InMemoryInboxStorage::new();
        for n in 0..unread {
            inbox
                .upsert(
                    recipient,
                    waddle_xmpp::inbox::InboxEntry::new(
                        conversation.clone(),
                        waddle_xmpp::inbox::ConversationKind::Direct,
                        format!("archive-{n}"),
                        i64::from(n),
                    ),
                    true,
                )
                .await
                .expect("upsert inbox entry");
        }
        inbox
    }

    async fn drain_dm_outbox_with_blocking(
        archive_id: &str,
        blocking: &dyn BlockingStorage,
    ) -> (
        Vec<NotificationOutboxPublishOutcome>,
        usize,
        Vec<NotificationOutboxJob>,
    ) {
        drain_dm_outbox_with_sender_jid(
            archive_id,
            "bob@example.com/phone".parse().expect("sender JID"),
            blocking,
        )
        .await
    }

    async fn drain_dm_outbox_with_sender_jid(
        archive_id: &str,
        sender_jid: Jid,
        blocking: &dyn BlockingStorage,
    ) -> (
        Vec<NotificationOutboxPublishOutcome>,
        usize,
        Vec<NotificationOutboxJob>,
    ) {
        let store = store().await;
        let recipient = bare("alice@example.com");
        let sender = sender_jid.to_bare();
        let push_db_name = format!("push-service-{archive_id}");
        let push_service = crate::push_service::DatabasePushServiceStore::new(
            Database::in_memory(&push_db_name).await.unwrap(),
        )
        .await
        .expect("push service");
        crate::push_registrations::DatabasePushRegistrationStore::new(push_service.database())
            .await
            .expect("push registration schema");
        let push_node = push_service
            .ensure_node(&recipient, "web")
            .await
            .expect("push node");
        push_service
            .upsert_device(
                &recipient,
                crate::push_service::PushDeviceRegistration::new(
                    "web-1",
                    push_node.node(),
                    crate::push_service::PushDevicePlatform::Web,
                    "test",
                ),
            )
            .await
            .expect("push device");
        push_service
            .register_first_party_node_for_owner(
                &recipient,
                "push.example.com",
                push_node.node(),
                None,
            )
            .await
            .expect("first-party registration");
        let target = NotificationOutboxTarget::new(
            bare("push.example.com"),
            PushServiceNodeName::new(push_node.node()).expect("push node target"),
        );
        enqueue_jobs_for_test(
            &store,
            &candidate_for_sender_jid(&recipient, sender_jid, archive_id),
            &[target],
        )
        .await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        push_store
            .register(waddle_xmpp::push::PushSubscription {
                user_jid: recipient.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(push_node.node().to_string()),
                publish_options: None,
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("xep0357 registration");
        let inbox = inbox_with_unread(&recipient, &sender, 1).await;

        let outcomes = store
            .drain_due_outbox_jobs(
                &push_service,
                &push_store,
                &inbox,
                blocking,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain outbox");
        let queued_push_job_count = push_service
            .queued_publish_jobs()
            .await
            .expect("queued push jobs")
            .len();
        let pending = store.pending_outbox_jobs().await.expect("pending jobs");
        (outcomes, queued_push_job_count, pending)
    }

    async fn reclaim_stale_job(
        store: &NotificationOutboxStore,
    ) -> (NotificationOutboxJob, NotificationOutboxJob) {
        let stale_claim = store
            .claim_due_outbox_jobs(16)
            .await
            .expect("claim")
            .into_iter()
            .next()
            .expect("claimed job");
        let stale_claimed_at_ms = crate::time::now_ms()
            .saturating_sub(OUTBOX_CLAIM_TIMEOUT_MS)
            .saturating_sub(1);
        store
            .execute(
                "UPDATE notification_outbox SET claimed_at_ms = ? WHERE job_id = ?",
                crate::db_params![stale_claimed_at_ms, stale_claim.job_id().as_str()],
            )
            .await
            .expect("make claim stale");
        let fresh_claim = store
            .claim_due_outbox_jobs(16)
            .await
            .expect("reclaim")
            .into_iter()
            .next()
            .expect("reclaimed job");
        (stale_claim, fresh_claim)
    }

    #[tokio::test]
    async fn candidate_insert_is_idempotent_and_worker_coalesces_distinct_messages() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let first = candidate("archive-1");
        let duplicate = candidate("archive-1");
        let second = candidate("archive-2");
        register_push_target(&push_store, first.recipient_bare_jid(), &target).await;

        assert_eq!(
            store.insert_candidate(&first).await.expect("first insert"),
            NotificationCandidateInsertOutcome::Inserted
        );
        assert_eq!(
            store
                .insert_candidate(&duplicate)
                .await
                .expect("duplicate insert"),
            NotificationCandidateInsertOutcome::Duplicate
        );
        assert_eq!(
            store
                .insert_candidate(&second)
                .await
                .expect("second insert"),
            NotificationCandidateInsertOutcome::Inserted
        );

        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pre-worker jobs")
            .is_empty());
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            2
        );
        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].message_count(), 2);
        assert_eq!(jobs[0].conversation_jid(), &bare("bob@example.com"));
        assert_eq!(jobs[0].class(), NotificationClass::DirectMessage);
    }

    #[tokio::test]
    async fn candidate_worker_marks_malformed_bare_sender_candidate_terminal() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        register_push_target(&push_store, &recipient, &target).await;
        let candidate = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-malformed-candidate-bare-sender",
        );
        store
            .insert_candidate(&candidate)
            .await
            .expect("candidate insert");
        store
            .execute(
                "UPDATE notification_candidates SET sender_jid = ? WHERE stanza_id = ?",
                crate::db_params!["bob@example.com", "archive-malformed-candidate-bare-sender"],
            )
            .await
            .expect("make candidate sender malformed");

        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            0
        );

        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
        assert!(store
            .pending_candidates(16)
            .await
            .expect("pending candidates")
            .is_empty());
        let mut rows = store
            .query(
                "SELECT outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["archive-malformed-candidate-bare-sender"],
            )
            .await
            .expect("candidate marker query");
        let row = rows
            .next()
            .await
            .expect("candidate marker row")
            .expect("candidate marker row");
        assert!(row
            .get::<Option<i64>>(0)
            .expect("outboxed marker")
            .is_some());
    }

    #[tokio::test]
    async fn candidate_worker_marks_empty_sender_candidate_terminal_without_conversation_fallback()
    {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        register_push_target(&push_store, &recipient, &target).await;
        let candidate = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-malformed-candidate-empty-sender",
        );
        store
            .insert_candidate(&candidate)
            .await
            .expect("candidate insert");
        store
            .execute(
                "UPDATE notification_candidates SET sender_jid = ? WHERE stanza_id = ?",
                crate::db_params!["", "archive-malformed-candidate-empty-sender"],
            )
            .await
            .expect("make candidate sender empty");

        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            0
        );

        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
        assert!(store
            .pending_candidates(16)
            .await
            .expect("pending candidates")
            .is_empty());
        let mut rows = store
            .query(
                "SELECT outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["archive-malformed-candidate-empty-sender"],
            )
            .await
            .expect("candidate marker query");
        let row = rows
            .next()
            .await
            .expect("candidate marker row")
            .expect("candidate marker row");
        assert!(row
            .get::<Option<i64>>(0)
            .expect("outboxed marker")
            .is_some());
    }

    #[tokio::test]
    async fn candidate_worker_marks_mismatched_sender_candidate_terminal() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        register_push_target(&push_store, &recipient, &target).await;
        let candidate = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-malformed-candidate-mismatch",
        );
        store
            .insert_candidate(&candidate)
            .await
            .expect("candidate insert");
        store
            .execute(
                "UPDATE notification_candidates SET conversation_jid = ? WHERE stanza_id = ?",
                crate::db_params!["carol@example.com", "archive-malformed-candidate-mismatch"],
            )
            .await
            .expect("make candidate sender mismatch conversation");

        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            0
        );

        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
        assert!(store
            .pending_candidates(16)
            .await
            .expect("pending candidates")
            .is_empty());
    }

    #[tokio::test]
    async fn candidate_worker_marks_malformed_conversation_candidate_terminal_and_continues() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let malformed_recipient = bare("alice@example.com");
        let valid_recipient = bare("carol@example.com");
        register_push_target(&push_store, &malformed_recipient, &target).await;
        register_push_target(&push_store, &valid_recipient, &target).await;
        let malformed = candidate_for_sender_jid(
            &malformed_recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-malformed-candidate-conversation",
        );
        let valid = candidate_for_sender_jid(
            &valid_recipient,
            "dave@example.com/phone".parse().expect("valid sender"),
            "archive-valid-after-malformed-candidate",
        );
        store
            .insert_candidate(&malformed)
            .await
            .expect("malformed candidate insert");
        store
            .insert_candidate(&valid)
            .await
            .expect("valid candidate insert");
        store
            .execute(
                "UPDATE notification_candidates SET conversation_jid = ? WHERE stanza_id = ?",
                crate::db_params!["not a jid", "archive-malformed-candidate-conversation"],
            )
            .await
            .expect("make candidate conversation malformed");

        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            1
        );

        assert!(store
            .pending_candidates(16)
            .await
            .expect("pending candidates")
            .is_empty());
        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].recipient_bare_jid(), &valid_recipient);
    }

    #[tokio::test]
    async fn candidate_worker_coalesces_distinct_sender_resources_into_one_bare_conversation_job() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        register_push_target(&push_store, &recipient, &target).await;

        let phone = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-bob-phone",
        );
        let laptop = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/laptop".parse().expect("laptop sender"),
            "archive-bob-laptop",
        );
        assert_eq!(
            store.insert_candidate(&phone).await.expect("phone insert"),
            NotificationCandidateInsertOutcome::Inserted
        );
        assert_eq!(
            store
                .insert_candidate(&laptop)
                .await
                .expect("laptop insert"),
            NotificationCandidateInsertOutcome::Inserted
        );

        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            2
        );

        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].conversation_jid(), &bare("bob@example.com"));
        assert_eq!(jobs[0].message_count(), 2);
        let mut sender_jids = jobs[0]
            .sender_jids()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        sender_jids.sort();
        assert_eq!(
            sender_jids,
            vec![
                "bob@example.com/laptop".to_string(),
                "bob@example.com/phone".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn candidate_worker_fails_malformed_coalesced_job_before_requeueing_exact_sender() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        register_push_target(&push_store, &recipient, &target).await;

        let phone = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-malformed-existing-phone",
        );
        store.insert_candidate(&phone).await.expect("phone insert");
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain first candidate"),
            1
        );
        store
            .execute(
                "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
                crate::db_params!["bob@example.com", "[]"],
            )
            .await
            .expect("make queued job malformed");

        let laptop_sender = "bob@example.com/laptop".parse().expect("laptop sender");
        let laptop = candidate_for_sender_jid(
            &recipient,
            laptop_sender,
            "archive-malformed-existing-laptop",
        );
        store
            .insert_candidate(&laptop)
            .await
            .expect("laptop insert");
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain second candidate"),
            1
        );

        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].message_count(), 1);
        assert_eq!(
            jobs[0].sender_jids(),
            &["bob@example.com/laptop"
                .parse::<Jid>()
                .expect("laptop sender")]
        );
    }

    #[tokio::test]
    async fn candidate_worker_fails_malformed_sender_jids_json_before_requeueing_exact_sender() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        register_push_target(&push_store, &recipient, &target).await;

        let phone = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-malformed-json-phone",
        );
        store.insert_candidate(&phone).await.expect("phone insert");
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain first candidate"),
            1
        );
        store
            .execute(
                "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
                crate::db_params!["bob@example.com/phone", "not-json"],
            )
            .await
            .expect("make queued job sender_jids malformed");

        let laptop = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/laptop".parse().expect("laptop sender"),
            "archive-malformed-json-laptop",
        );
        store
            .insert_candidate(&laptop)
            .await
            .expect("laptop insert");
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain second candidate"),
            1
        );

        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].message_count(), 1);
        assert_eq!(
            jobs[0].sender_jids(),
            &["bob@example.com/laptop"
                .parse::<Jid>()
                .expect("laptop sender")]
        );
    }

    #[tokio::test]
    async fn candidate_worker_fails_sender_set_missing_scalar_before_requeueing_exact_sender() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        register_push_target(&push_store, &recipient, &target).await;

        let phone = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-missing-scalar-phone",
        );
        store.insert_candidate(&phone).await.expect("phone insert");
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain first candidate"),
            1
        );
        store
            .execute(
                "UPDATE notification_outbox SET sender_jids = ?",
                crate::db_params!["[\"bob@example.com/laptop\"]"],
            )
            .await
            .expect("make queued job sender_jids omit scalar sender");

        let laptop = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/laptop".parse().expect("laptop sender"),
            "archive-missing-scalar-laptop",
        );
        store
            .insert_candidate(&laptop)
            .await
            .expect("laptop insert");
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain second candidate"),
            1
        );

        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].message_count(), 1);
        assert_eq!(
            jobs[0].sender_jids(),
            &["bob@example.com/laptop"
                .parse::<Jid>()
                .expect("laptop sender")]
        );
    }

    #[tokio::test]
    async fn candidate_worker_fails_semantically_invalid_sender_jids_before_requeueing_exact_sender(
    ) {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        register_push_target(&push_store, &recipient, &target).await;

        let phone = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-invalid-sender-jids-phone",
        );
        store.insert_candidate(&phone).await.expect("phone insert");
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain first candidate"),
            1
        );
        store
            .execute(
                "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
                crate::db_params!["bob@example.com/phone", "[\"carol@example.com/phone\"]"],
            )
            .await
            .expect("make queued job sender_jids semantically invalid");

        let laptop = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/laptop".parse().expect("laptop sender"),
            "archive-invalid-sender-jids-laptop",
        );
        store
            .insert_candidate(&laptop)
            .await
            .expect("laptop insert");
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain second candidate"),
            1
        );

        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].message_count(), 1);
        assert_eq!(
            jobs[0].sender_jids(),
            &["bob@example.com/laptop"
                .parse::<Jid>()
                .expect("laptop sender")]
        );
    }

    #[tokio::test]
    async fn candidate_worker_fails_mismatched_scalar_sender_before_requeueing_exact_sender() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        register_push_target(&push_store, &recipient, &target).await;

        let phone = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-invalid-scalar-phone",
        );
        store.insert_candidate(&phone).await.expect("phone insert");
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain first candidate"),
            1
        );
        store
            .execute(
                "UPDATE notification_outbox SET sender_jid = ?",
                crate::db_params!["carol@example.com/phone"],
            )
            .await
            .expect("make queued job scalar sender invalid");

        let laptop = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/laptop".parse().expect("laptop sender"),
            "archive-invalid-scalar-laptop",
        );
        store
            .insert_candidate(&laptop)
            .await
            .expect("laptop insert");
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain second candidate"),
            1
        );

        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].message_count(), 1);
        assert_eq!(
            jobs[0].sender_jids(),
            &["bob@example.com/laptop"
                .parse::<Jid>()
                .expect("laptop sender")]
        );
    }

    #[tokio::test]
    async fn candidate_worker_filters_full_jid_block_before_bare_conversation_coalescing() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        register_push_target(&push_store, &recipient, &target).await;
        blocking.set_blocklist_jids(
            recipient.clone(),
            vec!["bob@example.com/phone".parse().expect("blocked sender")],
        );

        let blocked_phone = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-blocked-phone",
        );
        let allowed_laptop = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/laptop".parse().expect("laptop sender"),
            "archive-allowed-laptop",
        );
        store
            .insert_candidate(&blocked_phone)
            .await
            .expect("blocked insert");
        store
            .insert_candidate(&allowed_laptop)
            .await
            .expect("allowed insert");

        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            2
        );

        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].conversation_jid(), &bare("bob@example.com"));
        assert_eq!(jobs[0].sender_jid().to_string(), "bob@example.com/laptop");
        assert_eq!(jobs[0].message_count(), 1);
    }

    #[tokio::test]
    async fn candidate_worker_applies_xep0191_to_groupchat_notifications() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        let room = bare("team@muc.example.com");
        register_push_target(&push_store, &recipient, &target).await;
        blocking.set_blocklist_jids(recipient.clone(), vec![Jid::from(room.clone())]);

        let candidate = groupchat_candidate_for(
            &recipient,
            &room,
            "team@muc.example.com/bob".parse().expect("room occupant"),
            "archive-blocked-groupchat",
            NotificationClass::ChannelMention,
        );
        store
            .insert_candidate(&candidate)
            .await
            .expect("groupchat insert");

        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            1
        );

        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "XEP-0191-blocked groupchat notifications must not enqueue outbox jobs"
        );
    }

    #[tokio::test]
    async fn publish_worker_applies_xep0191_to_groupchat_notifications() {
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let recipient = bare("alice@example.com");
        let room = bare("team@muc.example.com");
        let sender: Jid = "team@muc.example.com/bob"
            .parse()
            .expect("room occupant sender");
        blocking.set_blocklist_jids(recipient.clone(), vec![Jid::from(room.clone())]);
        let job = NotificationOutboxJob {
            job_id: NotificationOutboxJobId::from("groupchat-blocked-job".to_string()),
            recipient_bare_jid: recipient,
            push_service_jid: bare("push.example.com"),
            node: PushServiceNodeName::new("web-node").expect("node"),
            conversation_jid: room,
            sender_jid: sender.clone(),
            sender_jids: vec![sender],
            thread_id: NotificationThreadId::root(),
            class: NotificationClass::ChannelMention,
            message_count: 1,
            context: Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build(),
            status: NotificationOutboxStatus::Queued,
            attempt_count: 0,
            policy_error_count: 0,
            claim_token: None,
        };

        assert!(
            xep0191_blocks_notification_job(&job, &blocking)
                .await
                .expect("block check"),
            "publish-time XEP-0191 checks must apply to groupchat notification classes"
        );
    }

    #[tokio::test]
    async fn xep0191_full_jid_block_added_after_coalescing_suppresses_dm_push_job() {
        let store = store().await;
        let recipient = bare("alice@example.com");
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let push_service = crate::push_service::DatabasePushServiceStore::new(
            Database::in_memory("push-service-coalesced-full-jid-block")
                .await
                .unwrap(),
        )
        .await
        .expect("push service");
        crate::push_registrations::DatabasePushRegistrationStore::new(push_service.database())
            .await
            .expect("push registration schema");
        let push_node = push_service
            .ensure_node(&recipient, "web")
            .await
            .expect("push node");
        push_service
            .upsert_device(
                &recipient,
                crate::push_service::PushDeviceRegistration::new(
                    "web-1",
                    push_node.node(),
                    crate::push_service::PushDevicePlatform::Web,
                    "test",
                ),
            )
            .await
            .expect("push device");
        push_service
            .register_first_party_node_for_owner(
                &recipient,
                "push.example.com",
                push_node.node(),
                None,
            )
            .await
            .expect("first-party registration");
        let target = NotificationOutboxTarget::new(
            bare("push.example.com"),
            PushServiceNodeName::new(push_node.node()).expect("push node target"),
        );
        register_push_target(&push_store, &recipient, &target).await;

        let phone = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/phone".parse().expect("phone sender"),
            "archive-coalesced-phone",
        );
        let laptop = candidate_for_sender_jid(
            &recipient,
            "bob@example.com/laptop".parse().expect("laptop sender"),
            "archive-coalesced-laptop",
        );
        store.insert_candidate(&phone).await.expect("phone insert");
        store
            .insert_candidate(&laptop)
            .await
            .expect("laptop insert");

        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            2
        );
        let pending = store.pending_outbox_jobs().await.expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].message_count(), 2);
        assert!(pending[0]
            .sender_jids()
            .contains(&"bob@example.com/phone".parse().expect("phone sender")));

        blocking.set_blocklist_jids(
            recipient.clone(),
            vec!["bob@example.com/phone".parse().expect("blocked sender")],
        );

        let publish_push_store = waddle_xmpp::push::InMemoryPushStore::new();
        register_push_target(&publish_push_store, &recipient, &target).await;
        let inbox = inbox_with_unread(&recipient, &bare("bob@example.com"), 2).await;

        let outcomes = store
            .drain_due_outbox_jobs(
                &push_service,
                &publish_push_store,
                &inbox,
                &blocking,
                &bare("push.example.com"),
                16,
            )
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
            "a coalesced job that includes a blocked full sender JID must not publish"
        );
        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
    }

    #[tokio::test]
    async fn candidate_worker_skips_malformed_registration_and_continues_batch() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let alice = bare("alice@example.com");
        let carol = bare("carol@example.com");
        let bob = bare("bob@example.com");
        let bad_candidate = candidate_for(&alice, &bob, "archive-bad-target");
        let good_candidate = candidate_for(&carol, &bob, "archive-good-target");

        push_store
            .register(waddle_xmpp::push::PushSubscription {
                user_jid: alice.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(String::new()),
                publish_options: None,
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("register malformed push target");
        register_push_target(&push_store, &carol, &target).await;

        assert_eq!(
            store
                .insert_candidate(&bad_candidate)
                .await
                .expect("bad candidate insert"),
            NotificationCandidateInsertOutcome::Inserted
        );
        assert_eq!(
            store
                .insert_candidate(&good_candidate)
                .await
                .expect("good candidate insert"),
            NotificationCandidateInsertOutcome::Inserted
        );

        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            2
        );

        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].recipient_bare_jid(), &carol);
        assert_eq!(jobs[0].message_count(), 1);
    }

    #[tokio::test]
    async fn candidate_worker_defers_candidates_fail_closed_when_blocklist_load_fails() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let recipient = bare("alice@example.com");
        register_push_target(&push_store, &recipient, &target).await;
        store
            .insert_candidate(&candidate_for(
                &recipient,
                &bare("bob@example.com"),
                "archive-policy-error-1",
            ))
            .await
            .expect("first insert");
        store
            .insert_candidate(&candidate_for(
                &recipient,
                &bare("carol@example.com"),
                "archive-policy-error-2",
            ))
            .await
            .expect("second insert");

        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &FailingBlockingStorage,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            0
        );

        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
        assert!(store
            .pending_candidates(16)
            .await
            .expect("backed-off pending candidates")
            .is_empty());

        let mut rows = store
            .query(
                "SELECT policy_error_count FROM notification_candidates ORDER BY stanza_id",
                (),
            )
            .await
            .expect("policy count query");
        let mut policy_error_counts = Vec::new();
        while let Some(row) = rows.next().await.expect("policy count row") {
            policy_error_counts.push(row.get::<i64>(0).expect("policy count"));
        }
        assert_eq!(policy_error_counts, vec![1, 1]);

        store
            .execute(
                "UPDATE notification_candidates SET next_attempt_at_ms = NULL",
                (),
            )
            .await
            .expect("release backoff");
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("retry drain candidates"),
            2
        );
        assert_eq!(
            store
                .pending_outbox_jobs()
                .await
                .expect("retried pending jobs")
                .len(),
            2
        );
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
        assert_eq!(summary.attr("type"), Some("result"));
        assert!(summary.children().any(|field| {
            field.is("field", NS_DATA_FORMS)
                && field.attr("var") == Some("FORM_TYPE")
                && field.attr("type") == Some("hidden")
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
        enqueue_jobs_for_test(&store, &candidate, &[target]).await;

        let jobs = store.claim_due_outbox_jobs(16).await.expect("claim");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status(), NotificationOutboxStatus::InProgress);
        let item = jobs[0].to_xep0357_pubsub_item();
        assert_eq!(item.id.as_deref(), Some(jobs[0].job_id().as_str()));
        let payload = item.payload.expect("payload");
        assert!(payload.is("notification", waddle_xmpp::xep::xep0357::NS_PUSH));
    }

    #[tokio::test]
    async fn claim_due_outbox_jobs_fails_malformed_sender_set_before_publish() {
        let store = store().await;
        enqueue_jobs_for_test(&store, &candidate("archive-malformed-claim"), &[target()]).await;
        store
            .execute(
                "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
                crate::db_params!["bob@example.com", "[]"],
            )
            .await
            .expect("make queued job malformed");

        let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

        assert!(claimed.is_empty());
        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
    }

    #[tokio::test]
    async fn claim_due_outbox_jobs_fails_empty_sender_without_conversation_fallback() {
        let store = store().await;
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-empty-sender-claim"),
            &[target()],
        )
        .await;
        store
            .execute(
                "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
                crate::db_params!["", "[]"],
            )
            .await
            .expect("make queued job sender provenance empty");

        let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

        assert!(claimed.is_empty());
        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
    }

    #[tokio::test]
    async fn claim_due_outbox_jobs_fails_malformed_sender_jids_json_before_publish() {
        let store = store().await;
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-malformed-json-claim"),
            &[target()],
        )
        .await;
        store
            .execute(
                "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
                crate::db_params!["bob@example.com/test-resource", "not-json"],
            )
            .await
            .expect("make queued job sender_jids malformed");

        let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

        assert!(claimed.is_empty());
        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
    }

    #[tokio::test]
    async fn claim_due_outbox_jobs_fails_sender_set_missing_scalar_before_publish() {
        let store = store().await;
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-missing-scalar-claim"),
            &[target()],
        )
        .await;
        store
            .execute(
                "UPDATE notification_outbox SET sender_jids = ?",
                crate::db_params!["[\"bob@example.com/laptop\"]"],
            )
            .await
            .expect("make queued job sender_jids omit scalar sender");

        let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

        assert!(claimed.is_empty());
        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
    }

    #[tokio::test]
    async fn claim_due_outbox_jobs_fails_semantically_invalid_sender_jids_before_publish() {
        let store = store().await;
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-invalid-sender-jids-claim"),
            &[target()],
        )
        .await;
        store
            .execute(
                "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
                crate::db_params![
                    "bob@example.com/test-resource",
                    "[\"carol@example.com/phone\"]"
                ],
            )
            .await
            .expect("make queued job sender_jids semantically invalid");

        let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

        assert!(claimed.is_empty());
        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
    }

    #[tokio::test]
    async fn claim_due_outbox_jobs_fails_mismatched_scalar_sender_before_publish() {
        let store = store().await;
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-invalid-scalar-sender-claim"),
            &[target()],
        )
        .await;
        store
            .execute(
                "UPDATE notification_outbox SET sender_jid = ?",
                crate::db_params!["carol@example.com/phone"],
            )
            .await
            .expect("make queued job scalar sender invalid");

        let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

        assert!(claimed.is_empty());
        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
    }

    #[tokio::test]
    async fn claim_due_outbox_jobs_fails_malformed_context_before_publish() {
        let store = store().await;
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-malformed-context-claim"),
            &[target()],
        )
        .await;
        store
            .execute(
                "UPDATE notification_outbox SET context_xml = ?",
                crate::db_params!["<context"],
            )
            .await
            .expect("make queued job context malformed");

        let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

        assert!(claimed.is_empty());
        assert_eq!(failed_outbox_jobs_count(&store).await, 1);
        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
    }

    #[tokio::test]
    async fn stale_in_progress_outbox_job_is_claimable_again() {
        let store = store().await;
        let target = target();
        let candidate = candidate("archive-1");
        enqueue_jobs_for_test(&store, &candidate, &[target]).await;

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
        assert_ne!(reclaimed[0].claim_token(), first_claim[0].claim_token());
    }

    #[tokio::test]
    async fn stale_claim_cannot_mark_reclaimed_outbox_job_published() {
        let store = store().await;
        let target = target();
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-1"),
            std::slice::from_ref(&target),
        )
        .await;
        let (stale_claim, fresh_claim) = reclaim_stale_job(&store).await;

        assert!(
            !store
                .mark_job_published(&stale_claim)
                .await
                .expect("stale publish mark should not fail"),
            "stale worker must not complete a job after another worker reclaimed it"
        );
        let pending = store.pending_outbox_jobs().await.expect("pending jobs");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status(), NotificationOutboxStatus::InProgress);
        assert_eq!(pending[0].claim_token(), fresh_claim.claim_token());

        assert!(store
            .mark_job_published(&fresh_claim)
            .await
            .expect("fresh publish mark should succeed"));
        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending after fresh mark")
            .is_empty());
    }

    #[tokio::test]
    async fn stale_claim_does_not_enqueue_push_service_publish_job() {
        let store = store().await;
        let recipient = bare("alice@example.com");
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let push_service = crate::push_service::DatabasePushServiceStore::new(
            Database::in_memory("push-service").await.unwrap(),
        )
        .await
        .expect("push service");
        crate::push_registrations::DatabasePushRegistrationStore::new(push_service.database())
            .await
            .expect("push registration schema");
        let push_node = push_service
            .ensure_node(&recipient, "web")
            .await
            .expect("push node");
        push_service
            .upsert_device(
                &recipient,
                crate::push_service::PushDeviceRegistration::new(
                    "web-1",
                    push_node.node(),
                    crate::push_service::PushDevicePlatform::Web,
                    "test",
                ),
            )
            .await
            .expect("push device");
        push_service
            .register_first_party_node_for_owner(
                &recipient,
                "push.example.com",
                push_node.node(),
                None,
            )
            .await
            .expect("first-party registration");
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        push_store
            .register(waddle_xmpp::push::PushSubscription {
                user_jid: recipient.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(push_node.node().to_string()),
                publish_options: None,
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("xep0357 registration");
        let target = NotificationOutboxTarget::new(
            bare("push.example.com"),
            PushServiceNodeName::new(push_node.node()).expect("push node target"),
        );
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-1"),
            std::slice::from_ref(&target),
        )
        .await;
        let (stale_claim, fresh_claim) = reclaim_stale_job(&store).await;
        let inbox = inbox_with_unread(&recipient, &bare("bob@example.com"), 1).await;

        let outcome = store
            .publish_claimed_job(
                &stale_claim,
                &push_service,
                &push_store,
                &inbox,
                &blocking,
                &bare("push.example.com"),
            )
            .await
            .expect("stale publish");

        assert!(matches!(
            outcome,
            NotificationOutboxPublishOutcome::RetryScheduled { .. }
        ));
        assert!(
            push_service
                .queued_publish_jobs()
                .await
                .expect("queued push jobs")
                .is_empty(),
            "stale claims must not enqueue durable Push Service publish jobs"
        );
        let pending = store.pending_outbox_jobs().await.expect("pending jobs");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].claim_token(), fresh_claim.claim_token());
    }

    #[tokio::test]
    async fn new_candidate_after_claim_creates_fresh_queued_job() {
        let store = store().await;
        let target = target();
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-1"),
            std::slice::from_ref(&target),
        )
        .await;

        let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].message_count(), 1);
        enqueue_jobs_for_test(&store, &candidate("archive-2"), &[target]).await;

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
    async fn coalesce_retry_creates_fresh_job_when_queued_job_is_claimed_after_select() {
        let store = store().await;
        let target = target();
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-race-1"),
            std::slice::from_ref(&target),
        )
        .await;
        store
            .db
            .guard()
            .await
            .expect("db guard")
            .execute(
                r#"
                CREATE TRIGGER simulate_notification_outbox_claim_race
                BEFORE UPDATE OF message_count ON notification_outbox
                WHEN OLD.status = 'queued'
                BEGIN
                    UPDATE notification_outbox
                    SET status = 'in-progress',
                        claimed_at_ms = OLD.updated_at_ms + 1,
                        claim_token = 'race-claim',
                        updated_at_ms = OLD.updated_at_ms + 1
                    WHERE job_id = OLD.job_id;
                    SELECT RAISE(IGNORE);
                END;
                "#,
                (),
            )
            .await
            .expect("install coalesce race trigger");

        enqueue_jobs_for_test(&store, &candidate("archive-race-2"), &[target]).await;

        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 2);
        assert!(jobs
            .iter()
            .any(|job| job.status() == NotificationOutboxStatus::InProgress
                && job.message_count() == 1
                && job.claim_token() == Some("race-claim")));
        let queued = jobs
            .iter()
            .find(|job| job.status() == NotificationOutboxStatus::Queued)
            .expect("fresh queued job from retry");
        assert_eq!(queued.message_count(), 1);
        assert_eq!(
            queued.sender_jids(),
            &["bob@example.com/test-resource"
                .parse::<Jid>()
                .expect("sender resource")]
        );
        let claimed = store
            .claim_due_outbox_jobs(16)
            .await
            .expect("claim replacement job");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].status(), NotificationOutboxStatus::InProgress);
        assert_eq!(claimed[0].message_count(), 1);
        assert_ne!(claimed[0].claim_token(), Some("race-claim"));
        assert!(store
            .pending_candidates(16)
            .await
            .expect("pending candidates")
            .is_empty());
    }

    #[tokio::test]
    async fn coalescing_new_candidate_clears_retry_backoff() {
        let store = store().await;
        let target = target();
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-1"),
            std::slice::from_ref(&target),
        )
        .await;
        let claimed = store
            .claim_due_outbox_jobs(16)
            .await
            .expect("claim")
            .into_iter()
            .next()
            .expect("claimed job");
        assert_eq!(
            store
                .schedule_retry_or_fail(&claimed, "temporary failure".to_string())
                .await
                .expect("schedule retry"),
            Some(1)
        );
        assert!(
            store
                .claim_due_outbox_jobs(16)
                .await
                .expect("backoff claim")
                .is_empty(),
            "retry backoff should hide the job until new work arrives"
        );

        enqueue_jobs_for_test(&store, &candidate("archive-2"), &[target]).await;

        let reclaimed = store
            .claim_due_outbox_jobs(16)
            .await
            .expect("claim after coalesce");
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].message_count(), 2);
        assert_eq!(reclaimed[0].attempt_count(), 1);
    }

    #[tokio::test]
    async fn xep0357_publish_count_is_derived_from_current_inbox_unread() {
        let store = store().await;
        let recipient = bare("alice@example.com");
        let conversation = bare("bob@example.com");
        enqueue_jobs_for_test(&store, &candidate("archive-1"), &[target()]).await;
        let claimed = store
            .claim_due_outbox_jobs(16)
            .await
            .expect("claim")
            .into_iter()
            .next()
            .expect("claimed job");
        assert_eq!(claimed.message_count(), 1);
        let inbox = inbox_with_unread(&recipient, &conversation, 3).await;

        let current_count = current_unread_count_for_job(&claimed, &inbox)
            .await
            .expect("current unread");
        let item = claimed.to_xep0357_pubsub_item_with_count(current_count);
        let payload = item.payload.expect("payload");
        let summary = payload
            .children()
            .find(|child| child.is("x", NS_DATA_FORMS))
            .expect("summary form");

        assert!(summary.children().any(|field| {
            field.is("field", NS_DATA_FORMS)
                && field.attr("var") == Some("message-count")
                && field
                    .children()
                    .any(|value| value.is("value", NS_DATA_FORMS) && value.text() == "3")
        }));
    }

    #[tokio::test]
    async fn publish_rejects_non_first_party_outbox_target() {
        let store = store().await;
        enqueue_jobs_for_test(&store, &candidate("archive-1"), &[foreign_target()]).await;
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
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
        let inbox =
            inbox_with_unread(&bare("alice@example.com"), &bare("bob@example.com"), 1).await;

        let outcomes = store
            .drain_due_outbox_jobs(
                &push_service,
                &push_store,
                &inbox,
                &blocking,
                &bare("push.example.com"),
                16,
            )
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
    async fn xep0191_blocked_dm_outbox_job_does_not_publish_push_notification() {
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        blocking.set_blocklist(recipient.clone(), vec![sender.clone()]);
        let (outcomes, queued_push_job_count, pending_jobs) =
            drain_dm_outbox_with_blocking("archive-blocked-bare", &blocking).await;

        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            outcomes[0],
            NotificationOutboxPublishOutcome::Failed { .. }
        ));
        assert_eq!(
            queued_push_job_count, 0,
            "XEP-0191-blocked DMs must not enqueue XEP-0357 push publish jobs"
        );
        assert!(
            pending_jobs.is_empty(),
            "blocked notification jobs should become terminal instead of retrying forever"
        );
    }

    #[tokio::test]
    async fn xep0191_full_jid_block_suppresses_dm_push_candidate() {
        let recipient = bare("alice@example.com");
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        blocking.set_blocklist_jids(
            recipient,
            vec!["bob@example.com/phone".parse().expect("full blocked JID")],
        );

        let (outcomes, queued_push_job_count, pending_jobs) =
            drain_dm_outbox_with_blocking("archive-blocked-full", &blocking).await;

        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            outcomes[0],
            NotificationOutboxPublishOutcome::Failed { .. }
        ));
        assert_eq!(queued_push_job_count, 0);
        assert!(pending_jobs.is_empty());
    }

    #[tokio::test]
    async fn xep0191_full_jid_block_does_not_suppress_other_sender_resource() {
        let recipient = bare("alice@example.com");
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        blocking.set_blocklist_jids(
            recipient,
            vec!["bob@example.com/phone".parse().expect("full blocked JID")],
        );

        let (outcomes, queued_push_job_count, pending_jobs) = drain_dm_outbox_with_sender_jid(
            "archive-full-block-other-resource",
            "bob@example.com/laptop".parse().expect("sender resource"),
            &blocking,
        )
        .await;

        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            outcomes[0],
            NotificationOutboxPublishOutcome::Published { .. }
        ));
        assert_eq!(
            queued_push_job_count, 1,
            "a full-JID XEP-0191 block must not suppress another resource from the same bare JID"
        );
        assert!(pending_jobs.is_empty());
    }

    #[tokio::test]
    async fn xep0191_domain_block_suppresses_dm_push_candidate() {
        let recipient = bare("alice@example.com");
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        blocking.set_blocklist_jids(
            recipient,
            vec!["example.com".parse().expect("domain blocked JID")],
        );

        let (outcomes, queued_push_job_count, pending_jobs) =
            drain_dm_outbox_with_blocking("archive-blocked-domain", &blocking).await;

        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            outcomes[0],
            NotificationOutboxPublishOutcome::Failed { .. }
        ));
        assert_eq!(queued_push_job_count, 0);
        assert!(pending_jobs.is_empty());
    }

    #[derive(Debug, thiserror::Error)]
    #[error("blocking storage unavailable")]
    struct BlockingStorageUnavailable;

    struct FailingBlockingStorage;

    #[async_trait::async_trait]
    impl BlockingStorage for FailingBlockingStorage {
        async fn list_blocked_jids(
            &self,
            _user: &BareJid,
        ) -> Result<Vec<BareJid>, BlockingStorageError> {
            Err(BlockingStorageError::new(BlockingStorageUnavailable))
        }
    }

    #[tokio::test]
    async fn xep0191_blocklist_load_error_preserves_outbox_job_without_spending_attempt() {
        let (outcomes, queued_push_job_count, pending_jobs) = drain_dm_outbox_with_blocking(
            "archive-blocking-storage-error",
            &FailingBlockingStorage,
        )
        .await;

        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            outcomes[0],
            NotificationOutboxPublishOutcome::RetryScheduled { .. }
        ));
        assert_eq!(
            queued_push_job_count, 0,
            "policy-read failures must not publish before XEP-0191 can be enforced"
        );
        assert_eq!(pending_jobs.len(), 1);
        assert_eq!(pending_jobs[0].status(), NotificationOutboxStatus::Queued);
        assert_eq!(pending_jobs[0].attempt_count(), 0);
        assert_eq!(pending_jobs[0].policy_error_count(), 1);
        assert!(pending_jobs[0].claim_token().is_none());
    }

    struct FailingPushStore;

    impl waddle_xmpp::push::PushSubscriptionStore for FailingPushStore {
        fn register(
            &self,
            _sub: waddle_xmpp::push::PushSubscription,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<(), waddle_xmpp::push::PushError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(std::future::ready(Ok(())))
        }

        fn remove(
            &self,
            _user_jid: &str,
            _service_jid: &str,
            _node: Option<&str>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<(), waddle_xmpp::push::PushError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(std::future::ready(Ok(())))
        }

        fn get_for_user(
            &self,
            _user_jid: &str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            Vec<waddle_xmpp::push::PushSubscription>,
                            waddle_xmpp::push::PushError,
                        >,
                    > + Send
                    + '_,
            >,
        > {
            Box::pin(std::future::ready(Err(
                waddle_xmpp::push::PushError::StorageError("registration store unavailable".into()),
            )))
        }
    }

    #[tokio::test]
    async fn push_registration_lookup_error_retries_each_claimed_outbox_job() {
        let store = store().await;
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-1"),
            &[target_named("web-node-1"), target_named("web-node-2")],
        )
        .await;
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let push_service = crate::push_service::DatabasePushServiceStore::new(
            Database::in_memory("push-service").await.unwrap(),
        )
        .await
        .expect("push service");
        let inbox =
            inbox_with_unread(&bare("alice@example.com"), &bare("bob@example.com"), 1).await;

        let outcomes = store
            .drain_due_outbox_jobs(
                &push_service,
                &FailingPushStore,
                &inbox,
                &blocking,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain");

        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|outcome| matches!(
            outcome,
            NotificationOutboxPublishOutcome::RetryScheduled { .. }
        )));
        let pending = store.pending_outbox_jobs().await.expect("pending");
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|job| {
            job.status() == NotificationOutboxStatus::Queued && job.attempt_count() == 1
        }));
    }

    #[tokio::test]
    async fn prune_completed_removes_only_finished_jobs_and_outboxed_candidates() {
        let store = store().await;
        enqueue_jobs_for_test(&store, &candidate("archive-old"), &[target()]).await;
        let old_job = store
            .claim_due_outbox_jobs(16)
            .await
            .expect("claim old job")
            .into_iter()
            .next()
            .expect("old job");
        assert!(store.mark_job_published(&old_job).await.expect("published"));

        enqueue_jobs_for_test(&store, &candidate("archive-live"), &[target()]).await;
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

    #[tokio::test]
    async fn prune_completed_deletes_outboxed_candidates_in_ordered_batches() {
        let store = store().await;
        enqueue_jobs_for_test(&store, &candidate("archive-oldest"), &[target()]).await;
        enqueue_jobs_for_test(&store, &candidate("archive-older"), &[target()]).await;
        enqueue_jobs_for_test(&store, &candidate("archive-live"), &[target()]).await;
        let cutoff_ms = crate::time::now_ms().saturating_sub(1_000);
        let oldest_ms = cutoff_ms.saturating_sub(2);
        let older_ms = cutoff_ms.saturating_sub(1);
        let live_ms = cutoff_ms.saturating_add(1);
        store
            .execute(
                "UPDATE notification_candidates SET outboxed_at_ms = ? WHERE stanza_id = ?",
                crate::db_params![oldest_ms, "archive-oldest"],
            )
            .await
            .expect("age oldest candidate");
        store
            .execute(
                "UPDATE notification_candidates SET outboxed_at_ms = ? WHERE stanza_id = ?",
                crate::db_params![older_ms, "archive-older"],
            )
            .await
            .expect("age older candidate");
        store
            .execute(
                "UPDATE notification_candidates SET outboxed_at_ms = ? WHERE stanza_id = ?",
                crate::db_params![live_ms, "archive-live"],
            )
            .await
            .expect("keep live candidate");

        let pruned = store
            .prune_completed_before(cutoff_ms, 1)
            .await
            .expect("prune");

        assert_eq!(pruned.candidates_deleted, 1);
        assert_eq!(pruned.jobs_deleted, 0);
        let mut rows = store
            .query(
                "SELECT stanza_id FROM notification_candidates ORDER BY outboxed_at_ms ASC",
                (),
            )
            .await
            .expect("candidate query");
        let mut remaining = Vec::new();
        while let Some(row) = rows.next().await.expect("candidate row") {
            remaining.push(row.get::<String>(0).expect("stanza id"));
        }
        assert_eq!(
            remaining,
            vec!["archive-older".to_string(), "archive-live".to_string()]
        );
    }
}
