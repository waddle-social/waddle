//! Typed identifiers, classes, reasons, statuses, outcomes, and errors
//! for the notification outbox.

use super::*;

pub(super) const STATUS_QUEUED: &str = "queued";
pub(super) const STATUS_IN_PROGRESS: &str = "in-progress";
pub(super) const STATUS_PUBLISHED: &str = "published";
pub(super) const STATUS_FAILED: &str = "failed";

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
    pub(super) fn fresh() -> Self {
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
    DirectMessageMention,
    PersonalMention,
    ChannelMention,
    ActiveChannelMention,
    NotifyAll,
}

impl NotificationClass {
    pub(crate) fn as_db_value(self) -> &'static str {
        match self {
            Self::DirectMessage => "dm",
            Self::DirectMessageMention => "dm_mention",
            Self::PersonalMention => "personal_mention",
            Self::ChannelMention => "channel_mention",
            Self::ActiveChannelMention => "active_channel_mention",
            Self::NotifyAll => "notify_all",
        }
    }

    pub(super) fn from_db_value(value: &str) -> Result<Self, NotificationOutboxError> {
        match value {
            "dm" => Ok(Self::DirectMessage),
            "dm_mention" => Ok(Self::DirectMessageMention),
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
    OfflineDirectMessageMention,
    GroupchatPersonalMention,
    GroupchatChannelMention,
    GroupchatActiveChannelMention,
    GroupchatNotifyAll,
}

impl NotificationReason {
    pub(super) fn as_db_value(self) -> &'static str {
        match self {
            Self::OfflineDirectMessage => "offline_dm",
            Self::OfflineDirectMessageMention => "offline_dm_mention",
            Self::GroupchatPersonalMention => "groupchat_personal_mention",
            Self::GroupchatChannelMention => "groupchat_channel_mention",
            Self::GroupchatActiveChannelMention => "groupchat_active_channel_mention",
            Self::GroupchatNotifyAll => "groupchat_notify_all",
        }
    }

    pub(super) fn from_db_value(value: &str) -> Result<Self, NotificationOutboxError> {
        match value {
            "offline_dm" => Ok(Self::OfflineDirectMessage),
            "offline_dm_mention" => Ok(Self::OfflineDirectMessageMention),
            "groupchat_personal_mention" => Ok(Self::GroupchatPersonalMention),
            "groupchat_channel_mention" => Ok(Self::GroupchatChannelMention),
            "groupchat_active_channel_mention" => Ok(Self::GroupchatActiveChannelMention),
            "groupchat_notify_all" => Ok(Self::GroupchatNotifyAll),
            _ => Err(NotificationOutboxError::InvalidReason(value.to_string())),
        }
    }
}

/// Typed audit reason for a suppressed XEP-0357 notification candidate.
///
/// Closed set — one variant per XEP/Waddle rule that can suppress a
/// push notification. Persisted into `notification_candidates.suppressed_reason`
/// alongside the existing `class`/`reason` columns whenever the T1
/// drain decides to mark a candidate outboxed *without* enqueueing a
/// job. Also labels the `xmpp.push.suppressed` OTel counter (exported
/// to Mimir under the `waddle_push_suppressed_total` alias) so
/// deployments can observe per-rule suppression rates.
///
/// `SuppressedReason` is the **audit shape**, distinct from the
/// dispatch shape ([`T1PushDispatchOutcome`]): the evaluator decides
/// publish/suppress/defer, this enum records *why* a suppression
/// happened so adversarial review can branch on the exact rule
/// without stringly-typed sniffing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SuppressedReason {
    /// Self-DM rejected at the [`NotificationCandidate`] constructor
    /// boundary. Reserved variant; emitted by the constructor-rejection
    /// path is structurally a no-row outcome, but the audit shape
    /// exists so race-window state changes can record it.
    Xep0357Self,
    /// Recipient has no usable active first-party XEP-0357 push
    /// registration when the T1 drain resolves delivery targets.
    Xep0357NoRegistration,
    /// Recipient's XEP-0357 push registration was disabled in
    /// response to a stanza-error from the push service
    /// (XEP-0357 §6). Reserved variant; emitted at publish time.
    Xep0357RegistrationDisabled,
    /// XEP-0492 setting resolved to `<never/>` at T1.
    Xep0492Never,
    /// XEP-0492 setting resolved to `<on-mention/>` and the candidate
    /// is not a mention class.
    Xep0492OnMentionMiss,
    /// XEP-0191 blocklist matched the candidate's sender/conversation.
    Xep0191Blocked,
    /// XEP-0513 explicit mention carried `<noping/>` — sender opted
    /// out of pinging this recipient for the mention.
    Xep0513Noping,
    /// XEP-0513 `<active/>` filter missed — reserved variant; emitted
    /// in slice 2b once the `notification_activity` projection lands.
    Xep0513ActiveMiss,
    /// Waddle DnD state is active for the recipient.
    WaddleDnd,
    /// Push provider rejected the published payload. Reserved variant;
    /// emitted by provider slices (#528 / #529 / #530).
    ProviderRejected,
    /// Push provider returned an expired-token signal. Reserved
    /// variant; emitted by provider slices.
    ProviderTokenExpired,
    /// The XEP-0357 push service is degraded — currently means the
    /// VAPID signer or Web Push transport is not wired (a
    /// boot-time failure on the
    /// [`WebPushCapability`](waddle_xmpp::push::WebPushCapability)
    /// path). Reserved variant: this PR introduces the typed
    /// suppression reason so a future in-band fallback path
    /// (e.g. plaintext chat-notify) cannot accidentally leak
    /// content when the encrypted push path is unavailable. No
    /// current code path falls back to in-band notify, so today
    /// this variant is never emitted — the audit shape is in
    /// place for the day one is added.
    Xep0357PushServiceDegraded,
    /// The conversation's unread count was already 0 when the
    /// outbox job reached publish — the recipient reconnected and
    /// read the message inside the notification window, so an OS
    /// push would be spurious (#1126). Emitted at publish time;
    /// the job is dropped terminally instead of published.
    UnreadZeroAtPublish,
    /// T1 policy evaluation deferred too many times for a durable
    /// candidate, most commonly because the room policy is permanently
    /// unavailable after a MUC room was evicted and never became live
    /// again. Emitted by the candidate drain as a terminal dead-letter.
    PolicyRetriesExhausted,
    /// The originating message was XEP-0444 reaction-only (reactions
    /// payload, no substantive body) — "Alice reacted 👍" is archived
    /// but never fires an OS push (#780). Message-frozen at T0,
    /// suppressed by the T1 evaluator like `<noping/>`.
    Xep0444Reaction,
}

impl SuppressedReason {
    /// Every variant of the closed audit set, in declaration order.
    ///
    /// Exposed so the runtime schema migration / test surface can
    /// iterate the typed values without re-declaring them. Reserved
    /// variants (`Xep0357Self`, the provider variants, and
    /// `Xep0513ActiveMiss`) are kept in this list deliberately — they
    /// are part of the closed audit shape today; provider slices and
    /// slice 2b wire the emitters.
    pub(crate) const ALL: &'static [SuppressedReason] = &[
        Self::Xep0357Self,
        Self::Xep0357NoRegistration,
        Self::Xep0357RegistrationDisabled,
        Self::Xep0492Never,
        Self::Xep0492OnMentionMiss,
        Self::Xep0191Blocked,
        Self::Xep0513Noping,
        Self::Xep0513ActiveMiss,
        Self::WaddleDnd,
        Self::ProviderRejected,
        Self::ProviderTokenExpired,
        Self::Xep0357PushServiceDegraded,
        Self::UnreadZeroAtPublish,
        Self::PolicyRetriesExhausted,
        Self::Xep0444Reaction,
    ];

    pub(crate) fn as_db_value(self) -> &'static str {
        match self {
            Self::Xep0357Self => "xep0357_self",
            Self::Xep0357NoRegistration => "xep0357_no_registration",
            Self::Xep0357RegistrationDisabled => "xep0357_registration_disabled",
            Self::Xep0492Never => "xep0492_never",
            Self::Xep0492OnMentionMiss => "xep0492_on_mention_miss",
            Self::Xep0191Blocked => "xep0191_blocked",
            Self::Xep0513Noping => "xep0513_noping",
            Self::Xep0513ActiveMiss => "xep0513_active_miss",
            Self::WaddleDnd => "waddle_dnd",
            Self::ProviderRejected => "provider_rejected",
            Self::ProviderTokenExpired => "provider_token_expired",
            Self::Xep0357PushServiceDegraded => "xep0357_push_service_degraded",
            Self::UnreadZeroAtPublish => "unread_zero_at_publish",
            Self::PolicyRetriesExhausted => "policy_retries_exhausted",
            Self::Xep0444Reaction => "xep0444_reaction",
        }
    }

    /// Convert the persisted audit reason into the metric-facing closed set.
    /// Exhaustive matching keeps the boundary typed and makes new variants a
    /// compile-time decision for telemetry as well as storage.
    pub(crate) fn telemetry_reason(self) -> waddle_xmpp::telemetry::attributes::PushSuppressReason {
        use waddle_xmpp::telemetry::attributes::PushSuppressReason as MetricReason;

        match self {
            Self::Xep0357Self => MetricReason::Xep0357Self,
            Self::Xep0357NoRegistration => MetricReason::Xep0357NoRegistration,
            Self::Xep0357RegistrationDisabled => MetricReason::Xep0357RegistrationDisabled,
            Self::Xep0492Never => MetricReason::Xep0492Never,
            Self::Xep0492OnMentionMiss => MetricReason::Xep0492OnMentionMiss,
            Self::Xep0191Blocked => MetricReason::Xep0191Blocked,
            Self::Xep0513Noping => MetricReason::Xep0513Noping,
            Self::Xep0513ActiveMiss => MetricReason::Xep0513ActiveMiss,
            Self::WaddleDnd => MetricReason::WaddleDnd,
            Self::ProviderRejected => MetricReason::ProviderRejected,
            Self::ProviderTokenExpired => MetricReason::ProviderTokenExpired,
            Self::Xep0357PushServiceDegraded => MetricReason::Xep0357PushServiceDegraded,
            Self::UnreadZeroAtPublish => MetricReason::UnreadZeroAtPublish,
            Self::PolicyRetriesExhausted => MetricReason::PolicyRetriesExhausted,
            Self::Xep0444Reaction => MetricReason::Xep0444Reaction,
        }
    }

    pub(crate) fn from_db_value(value: &str) -> Result<Self, NotificationOutboxError> {
        // Iterate the closed `ALL` set rather than re-listing the
        // variants in a `match` arm — keeps the audit shape, the
        // schema CHECK list, and the metric `reason` label set in
        // lockstep without three independently-edited match arms.
        Self::ALL
            .iter()
            .copied()
            .find(|variant| variant.as_db_value() == value)
            .ok_or_else(|| NotificationOutboxError::InvalidSuppressedReason(value.to_string()))
    }
}

impl std::fmt::Display for SuppressedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_db_value())
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
    pub(super) fn from_db_value(value: &str) -> Result<Self, NotificationOutboxError> {
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
pub struct NotificationOutboxTarget {
    pub(super) push_service_jid: BareJid,
    pub(super) node: PushServiceNodeName,
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
    pub(super) job_id: NotificationOutboxJobId,
    pub(super) recipient_bare_jid: BareJid,
    pub(super) push_service_jid: BareJid,
    pub(super) node: PushServiceNodeName,
    pub(super) conversation_jid: BareJid,
    pub(super) sender_jid: Jid,
    pub(super) sender_jids: Vec<Jid>,
    pub(super) thread_id: NotificationThreadId,
    pub(super) class: NotificationClass,
    pub(super) message_count: u32,
    pub(super) context: Element,
    /// Resolved XEP-0357 §5.4 rich summary fields, decided at T1 drain
    /// from the recipient's XEP-0492 opt-in and the candidate's
    /// XEP-0334 hints. Defaults to [`RichSummary::minimal`] (opt-out).
    pub(super) rich_summary: RichSummary,
    pub(super) status: NotificationOutboxStatus,
    pub(super) attempt_count: i64,
    pub(super) policy_error_count: i64,
    pub(super) claim_token: Option<String>,
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

    pub fn rich_summary(&self) -> &RichSummary {
        &self.rich_summary
    }

    pub fn to_xep0357_pubsub_item_with_count(&self, message_count: u32) -> PubSubItem {
        PubSubItem::new(
            Some(self.job_id.as_str().to_string()),
            Some(build_xep0357_notification_payload(
                message_count,
                &self.rich_summary,
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
    /// The job was terminally dropped at publish time because the
    /// recipient had already read the conversation (unread count 0)
    /// — pushing would be spurious (#1126).
    Suppressed {
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
    #[error("invalid suppressed reason: {0}")]
    InvalidSuppressedReason(String),
    #[error("notification settings projection error: {0}")]
    NotificationSettings(
        #[from] crate::notification_settings_projection::NotificationSettingsProjectionError,
    ),
    #[error("notification activity projection error: {0}")]
    NotificationActivity(#[from] NotificationActivityError),
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
    #[error("notification candidate sender bare JID equals recipient bare JID: {0}")]
    SelfDirectedNotificationCandidate(BareJid),
    #[error("room policy lookup failed for {room}: {message}")]
    RoomPolicyLookup { room: BareJid, message: String },
    #[error("message count is out of range: {0}")]
    InvalidMessageCount(i64),
    #[error("notification outbox coalesce contention persisted after retry")]
    OutboxCoalesceContention,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification_outbox::schema::NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES;

    // ─────────────────────────────────────────────────────────────
    // Slice 2a — SuppressedReason audit + new suppressors (#526).
    // ─────────────────────────────────────────────────────────────

    /// `SuppressedReason` is the canonical audit shape. Every variant
    /// MUST round-trip through `as_db_value` / `from_db_value` so a
    /// row written today can be decoded tomorrow without ambiguity.
    /// The closed-set discipline is what keeps the CHECK constraint
    /// + the `reason`-labeled suppression counter in lockstep.
    #[test]
    fn suppressed_reason_round_trip_covers_every_variant() {
        // Iterate `SuppressedReason::ALL` (the same closed-set array the
        // startup invariant traverses) so any future variant addition
        // joins this test automatically — no parallel hand-maintained
        // list to drift.
        assert_eq!(
            SuppressedReason::ALL.len(),
            NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES.len(),
            "variant count must match CHECK constraint value list",
        );
        for variant in SuppressedReason::ALL.iter().copied() {
            let db = variant.as_db_value();
            assert!(
                NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES.contains(&db),
                "variant {variant:?} db value {db} missing from CHECK constraint list",
            );
            let decoded = SuppressedReason::from_db_value(db).expect("decode");
            assert_eq!(
                decoded, variant,
                "round-trip failed for {variant:?} (db value {db})"
            );
        }
        assert!(matches!(
            SuppressedReason::from_db_value("nonsense"),
            Err(NotificationOutboxError::InvalidSuppressedReason(_))
        ));
    }
}
