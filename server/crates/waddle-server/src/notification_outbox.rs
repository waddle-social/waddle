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
use crate::notification_activity::{
    NotificationActivity, NotificationActivityError, NotificationActivityReader,
};

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
const NOTIFICATION_CANDIDATES_REASON_CHECK_NAME: &str = "notification_candidates_reason_check";
const NOTIFICATION_CANDIDATES_REASON_VALUES: [&str; 6] = [
    "offline_dm",
    "offline_dm_mention",
    "groupchat_personal_mention",
    "groupchat_channel_mention",
    "groupchat_active_channel_mention",
    "groupchat_notify_all",
];
const NOTIFICATION_CANDIDATES_REASON_CHECK_SQL: &str = "reason IN ('offline_dm', 'offline_dm_mention', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all')";
const NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME: &str = "notification_candidates_class_check";
const NOTIFICATION_CANDIDATES_CLASS_VALUES: [&str; 6] = [
    "dm",
    "dm_mention",
    "personal_mention",
    "channel_mention",
    "active_channel_mention",
    "notify_all",
];
const NOTIFICATION_CANDIDATES_CLASS_CHECK_SQL: &str = "class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')";
const NOTIFICATION_OUTBOX_CLASS_CHECK_NAME: &str = "notification_outbox_class_check";
const NOTIFICATION_OUTBOX_CLASS_VALUES: [&str; 6] = [
    "dm",
    "dm_mention",
    "personal_mention",
    "channel_mention",
    "active_channel_mention",
    "notify_all",
];
const NOTIFICATION_OUTBOX_CLASS_CHECK_SQL: &str = "class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')";
const NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_NAME: &str =
    "notification_candidates_suppressed_reason_check";
const NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES: [&str; 12] = [
    "xep0357_self",
    "xep0357_no_registration",
    "xep0357_registration_disabled",
    "xep0492_never",
    "xep0492_on_mention_miss",
    "xep0191_blocked",
    "xep0513_noping",
    "xep0513_active_miss",
    "waddle_dnd",
    "provider_rejected",
    "provider_token_expired",
    "xep0357_push_service_degraded",
];
const NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_SQL: &str = "suppressed_reason IS NULL OR suppressed_reason IN ('xep0357_self', 'xep0357_no_registration', 'xep0357_registration_disabled', 'xep0492_never', 'xep0492_on_mention_miss', 'xep0191_blocked', 'xep0513_noping', 'xep0513_active_miss', 'waddle_dnd', 'provider_rejected', 'provider_token_expired', 'xep0357_push_service_degraded')";
const NOTIFICATION_CANDIDATES_INDEXES: [&str; 4] = [
    "idx_notification_candidates_recipient_created",
    "idx_notification_candidates_identity",
    "idx_notification_candidates_pending_worker",
    "idx_notification_candidates_outboxed_prune",
];
const NOTIFICATION_OUTBOX_INDEXES: [&str; 4] = [
    "idx_notification_outbox_queued_coalesce",
    "idx_notification_outbox_conversation_status",
    "idx_notification_outbox_status_next_attempt",
    "idx_notification_outbox_retention_prune",
];

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
    DirectMessageMention,
    PersonalMention,
    ChannelMention,
    ActiveChannelMention,
    NotifyAll,
}

impl NotificationClass {
    fn as_db_value(self) -> &'static str {
        match self {
            Self::DirectMessage => "dm",
            Self::DirectMessageMention => "dm_mention",
            Self::PersonalMention => "personal_mention",
            Self::ChannelMention => "channel_mention",
            Self::ActiveChannelMention => "active_channel_mention",
            Self::NotifyAll => "notify_all",
        }
    }

    fn from_db_value(value: &str) -> Result<Self, NotificationOutboxError> {
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
    fn as_db_value(self) -> &'static str {
        match self {
            Self::OfflineDirectMessage => "offline_dm",
            Self::OfflineDirectMessageMention => "offline_dm_mention",
            Self::GroupchatPersonalMention => "groupchat_personal_mention",
            Self::GroupchatChannelMention => "groupchat_channel_mention",
            Self::GroupchatActiveChannelMention => "groupchat_active_channel_mention",
            Self::GroupchatNotifyAll => "groupchat_notify_all",
        }
    }

    fn from_db_value(value: &str) -> Result<Self, NotificationOutboxError> {
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
/// job. Also labels the `waddle_push_suppressed_total` metric so
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
    /// Recipient has no XEP-0357 push registration. Reserved variant;
    /// emitted by the publish-layer disable transitions
    /// (XEP-0357 §6) in later slices.
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
        }
    }

    pub(crate) fn from_db_value(value: &str) -> Result<Self, NotificationOutboxError> {
        // Iterate the closed `ALL` set rather than re-listing the
        // variants in a `match` arm — keeps the audit shape, the
        // schema CHECK list, and the prometheus label set in
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
    /// XEP-0513 `<noping/>` mention hint, message-frozen at T0. When
    /// `true`, the T1 evaluator suppresses the candidate with
    /// [`SuppressedReason::Xep0513Noping`] — sender opted the
    /// recipient out of being pinged for this mention.
    noping: bool,
    /// XEP-0334 `<no-store/>` hint, message-frozen at T0. When `true`
    /// the body is stripped from the XEP-0357 summary at T1 (the
    /// minimal push still fires).
    no_store: bool,
    /// XEP-0334 `<no-permanent-store/>` hint, message-frozen at T0.
    /// When `true` the body is stripped from the XEP-0357 summary at T1
    /// (the minimal push still fires).
    no_permanent_store: bool,
    /// Snapshot of the message body, message-frozen at T0, used to build
    /// the optional XEP-0357 §5.4 `last-message-body` field when the
    /// recipient opts in (see [`RichSummary`]). `None` when the message
    /// had no body OR when an XEP-0334 `<no-store/>`/`<no-permanent-store/>`
    /// hint applies — an off-the-record body is never persisted onto the
    /// candidate row, even temporarily (XEP-0334 §3 storage conformance).
    last_message_body: Option<String>,
}

/// Message-frozen suppression hints carried on a
/// [`NotificationCandidate`] from T0 emission to T1 dispatch.
///
/// Per locked Q3 (see #506), T0 declines candidates only for
/// structural-validity reasons (self-DM). Message-frozen suppression
/// hints like XEP-0513 `<noping/>` and XEP-0334 storage hints are
/// recipient-level signals from the sender — the candidate is still
/// constructed and persisted, and the T1 evaluator reads the hint
/// back and suppresses with the typed `SuppressedReason`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotificationMessageHints {
    pub noping: bool,
    pub no_store: bool,
    pub no_permanent_store: bool,
}

impl NotificationMessageHints {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn with_noping(mut self, noping: bool) -> Self {
        self.noping = noping;
        self
    }

    pub fn with_xep0334(mut self, no_store: bool, no_permanent_store: bool) -> Self {
        self.no_store = no_store;
        self.no_permanent_store = no_permanent_store;
        self
    }
}

impl NotificationCandidate {
    pub fn direct_message(
        recipient_bare_jid: BareJid,
        sender_jid: Jid,
        archive_stanza_id: StanzaId,
        is_mention: bool,
    ) -> Result<Self, NotificationOutboxError> {
        Self::direct_message_with_hints(
            recipient_bare_jid,
            sender_jid,
            archive_stanza_id,
            is_mention,
            NotificationMessageHints::none(),
        )
    }

    pub fn direct_message_with_hints(
        recipient_bare_jid: BareJid,
        sender_jid: Jid,
        archive_stanza_id: StanzaId,
        is_mention: bool,
        hints: NotificationMessageHints,
    ) -> Result<Self, NotificationOutboxError> {
        require_full_sender_jid(&sender_jid)?;
        // Structural invariant: a notification candidate cannot be
        // self-directed. A self-DM (sender bare JID == recipient bare
        // JID) is not a valid push candidate at all — there is no
        // distinct recipient to notify, so the candidate is malformed
        // by construction. This is *input validation*, not recipient-
        // state suppression, so it lives at the constructor boundary
        // alongside the existing full-sender-JID and archive-id owner
        // checks. T0 emission paths surface this error as a typed
        // emission no-op; no candidate row is persisted. (Per #506 Q3:
        // T0 has no recipient-state reads — sender vs recipient JID
        // comparison is message-intrinsic provenance.)
        if sender_jid.to_bare() == recipient_bare_jid {
            return Err(NotificationOutboxError::SelfDirectedNotificationCandidate(
                recipient_bare_jid,
            ));
        }
        let expected_by = Jid::from(recipient_bare_jid.clone());
        if archive_stanza_id.by != expected_by {
            return Err(NotificationOutboxError::ArchiveStanzaIdOwnerMismatch {
                expected: expected_by,
                actual: archive_stanza_id.by,
            });
        }
        let (class, reason) = if is_mention {
            (
                NotificationClass::DirectMessageMention,
                NotificationReason::OfflineDirectMessageMention,
            )
        } else {
            (
                NotificationClass::DirectMessage,
                NotificationReason::OfflineDirectMessage,
            )
        };
        Ok(Self {
            recipient_bare_jid,
            conversation_jid: sender_jid.to_bare(),
            sender_jid,
            thread_id: NotificationThreadId::root(),
            archive_stanza_id,
            class,
            reason,
            policy_error_count: 0,
            noping: hints.noping,
            no_store: hints.no_store,
            no_permanent_store: hints.no_permanent_store,
            last_message_body: None,
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
        Self::groupchat_with_hints(
            recipient_bare_jid,
            conversation_jid,
            sender_jid,
            thread_id,
            archive_stanza_id,
            class,
            NotificationMessageHints::none(),
        )
    }

    pub fn groupchat_with_hints(
        recipient_bare_jid: BareJid,
        conversation_jid: BareJid,
        sender_jid: Jid,
        thread_id: NotificationThreadId,
        archive_stanza_id: StanzaId,
        class: NotificationClass,
        hints: NotificationMessageHints,
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
            NotificationClass::DirectMessage | NotificationClass::DirectMessageMention => {
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
            noping: hints.noping,
            no_store: hints.no_store,
            no_permanent_store: hints.no_permanent_store,
            last_message_body: None,
        })
    }

    /// Snapshot the message body for the optional XEP-0357 §5.4
    /// `last-message-body` field.
    ///
    /// XEP-0334 §3 storage conformance: when this candidate carries a
    /// `<no-store/>` or `<no-permanent-store/>` hint, the body is dropped
    /// here so an off-the-record body is never persisted onto the
    /// candidate row — not even temporarily. The T1 evaluator applies the
    /// same hint precedence again when resolving the [`RichSummary`]
    /// (defense in depth + the XEP-defined T1 decision point).
    pub fn with_last_message_body(mut self, body: Option<String>) -> Self {
        self.last_message_body = if self.no_store || self.no_permanent_store {
            None
        } else {
            body
        };
        self
    }

    pub fn last_message_body(&self) -> Option<&str> {
        self.last_message_body.as_deref()
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

    pub fn noping(&self) -> bool {
        self.noping
    }

    pub fn no_store(&self) -> bool {
        self.no_store
    }

    pub fn no_permanent_store(&self) -> bool {
        self.no_permanent_store
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
    /// Resolved XEP-0357 §5.4 rich summary fields, decided at T1 drain
    /// from the recipient's XEP-0492 opt-in and the candidate's
    /// XEP-0334 hints. Defaults to [`RichSummary::minimal`] (opt-out).
    rich_summary: RichSummary,
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

/// T1 lookup of XEP-0045 room policy state needed to project a
/// candidate's conversation kind for the XEP-0492 evaluator.
///
/// At T0 (candidate emission) we record the message-derived
/// [`NotificationClass`] only. At T1 (outbox dispatch) the evaluator
/// needs to know whether the room is members-only to pick
/// [`crate::notification_settings_projection::ConversationKind::PrivateGroup`]
/// or [`crate::notification_settings_projection::ConversationKind::PublicGroup`]
/// — the kind drives both the XEP-0492 default level and the projection
/// store lookup.
///
/// Returning `Ok(None)` signals "room not currently live" — the T1
/// evaluator treats this as an *unknown* signal (not a public one)
/// and defers the candidate via the policy-error backoff so the next
/// drain pass can retry once the actor is reachable. Slice 2 will
/// replace the live-actor lookup with a durable T1 projection of
/// MUC config that does not have this hole.
#[async_trait::async_trait]
pub trait RoomPolicyStore: Send + Sync {
    async fn room_members_only(
        &self,
        room: &BareJid,
    ) -> Result<Option<bool>, NotificationOutboxError>;
}

/// Zero-state [`RoomPolicyStore`] for DM emission paths.
///
/// The T0 emission gate for direct messages calls
/// [`evaluate_push_gate_at_dispatch`] on a candidate whose class is
/// [`NotificationClass::DirectMessage`] or
/// [`NotificationClass::DirectMessageMention`]. Those arms never
/// dispatch into `room_policy`, so the trait object is held only to
/// satisfy the typed signature. This adapter encodes that no-op shape
/// once at the type level; if the evaluator ever did consult it for a
/// DM, it would surface as [`T1PushDispatchOutcome::DeferUnknownRoomPolicy`]
/// rather than a silent default — fail-loud per the slice 1 design.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRoomPolicy;

#[async_trait::async_trait]
impl RoomPolicyStore for NoopRoomPolicy {
    async fn room_members_only(
        &self,
        _room: &BareJid,
    ) -> Result<Option<bool>, NotificationOutboxError> {
        Ok(None)
    }
}

/// Typed recipient-level Do Not Disturb state, consulted at T1 push
/// dispatch.
///
/// `Inactive` means the recipient is NOT in DND; the evaluator
/// proceeds with the XEP-0492 / XEP-0191 / XEP-0513 / XEP-0334 gates.
/// `Active` means the evaluator MUST suppress the candidate with
/// [`SuppressedReason::WaddleDnd`]. The DND state is a recipient-state
/// read (not a message-frozen fact), so the consultation belongs at
/// T1 alongside XEP-0492 — the same race-window semantics apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DndState {
    Inactive,
    Active,
}

/// T1 lookup of the recipient's Waddle DnD state.
///
/// Production implementation lives in [`crate::dnd_reader::PepDndReader`],
/// which reads the durable [`crate::dnd_projection::DndProjectionStore`]
/// projection of the user's `urn:waddle:dnd:0` PEP item and resolves
/// the typed [`DndState`] via the pure evaluator in
/// [`waddle_xmpp::xep::xep_waddle_dnd`].
///
/// The T0 emit path keeps the [`NoopDndReader`] below — DND is a T1
/// recipient-state read and is intentionally not consulted at emit
/// time (see the stage check at the call site).
#[async_trait::async_trait]
pub trait DndReader: Send + Sync {
    async fn dnd_state(&self, user: &BareJid) -> Result<DndState, NotificationOutboxError>;
}

/// [`DndReader`] that reports every user as not-in-DND.
///
/// Used at the T0 emit call sites
/// ([`crate::server::routes::interpret::offline_delivery`],
/// [`crate::server::routes::interpret::groupchat_inbox`]) where the
/// evaluator's typed signature requires a reader but DND consultation
/// is skipped by the [`PushEvalStage::T1Drain`] guard. Production
/// T1 drain (`session_janitors`) uses [`crate::dnd_reader::PepDndReader`]
/// instead.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopDndReader;

#[async_trait::async_trait]
impl DndReader for NoopDndReader {
    async fn dnd_state(&self, _user: &BareJid) -> Result<DndState, NotificationOutboxError> {
        Ok(DndState::Inactive)
    }
}

/// Typed bundle of T1 recipient-state readers consulted by
/// [`NotificationOutboxStore::drain_pending_candidates_into_outbox`].
///
/// Bundling these reduces the drain method's argument count below
/// the clippy `too_many_arguments` threshold without losing
/// explicitness — each field is a trait object, so call sites pass
/// distinct concrete implementations rather than a single composite
/// dependency.
#[derive(Copy, Clone)]
pub struct NotificationDrainDeps<'a> {
    pub room_policy: &'a dyn RoomPolicyStore,
    pub dnd_reader: &'a dyn DndReader,
    pub activity_reader: &'a dyn NotificationActivityReader,
}

impl<'a> NotificationDrainDeps<'a> {
    pub fn new(
        room_policy: &'a dyn RoomPolicyStore,
        dnd_reader: &'a dyn DndReader,
        activity_reader: &'a dyn NotificationActivityReader,
    ) -> Self {
        Self {
            room_policy,
            dnd_reader,
            activity_reader,
        }
    }
}

/// Default TTL window for the XEP-0513 `<active/>` push filter (5
/// minutes). A recipient whose
/// [`crate::notification_activity::NotificationActivity::last_active_at_ms`]
/// is older than `now - ACTIVE_MENTION_TTL_MS` is treated as
/// "currently not active" and the T1 evaluator suppresses
/// [`NotificationClass::ActiveChannelMention`] candidates with
/// [`SuppressedReason::Xep0513ActiveMiss`].
pub const DEFAULT_ACTIVE_MENTION_TTL_SECONDS: u64 = 300;

/// Lower bound for the operator-tunable TTL (1 second). A value of 0
/// would suppress *every* `ActiveChannelMention` regardless of
/// activity — almost certainly an operator misconfiguration — so we
/// clamp to a minimum of one second.
pub const MIN_ACTIVE_MENTION_TTL_SECONDS: u64 = 1;

/// Upper bound for the operator-tunable TTL (24 hours). Anything
/// beyond this is effectively "disable the filter"; deployments that
/// want that should remove the `ActiveChannelMention` candidate at the
/// emission boundary rather than turning the gate into a no-op.
pub const MAX_ACTIVE_MENTION_TTL_SECONDS: u64 = 86_400;

const WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV: &str = "WADDLE_PUSH_ACTIVE_MENTION_TTL_SECONDS";

/// Reads `WADDLE_PUSH_ACTIVE_MENTION_TTL_SECONDS` and clamps to the
/// [`MIN_ACTIVE_MENTION_TTL_SECONDS`, `MAX_ACTIVE_MENTION_TTL_SECONDS`]
/// window. Unparseable or unset values fall back to
/// [`DEFAULT_ACTIVE_MENTION_TTL_SECONDS`].
pub fn active_mention_ttl_ms_from_env() -> i64 {
    let seconds = std::env::var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_ACTIVE_MENTION_TTL_SECONDS)
        .clamp(
            MIN_ACTIVE_MENTION_TTL_SECONDS,
            MAX_ACTIVE_MENTION_TTL_SECONDS,
        );
    i64::try_from(seconds.saturating_mul(1_000)).unwrap_or(i64::MAX)
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

fn notification_candidates_table_sql(i64_type: &str, if_not_exists: bool) -> String {
    let if_not_exists = if if_not_exists { "IF NOT EXISTS " } else { "" };
    format!(
        r#"
        CREATE TABLE {if_not_exists}notification_candidates (
            recipient_bare_jid TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            stanza_id_by TEXT NOT NULL,
            stanza_id TEXT NOT NULL,
            class TEXT NOT NULL CONSTRAINT {NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME} CHECK ({NOTIFICATION_CANDIDATES_CLASS_CHECK_SQL}),
            reason TEXT NOT NULL CONSTRAINT {NOTIFICATION_CANDIDATES_REASON_CHECK_NAME} CHECK ({NOTIFICATION_CANDIDATES_REASON_CHECK_SQL}),
            created_at_ms {i64_type} NOT NULL,
            policy_error_count INTEGER NOT NULL DEFAULT 0,
            next_attempt_at_ms {i64_type},
            outboxed_at_ms {i64_type},
            suppressed_reason TEXT CONSTRAINT {NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_NAME} CHECK ({NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_SQL}),
            noping INTEGER NOT NULL DEFAULT 0,
            no_store INTEGER NOT NULL DEFAULT 0,
            no_permanent_store INTEGER NOT NULL DEFAULT 0,
            last_message_body TEXT,
            PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
        )
        "#
    )
}

fn notification_outbox_table_sql(i64_type: &str, if_not_exists: bool) -> String {
    let if_not_exists = if if_not_exists { "IF NOT EXISTS " } else { "" };
    format!(
        r#"
        CREATE TABLE {if_not_exists}notification_outbox (
            job_id TEXT PRIMARY KEY,
            recipient_bare_jid TEXT NOT NULL,
            push_service_jid TEXT NOT NULL,
            node TEXT NOT NULL,
            conversation_jid TEXT NOT NULL,
            sender_jid TEXT NOT NULL,
            sender_jids TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            class TEXT NOT NULL CONSTRAINT {NOTIFICATION_OUTBOX_CLASS_CHECK_NAME} CHECK ({NOTIFICATION_OUTBOX_CLASS_CHECK_SQL}),
            message_count INTEGER NOT NULL,
            context_xml TEXT NOT NULL,
            rich_opt_in INTEGER NOT NULL DEFAULT 0 CHECK (rich_opt_in IN (0, 1)),
            summary_body TEXT,
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
    )
}

/// Returns `true` iff `definition` quotes `value` as a SQL string literal,
/// e.g. `'value'`.
///
/// Postgres' `pg_get_constraintdef` and SQLite's `sqlite_master.sql` both
/// render IN-list literals with single quotes around each enum value
/// (Postgres adds a `::character varying` cast but the leading `'value'`
/// token is the same). Matching against the quoted form prevents false
/// positives where one enum value is a substring of another — e.g.
/// `'dm_mention'` contains the substring `dm`, but the bare token `'dm'`
/// is absent from a CHECK list that only allows `dm_mention`.
fn constraint_definition_quotes_value(definition: &str, value: &str) -> bool {
    let mut needle = String::with_capacity(value.len() + 2);
    needle.push('\'');
    needle.push_str(value);
    needle.push('\'');
    definition.contains(&needle)
}

fn notification_candidates_reason_constraint_matches_expected(definition: &str) -> bool {
    let normalized = definition.to_ascii_lowercase();
    normalized.contains("reason")
        && NOTIFICATION_CANDIDATES_REASON_VALUES
            .iter()
            .all(|reason| constraint_definition_quotes_value(&normalized, reason))
}

fn notification_candidates_class_constraint_matches_expected(definition: &str) -> bool {
    let normalized = definition.to_ascii_lowercase();
    normalized.contains("class")
        && NOTIFICATION_CANDIDATES_CLASS_VALUES
            .iter()
            .all(|class| constraint_definition_quotes_value(&normalized, class))
}

fn notification_outbox_class_constraint_matches_expected(definition: &str) -> bool {
    let normalized = definition.to_ascii_lowercase();
    normalized.contains("class")
        && NOTIFICATION_OUTBOX_CLASS_VALUES
            .iter()
            .all(|class| constraint_definition_quotes_value(&normalized, class))
}

fn notification_candidates_suppressed_reason_constraint_matches_expected(definition: &str) -> bool {
    let normalized = definition.to_ascii_lowercase();
    normalized.contains("suppressed_reason")
        && NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES
            .iter()
            .all(|reason| constraint_definition_quotes_value(&normalized, reason))
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
        // Startup invariant: every closed-set typed `SuppressedReason`
        // db value MUST round-trip through `from_db_value`. This is a
        // typed sanity check that the enum, its db values, the schema
        // CHECK constraint, and the prometheus labels are in lockstep.
        // A mismatched build fails fast at process start rather than
        // surfacing as a confusing `CHECK violation` during the first
        // real suppression.
        for reason in SuppressedReason::ALL.iter().copied() {
            let db = reason.as_db_value();
            let decoded = SuppressedReason::from_db_value(db)?;
            if decoded != reason {
                return Err(NotificationOutboxError::InvalidSuppressedReason(format!(
                    "round-trip mismatch for {db}: decoded {decoded:?}",
                )));
            }
        }
        let i64_type = crate::db::i64_sql_type(self.db.driver());
        self.execute(&notification_candidates_table_sql(i64_type, true), ())
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
        // Reason/class CHECK migrations rebuild the table from a legacy
        // schema; they MUST run before the slice-2a columns are added
        // because the rebuild INSERT only copies the original column
        // set. Adding the slice-2a columns afterward then either creates
        // the column for-the-first-time (legacy upgrade) or is a no-op
        // (cold init, since `notification_candidates_table_sql` already
        // declares them).
        self.migrate_notification_candidates_reason_constraint(i64_type)
            .await?;
        self.migrate_notification_candidates_class_constraint(i64_type)
            .await?;
        self.add_column_if_missing("notification_candidates", "suppressed_reason TEXT")
            .await?;
        self.add_column_if_missing(
            "notification_candidates",
            "noping INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        self.add_column_if_missing(
            "notification_candidates",
            "no_store INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        self.add_column_if_missing(
            "notification_candidates",
            "no_permanent_store INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        self.migrate_notification_candidates_suppressed_reason_constraint(i64_type)
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
        self.execute(&notification_outbox_table_sql(i64_type, true), ())
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
        self.migrate_notification_outbox_class_constraint(i64_type)
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

    async fn migrate_notification_candidates_reason_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => {
                self.migrate_postgres_notification_candidates_reason_constraint()
                    .await
            }
            crate::db::DatabaseDriver::Sqlite => {
                self.migrate_sqlite_notification_candidates_reason_constraint(i64_type)
                    .await
            }
        }
    }

    async fn migrate_postgres_notification_candidates_reason_constraint(
        &self,
    ) -> Result<(), NotificationOutboxError> {
        self.migrate_postgres_check_constraint_on_column(
            "notification_candidates",
            "reason",
            NOTIFICATION_CANDIDATES_REASON_CHECK_NAME,
            NOTIFICATION_CANDIDATES_REASON_CHECK_SQL,
            notification_candidates_reason_constraint_matches_expected,
        )
        .await
    }

    async fn migrate_sqlite_notification_candidates_reason_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        if !self
            .sqlite_notification_candidates_reason_constraint_is_stale()
            .await?
        {
            return Ok(());
        }

        let mut tx = self.db.begin().await?;
        for index in NOTIFICATION_CANDIDATES_INDEXES {
            tx.execute(&format!("DROP INDEX IF EXISTS {index}"), ())
                .await?;
        }
        tx.execute(
            "ALTER TABLE notification_candidates RENAME TO notification_candidates_old_reason_check",
            (),
        )
        .await?;
        tx.execute(&notification_candidates_table_sql(i64_type, false), ())
            .await?;
        tx.execute(
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
            )
            SELECT
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
            FROM notification_candidates_old_reason_check
            "#,
            (),
        )
        .await?;
        tx.execute("DROP TABLE notification_candidates_old_reason_check", ())
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sqlite_notification_candidates_reason_constraint_is_stale(
        &self,
    ) -> Result<bool, NotificationOutboxError> {
        let mut rows = self
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notification_candidates'",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let create_sql: String = row.get(0)?;
        Ok(!notification_candidates_reason_constraint_matches_expected(
            &create_sql,
        ))
    }

    async fn migrate_notification_candidates_class_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => {
                self.migrate_postgres_notification_candidates_class_constraint()
                    .await
            }
            crate::db::DatabaseDriver::Sqlite => {
                self.migrate_sqlite_notification_candidates_class_constraint(i64_type)
                    .await
            }
        }
    }

    async fn migrate_postgres_notification_candidates_class_constraint(
        &self,
    ) -> Result<(), NotificationOutboxError> {
        self.migrate_postgres_check_constraint_on_column(
            "notification_candidates",
            "class",
            NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME,
            NOTIFICATION_CANDIDATES_CLASS_CHECK_SQL,
            notification_candidates_class_constraint_matches_expected,
        )
        .await
    }

    async fn migrate_sqlite_notification_candidates_class_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        if !self
            .sqlite_notification_candidates_class_constraint_is_stale()
            .await?
        {
            return Ok(());
        }

        let mut tx = self.db.begin().await?;
        for index in NOTIFICATION_CANDIDATES_INDEXES {
            tx.execute(&format!("DROP INDEX IF EXISTS {index}"), ())
                .await?;
        }
        tx.execute(
            "ALTER TABLE notification_candidates RENAME TO notification_candidates_old_class_check",
            (),
        )
        .await?;
        tx.execute(&notification_candidates_table_sql(i64_type, false), ())
            .await?;
        tx.execute(
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
            )
            SELECT
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
            FROM notification_candidates_old_class_check
            "#,
            (),
        )
        .await?;
        tx.execute("DROP TABLE notification_candidates_old_class_check", ())
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sqlite_notification_candidates_class_constraint_is_stale(
        &self,
    ) -> Result<bool, NotificationOutboxError> {
        let mut rows = self
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notification_candidates'",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let create_sql: String = row.get(0)?;
        Ok(!notification_candidates_class_constraint_matches_expected(
            &create_sql,
        ))
    }

    async fn migrate_notification_candidates_suppressed_reason_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => {
                self.migrate_postgres_notification_candidates_suppressed_reason_constraint()
                    .await
            }
            crate::db::DatabaseDriver::Sqlite => {
                self.migrate_sqlite_notification_candidates_suppressed_reason_constraint(i64_type)
                    .await
            }
        }
    }

    async fn migrate_postgres_notification_candidates_suppressed_reason_constraint(
        &self,
    ) -> Result<(), NotificationOutboxError> {
        self.migrate_postgres_check_constraint_on_column(
            "notification_candidates",
            "suppressed_reason",
            NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_NAME,
            NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_CHECK_SQL,
            notification_candidates_suppressed_reason_constraint_matches_expected,
        )
        .await
    }

    /// SQLite does not enforce CHECK constraints added after CREATE
    /// TABLE for existing rows, and adding a new CHECK requires a
    /// rebuild. Following the existing pattern, when the current
    /// schema text does not advertise the expected suppressed_reason
    /// CHECK we rebuild via rename-old → create-new → copy.
    async fn migrate_sqlite_notification_candidates_suppressed_reason_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        if !self
            .sqlite_notification_candidates_suppressed_reason_constraint_is_stale()
            .await?
        {
            return Ok(());
        }

        let mut tx = self.db.begin().await?;
        for index in NOTIFICATION_CANDIDATES_INDEXES {
            tx.execute(&format!("DROP INDEX IF EXISTS {index}"), ())
                .await?;
        }
        tx.execute(
            "ALTER TABLE notification_candidates RENAME TO notification_candidates_old_suppressed_reason_check",
            (),
        )
        .await?;
        tx.execute(&notification_candidates_table_sql(i64_type, false), ())
            .await?;
        tx.execute(
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
                outboxed_at_ms,
                suppressed_reason,
                noping,
                no_store,
                no_permanent_store
            )
            SELECT
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
                outboxed_at_ms,
                suppressed_reason,
                noping,
                no_store,
                no_permanent_store
            FROM notification_candidates_old_suppressed_reason_check
            "#,
            (),
        )
        .await?;
        tx.execute(
            "DROP TABLE notification_candidates_old_suppressed_reason_check",
            (),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sqlite_notification_candidates_suppressed_reason_constraint_is_stale(
        &self,
    ) -> Result<bool, NotificationOutboxError> {
        let mut rows = self
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notification_candidates'",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let create_sql: String = row.get(0)?;
        Ok(!notification_candidates_suppressed_reason_constraint_matches_expected(&create_sql))
    }

    async fn migrate_notification_outbox_class_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        match self.db.driver() {
            crate::db::DatabaseDriver::Postgres => {
                self.migrate_postgres_notification_outbox_class_constraint()
                    .await
            }
            crate::db::DatabaseDriver::Sqlite => {
                self.migrate_sqlite_notification_outbox_class_constraint(i64_type)
                    .await
            }
        }
    }

    async fn migrate_postgres_notification_outbox_class_constraint(
        &self,
    ) -> Result<(), NotificationOutboxError> {
        self.migrate_postgres_check_constraint_on_column(
            "notification_outbox",
            "class",
            NOTIFICATION_OUTBOX_CLASS_CHECK_NAME,
            NOTIFICATION_OUTBOX_CLASS_CHECK_SQL,
            notification_outbox_class_constraint_matches_expected,
        )
        .await
    }

    /// Drops every CHECK constraint on `table.column` whose definition
    /// does NOT match the expected value set, and ensures a single
    /// named CHECK constraint is in place.
    ///
    /// Old schemas (created before this PR via inline
    /// `CHECK (column IN (...))` literals in `CREATE TABLE`) carry
    /// **anonymous** CHECK constraints with autogenerated names like
    /// `notification_candidates_class_check1`. Dropping only the named
    /// constraint we own would leave those anonymous ones in place,
    /// rejecting any newly-added enum value indefinitely. Walking
    /// `pg_constraint` + `pg_attribute` and dropping every
    /// non-matching CHECK on the column closes that gap.
    async fn migrate_postgres_check_constraint_on_column(
        &self,
        table: &str,
        column: &str,
        expected_name: &str,
        expected_check_sql: &str,
        matches_expected: fn(&str) -> bool,
    ) -> Result<(), NotificationOutboxError> {
        let existing = self
            .postgres_check_constraints_on_column(table, column)
            .await?;
        let mut current_named_present = false;
        let mut to_drop: Vec<String> = Vec::new();
        for (conname, definition) in &existing {
            if conname == expected_name && matches_expected(definition) {
                current_named_present = true;
            } else {
                to_drop.push(conname.clone());
            }
        }
        if current_named_present && to_drop.is_empty() {
            return Ok(());
        }
        for conname in &to_drop {
            // Identifier-safe: conname comes from `pg_constraint` and
            // matches the Postgres identifier rules; we additionally
            // quote it to defend against unexpected characters.
            self.execute(
                &format!("ALTER TABLE {table} DROP CONSTRAINT IF EXISTS \"{conname}\""),
                (),
            )
            .await?;
        }
        if !current_named_present {
            self.execute(
                &format!(
                    "ALTER TABLE {table} ADD CONSTRAINT {expected_name} CHECK ({expected_check_sql})"
                ),
                (),
            )
            .await?;
        }
        Ok(())
    }

    /// Returns every CHECK constraint on `table` that references
    /// `column` exclusively, as `(conname, pg_get_constraintdef)`.
    ///
    /// The `conkey = ARRAY[<attnum>]::int2[]` filter narrows to
    /// single-column CHECKs against the target column — multi-column
    /// CHECKs covering other columns are deliberately out of scope
    /// since they encode different invariants.
    async fn postgres_check_constraints_on_column(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Vec<(String, String)>, NotificationOutboxError> {
        let mut rows = self
            .query(
                r#"
                SELECT c.conname,
                       pg_get_constraintdef(c.oid)
                FROM pg_constraint AS c
                JOIN pg_attribute AS a
                  ON a.attrelid = c.conrelid
                 AND a.attname = ?
                WHERE c.conrelid = (? :: regclass)
                  AND c.contype = 'c'
                  AND c.conkey = ARRAY[a.attnum]::int2[]
                "#,
                crate::db_params![column, table],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let conname: String = row.get(0)?;
            let definition: String = row.get(1)?;
            out.push((conname, definition));
        }
        Ok(out)
    }

    async fn migrate_sqlite_notification_outbox_class_constraint(
        &self,
        i64_type: &str,
    ) -> Result<(), NotificationOutboxError> {
        if !self
            .sqlite_notification_outbox_class_constraint_is_stale()
            .await?
        {
            return Ok(());
        }

        let mut tx = self.db.begin().await?;
        for index in NOTIFICATION_OUTBOX_INDEXES {
            tx.execute(&format!("DROP INDEX IF EXISTS {index}"), ())
                .await?;
        }
        tx.execute(
            "ALTER TABLE notification_outbox RENAME TO notification_outbox_old_class_check",
            (),
        )
        .await?;
        tx.execute(&notification_outbox_table_sql(i64_type, false), ())
            .await?;
        tx.execute(
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
            )
            SELECT
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
            FROM notification_outbox_old_class_check
            "#,
            (),
        )
        .await?;
        tx.execute("DROP TABLE notification_outbox_old_class_check", ())
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sqlite_notification_outbox_class_constraint_is_stale(
        &self,
    ) -> Result<bool, NotificationOutboxError> {
        let mut rows = self
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notification_outbox'",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let create_sql: String = row.get(0)?;
        Ok(!notification_outbox_class_constraint_matches_expected(
            &create_sql,
        ))
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
                    outboxed_at_ms,
                    suppressed_reason,
                    noping,
                    no_store,
                    no_permanent_store,
                    last_message_body
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?, ?, ?)
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
                    i64::from(candidate.noping),
                    i64::from(candidate.no_store),
                    i64::from(candidate.no_permanent_store),
                    candidate.last_message_body.clone(),
                ],
            )
            .await?;
        if inserted == 0 {
            // UNIQUE-constraint collision. `notification_candidates`
            // carries TWO intentional unique constraints, both of
            // which the `ON CONFLICT DO NOTHING` (no target)
            // suppresses:
            //
            // 1. The PRIMARY KEY on `(recipient_bare_jid,
            //    conversation_jid, thread_id, stanza_id_by,
            //    stanza_id, class)` — exact-identity dedup.
            // 2. The `idx_notification_candidates_identity` UNIQUE
            //    index on `(recipient_bare_jid, conversation_jid,
            //    thread_id, stanza_id, class)` — cross-archive
            //    dedup for the same logical stanza minted under
            //    different `by=` JIDs (XEP-0359).
            //
            // Both are intended Duplicate triggers, so the
            // counter increments on either path. If a third
            // unique constraint is ever added with different
            // dedup semantics, the SQL needs an explicit chained
            // `ON CONFLICT (cols) DO NOTHING` for each path
            // (Greptile review on PR #758).
            waddle_xmpp::prometheus::increment_push_candidate_coalesced();
            return Ok(NotificationCandidateInsertOutcome::Duplicate);
        }
        waddle_xmpp::prometheus::increment_push_candidate_created();
        Ok(NotificationCandidateInsertOutcome::Inserted)
    }

    pub async fn drain_pending_candidates_into_outbox(
        &self,
        push_store: &dyn PushSubscriptionStore,
        blocking_storage: &dyn BlockingStorage,
        settings_projection: &crate::notification_settings_projection::NotificationSettingsProjectionStore,
        deps: NotificationDrainDeps<'_>,
        first_party_service_jid: &BareJid,
        batch_size: usize,
    ) -> Result<usize, NotificationOutboxError> {
        let NotificationDrainDeps {
            room_policy,
            dnd_reader,
            activity_reader,
        } = deps;
        let candidates = self.pending_candidates(batch_size).await?;
        let mut target_cache =
            std::collections::BTreeMap::<BareJid, Vec<NotificationOutboxTarget>>::new();
        let mut room_policy_cache =
            std::collections::BTreeMap::<BareJid, RoomPolicyCacheEntry>::new();
        let mut dnd_cache = std::collections::BTreeMap::<BareJid, DndState>::new();
        let mut activity_cache =
            std::collections::BTreeMap::<(BareJid, BareJid), Option<NotificationActivity>>::new();
        let active_mention_ttl_ms = active_mention_ttl_ms_from_env();
        let mut processed = 0usize;
        for candidate in candidates {
            // Self-DM filtering happens at the `NotificationCandidate`
            // constructor (`SelfDirectedNotificationCandidate` typed
            // error). A self-directed candidate is structurally
            // invalid and is rejected before it can be persisted, so
            // the T1 drain loop never observes one. See
            // `NotificationCandidate::direct_message` for the typed
            // boundary.
            match xep0191_blocks_notification_candidate(&candidate, blocking_storage).await {
                Ok(true) => {
                    let now_ms = crate::time::now_ms();
                    let mut tx = self.db.begin().await?;
                    record_candidate_suppressed_reason_tx(
                        &mut tx,
                        &candidate,
                        SuppressedReason::Xep0191Blocked,
                    )
                    .await?;
                    let claimed = mark_candidate_outboxed_tx(&mut tx, &candidate, now_ms).await?;
                    tx.commit().await?;
                    if claimed > 0 {
                        waddle_xmpp::prometheus::increment_push_suppressed(
                            SuppressedReason::Xep0191Blocked.as_db_value(),
                        );
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
            // T1 push-gate re-evaluation — race-window guard,
            // defense-in-depth (XEP-0492 + XEP-0191 + XEP-0513 + XEP-0334 +
            // Waddle DnD).
            //
            // The same typed evaluator already ran at T0 (DM emission
            // in `offline_delivery.rs`, groupchat emission in
            // `groupchat_inbox.rs`) and a Suppressed outcome there
            // short-circuits the candidate insert entirely. Per the
            // compliance rule the common case is "no row in
            // `notification_candidates` for suppressed outcomes."
            //
            // This T1 invocation catches the race where recipient
            // state changed *between* the T0 emission and the T1
            // dispatch (e.g. the user flipped XEP-0492 to `<never/>`
            // mid-flight, or a groupchat config change toggled
            // members-only). If the projection has changed the drain
            // marks the candidate outboxed without enqueueing a job —
            // the row exists only briefly during the race window,
            // which is acceptable per the locked Q2 design (push
            // output is preserved).
            //
            // The class on the candidate is purely message-derived
            // from T0; combined with the recipient's effective
            // notification level (consulted fresh here against the
            // projection store) the typed reducer decides
            // publish-or-suppress. The room-policy lookup is cached
            // for the duration of this drain pass so a 100-member
            // groupchat does not produce 100 actor round-trips.
            let eval_deps = PushEvalDeps {
                settings_projection,
                room_policy,
                dnd_reader,
                activity_reader,
                active_mention_ttl_ms,
            };
            let mut eval_caches = PushEvalCaches {
                room_policy: &mut room_policy_cache,
                dnd: &mut dnd_cache,
                activity: &mut activity_cache,
            };
            let outcome = match evaluate_push_gate_at_dispatch(
                PushEvalStage::T1Drain,
                eval_deps,
                &candidate,
                &mut eval_caches,
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(error) => {
                    tracing::warn!(
                        recipient = %candidate.recipient_bare_jid(),
                        conversation = %candidate.conversation_jid(),
                        error = ?error,
                        "push gate evaluation failed at T1; deferring candidate"
                    );
                    self.defer_candidate_policy_error(&candidate).await?;
                    continue;
                }
            };
            let rich = match outcome {
                T1PushDispatchOutcome::Suppressed { reason } => {
                    tracing::info!(
                        recipient = %candidate.recipient_bare_jid(),
                        conversation = %candidate.conversation_jid(),
                        sender = %candidate.sender_jid(),
                        class = ?candidate.class(),
                        %reason,
                        "T1 push gate suppressed XEP-0357 notification candidate"
                    );
                    let now_ms = crate::time::now_ms();
                    let mut tx = self.db.begin().await?;
                    record_candidate_suppressed_reason_tx(&mut tx, &candidate, reason).await?;
                    let claimed = mark_candidate_outboxed_tx(&mut tx, &candidate, now_ms).await?;
                    tx.commit().await?;
                    if claimed > 0 {
                        waddle_xmpp::prometheus::increment_push_suppressed(reason.as_db_value());
                        processed += 1;
                    }
                    continue;
                }
                T1PushDispatchOutcome::DeferUnknownRoomPolicy => {
                    // Actionable diagnostics for `Err(_)` lookups already
                    // fired exactly once per (drain batch, room) in
                    // `resolve_cached_room_policy`. The per-candidate
                    // deferral is `debug!` here so the cache-miss warn
                    // stays the single source-of-truth signal for
                    // operators triaging room-policy lookup failures.
                    tracing::debug!(
                        recipient = %candidate.recipient_bare_jid(),
                        conversation = %candidate.conversation_jid(),
                        class = ?candidate.class(),
                        "MUC config unavailable at T1; deferring candidate (unknown room policy is not 'public')"
                    );
                    self.defer_candidate_policy_error(&candidate).await?;
                    continue;
                }
                T1PushDispatchOutcome::Deliver { rich } => rich,
            };
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
                enqueue_outbox_job_tx(&mut tx, &candidate, target, &context, &rich, now_ms).await?;
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
                       policy_error_count,
                       noping,
                       no_store,
                       no_permanent_store,
                       last_message_body
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

    /// Test/diagnostic helper: total count of `notification_candidates`
    /// rows, including ones already marked outboxed.
    ///
    /// Compliance regression tests use this to assert that a
    /// T0-suppressed XEP-0492 outcome persists *no* row at all
    /// (`count_all_candidates == 0`), distinct from the older
    /// "row exists, marked outboxed without a job" shape.
    pub async fn count_all_candidates(&self) -> Result<i64, NotificationOutboxError> {
        let mut rows = self
            .query("SELECT COUNT(*) FROM notification_candidates", ())
            .await?;
        // `COUNT(*)` is guaranteed to return exactly one row on every
        // SQL backend; an empty result here would mean a corrupted
        // driver. Default to 0 fail-loud-via-row-decode instead of
        // panicking.
        let Some(row) = rows.next().await? else {
            return Ok(0);
        };
        Ok(row.get::<i64>(0)?)
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
                       claim_token,
                       rich_opt_in,
                       summary_body
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
                       claim_token,
                       rich_opt_in,
                       summary_body
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
            let outcome = match self
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
                Ok(outcome) => outcome,
                Err(error) => {
                    self.retry_or_fail_outcome_for_claimed_job(&job, error.to_string())
                        .await?
                }
            };
            // #531 push-pipeline observability: bucket the typed
            // outcome into the parallel counter so a single drain
            // pass produces a histogram-like cardinality on the
            // metrics endpoint without per-job label explosion. The
            // Published / RetryScheduled / Failed arms are the
            // closed-set typed contract on
            // [`NotificationOutboxPublishOutcome`].
            match &outcome {
                NotificationOutboxPublishOutcome::Published { .. } => {
                    waddle_xmpp::prometheus::increment_push_outbox_published();
                }
                NotificationOutboxPublishOutcome::RetryScheduled { .. } => {
                    waddle_xmpp::prometheus::increment_push_outbox_retry_scheduled();
                }
                NotificationOutboxPublishOutcome::Failed { .. } => {
                    waddle_xmpp::prometheus::increment_push_outbox_dead_lettered();
                }
            }
            outcomes.push(outcome);
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

/// Typed outcome of `evaluate_push_gate_at_dispatch`.
///
/// Extends [`crate::notification_settings_projection::PushDispatchDecision`]
/// with a third state — `DeferUnknownRoomPolicy` — that surfaces the
/// "room actor not currently live" signal as a retry rather than
/// silently defaulting to public. Slice 1 has no durable T1
/// projection of MUC `members_only`; if the actor lookup returns
/// `Ok(None)` we cannot know whether the room is private (default
/// `Always` level → `NotifyAll` candidates SHOULD push) or public
/// (default `OnMention` level → `NotifyAll` candidates SHOULD NOT
/// push), and silently picking either would either drop legitimate
/// private-room pushes or fan out unwanted public-room pushes. Slice
/// 2 will replace the live actor lookup with a durable projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum T1PushDispatchOutcome {
    /// Push gate decided to fan out; enqueue the push job. `rich`
    /// carries the T1-resolved XEP-0357 §5.4 summary fields (minimal at
    /// T0Emit; resolved from the recipient's XEP-0492 opt-in and the
    /// candidate's XEP-0334 hints at T1Drain).
    Deliver { rich: RichSummary },
    /// Push gate decided to suppress; mark candidate outboxed without
    /// enqueueing a job. `reason` is the typed audit reason that
    /// caused suppression (XEP-0492 `<never/>` / `<on-mention/>` miss,
    /// XEP-0191 blocking, XEP-0513 `<noping/>`, or Waddle DnD).
    Suppressed { reason: SuppressedReason },
    /// MUC config could not be resolved (room actor unavailable or
    /// failed). Defer with policy-error backoff so the next drain
    /// pass can retry once the actor (or, slice 2, the durable
    /// projection) is available.
    DeferUnknownRoomPolicy,
}

/// Typed per-batch cache entry for the [`RoomPolicyStore`] lookup.
///
/// `Unknown` is deliberately distinct from `Public` — see
/// [`T1PushDispatchOutcome::DeferUnknownRoomPolicy`] for the
/// reasoning. Once a room resolves to `Unknown` for a given batch,
/// every candidate for that room in the same batch reuses that
/// outcome to avoid retrying the same failing actor 100×.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoomPolicyCacheEntry {
    Public,
    Private,
    /// MUC policy could not be resolved. Wrapped source distinguishes
    /// the expected/normal `Ok(None)` (room not currently live) case
    /// from the actionable `Err(_)` (actor transport / lookup failure)
    /// case so production debugging and alert triage can act on the
    /// distinction. Both still defer identically at the dispatch site
    /// (the typed `T1PushDispatchOutcome::DeferUnknownRoomPolicy`).
    Unknown(UnknownRoomPolicySource),
}

/// Why a [`RoomPolicyCacheEntry::Unknown`] was produced. Logged at
/// most once per (drain batch, room) thanks to the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnknownRoomPolicySource {
    /// `RoomPolicyStore::room_members_only` returned `Ok(None)` —
    /// the room actor is not currently live. Expected/normal on
    /// restart windows or for rooms with no recent activity.
    NotLive,
    /// `RoomPolicyStore::room_members_only` returned `Err(_)` —
    /// an actor transport failure or other lookup error. Actionable;
    /// surfaces the underlying error string at cache-miss time so
    /// operators can correlate without needing every per-candidate
    /// log line.
    LookupError,
}

/// Push-dispatch gate evaluator.
///
/// Single typed entry point that decides publish/suppress/defer for a
/// [`NotificationCandidate`]. The function name was previously
/// `evaluate_xep0492_at_dispatch`; the responsibility has since grown
/// to cover the full XEP/Waddle suppressor matrix consulted at push
/// dispatch — XEP-0492 (`<never/>` / `<on-mention/>`), XEP-0191
/// (blocklist), XEP-0513 (`<noping/>`), XEP-0334 (`<no-store/>` /
/// `<no-permanent-store/>`), and Waddle DnD — so the name now
/// reflects the actual gate, not just one of its inputs.
///
/// **Same typed evaluator called at two invocation moments**:
///
/// - **T0 (candidate emission gate, compliance)** — DM
///   ([`crate::server::routes::interpret::offline_delivery`]) and
///   groupchat
///   ([`crate::server::routes::interpret::groupchat_inbox`])
///   emission paths invoke this on a constructed-but-not-inserted
///   [`NotificationCandidate`] before persisting it. A `Suppressed`
///   outcome short-circuits emission entirely — no row is written.
///   This satisfies the compliance rule that suppressed candidates
///   leave no audit trail in `notification_candidates`.
/// - **T1 (drain re-evaluator, race-window guard)** — the same
///   function runs again inside
///   [`NotificationOutboxStore::drain_pending_candidates_into_outbox`]
///   against fresh recipient state. If the projection changed
///   between T0 and T1 (e.g. the user flipped XEP-0492 to
///   `<never/>` mid-flight), the drain marks the candidate outboxed
///   without enqueuing a job. The brief race window where a row
///   exists then gets retroactively suppressed is acceptable per
///   the locked Q2 design.
///
/// Derives `(level, is_mention)` from the recorded candidate class +
/// recipient state and feeds them into the shared pure reducer
/// [`crate::notification_settings_projection::PushDispatchDecision::evaluate`].
///
/// - DM classes encode the mention bit directly
///   ([`NotificationClass::DirectMessage`] vs
///   [`NotificationClass::DirectMessageMention`]). DM evaluation
///   never consults `room_policy` and may pass any [`RoomPolicyStore`]
///   (e.g. [`NoopRoomPolicy`]) at the T0 call site.
/// - Groupchat classes encode both mention scope and
///   live-occupant scope; the room is private/public per the
///   [`RoomPolicyStore`] lookup, cached per call through
///   `room_policy_cache` so a 100-member groupchat does not produce
///   100 actor round-trips at T1, and a single-message emission at
///   T0 trivially hits one entry. When the lookup yields
///   `Ok(None)`/`Err(_)`, the evaluator returns
///   [`T1PushDispatchOutcome::DeferUnknownRoomPolicy`] — slice 1 has
///   no durable T1 projection of MUC config yet, so an unknown
///   policy must defer rather than default-to-public.
///
/// Which leg of the push pipeline is invoking the evaluator.
///
/// The single typed function runs at two moments per #506 Q3 — T0
/// emission gate (compliance: no row for XEP-0492 suppressed) and T1
/// drain (race-window guard + durable audit). The two legs DO NOT
/// share the full suppressor set: message-frozen suppressors (XEP-0513
/// `<noping/>`, XEP-0334 `<no-store/>` / `<no-permanent-store/>`) and
/// Waddle DnD are deliberately skipped at T0 so the candidate row
/// persists with its hint bits and the typed `suppressed_reason`
/// audit fires at T1 — without this split, hinted candidates would
/// be silently filtered at T0 with no audit trail, contradicting the
/// [`NotificationMessageHints`] contract.
///
/// T0 still applies recipient-state suppressors that compliance
/// requires to leave no row at all (XEP-0492 `<never/>`/`<on-mention/>`
/// miss). Those persist their suppression intent via metric counters
/// at the T0 emission site; the row itself is the audit surface for
/// everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushEvalStage {
    /// Called synchronously from `enqueue_xep0357_notification_candidate_*`
    /// before `insert_candidate`. A `Suppressed` outcome here means
    /// the row will NOT be persisted — reserve this stage for
    /// compliance-required suppressors only.
    T0Emit,
    /// Called from `drain_pending_candidates_into_outbox` against an
    /// already-persisted candidate. `Suppressed` outcomes here are
    /// recorded as `suppressed_reason` on the row and counted via
    /// `increment_push_suppressed` — full audit + observability.
    T1Drain,
}

/// Typed bundle of recipient-state readers consulted by
/// [`evaluate_push_gate_at_dispatch`].
///
/// Both T0 emission sites and the T1 drain loop construct this once
/// per dispatch call. Bundling keeps the evaluator argument count
/// below the clippy `too_many_arguments` floor without resorting to
/// `#[allow]` — every field stays a typed trait object so the caller
/// supplies the production impl or a typed test double.
#[derive(Copy, Clone)]
pub(crate) struct PushEvalDeps<'a> {
    pub settings_projection:
        &'a crate::notification_settings_projection::NotificationSettingsProjectionStore,
    pub room_policy: &'a dyn RoomPolicyStore,
    pub dnd_reader: &'a dyn DndReader,
    pub activity_reader: &'a dyn NotificationActivityReader,
    /// Active-mention TTL window in milliseconds. The T1 evaluator
    /// suppresses [`NotificationClass::ActiveChannelMention`]
    /// candidates whose recipient's
    /// [`crate::notification_activity::NotificationActivity::last_active_at_ms`]
    /// is older than `now - active_mention_ttl_ms`.
    pub active_mention_ttl_ms: i64,
}

/// Mutable per-drain-pass caches threaded through
/// [`evaluate_push_gate_at_dispatch`]. Bundling keeps the argument
/// count down and gives callers a single allocation site for the
/// three caches.
pub(crate) struct PushEvalCaches<'a> {
    pub room_policy: &'a mut std::collections::BTreeMap<BareJid, RoomPolicyCacheEntry>,
    pub dnd: &'a mut std::collections::BTreeMap<BareJid, DndState>,
    pub activity:
        &'a mut std::collections::BTreeMap<(BareJid, BareJid), Option<NotificationActivity>>,
}

pub(crate) async fn evaluate_push_gate_at_dispatch(
    stage: PushEvalStage,
    deps: PushEvalDeps<'_>,
    candidate: &NotificationCandidate,
    caches: &mut PushEvalCaches<'_>,
) -> Result<T1PushDispatchOutcome, NotificationOutboxError> {
    let PushEvalDeps {
        settings_projection,
        room_policy,
        dnd_reader,
        activity_reader,
        active_mention_ttl_ms,
    } = deps;
    let room_policy_cache = &mut *caches.room_policy;
    let dnd_cache = &mut *caches.dnd;
    let activity_cache = &mut *caches.activity;
    // Message-frozen suppressor (`<noping/>`) runs ONLY at T1Drain so
    // the candidate row is persisted with its hint bits and the typed
    // `suppressed_reason` audit can fire. At T0Emit the row doesn't
    // exist yet — suppressing here would leave no audit trail,
    // defeating the whole purpose of snapshotting the hint bits onto
    // `NotificationCandidate` in the first place.
    //
    // XEP-0334 `<no-store/>`/`<no-permanent-store/>` are NOT push
    // suppressors. Per XEP-0334 §3 they scope to message *storage*
    // (archives, offline queues, logs), and §8 cautions that hints
    // MUST NOT be relied on for any particular purpose — a transient
    // push notification is not "storage". They instead strip the
    // `last-message-body` from the rich XEP-0357 summary (the body, not
    // the notification, is what would become a semi-permanent record at
    // the push gateway). That stripping is resolved alongside the
    // recipient's opt-in in `resolve_rich_summary` below; the minimal
    // push still fires.
    if stage == PushEvalStage::T1Drain && candidate.noping() {
        return Ok(T1PushDispatchOutcome::Suppressed {
            reason: SuppressedReason::Xep0513Noping,
        });
    }

    let (conversation_kind, is_mention) = match candidate.class() {
        NotificationClass::DirectMessage => (
            crate::notification_settings_projection::ConversationKind::Direct,
            false,
        ),
        NotificationClass::DirectMessageMention => (
            crate::notification_settings_projection::ConversationKind::Direct,
            true,
        ),
        NotificationClass::PersonalMention
        | NotificationClass::ChannelMention
        | NotificationClass::ActiveChannelMention => {
            match resolve_cached_room_policy(
                room_policy,
                candidate.conversation_jid(),
                room_policy_cache,
            )
            .await
            {
                RoomPolicyCacheEntry::Private => (
                    crate::notification_settings_projection::ConversationKind::PrivateGroup,
                    true,
                ),
                RoomPolicyCacheEntry::Public => (
                    crate::notification_settings_projection::ConversationKind::PublicGroup,
                    true,
                ),
                RoomPolicyCacheEntry::Unknown(_) => {
                    return Ok(T1PushDispatchOutcome::DeferUnknownRoomPolicy);
                }
            }
        }
        NotificationClass::NotifyAll => {
            match resolve_cached_room_policy(
                room_policy,
                candidate.conversation_jid(),
                room_policy_cache,
            )
            .await
            {
                RoomPolicyCacheEntry::Private => (
                    crate::notification_settings_projection::ConversationKind::PrivateGroup,
                    false,
                ),
                RoomPolicyCacheEntry::Public => (
                    crate::notification_settings_projection::ConversationKind::PublicGroup,
                    false,
                ),
                RoomPolicyCacheEntry::Unknown(_) => {
                    return Ok(T1PushDispatchOutcome::DeferUnknownRoomPolicy);
                }
            }
        }
    };
    // Waddle DnD is a recipient-state read, fresh-at-T1 alongside
    // XEP-0492. The per-batch cache keys on (user → state) so a
    // recipient with many candidates in the same drain pass only
    // reads DnD once.
    //
    // Skipped at T0Emit so a hinted candidate from a DnD'd recipient
    // still persists (T1 then records DnD or hint reason as
    // appropriate). DnD also moves with the recipient between T0
    // and T1; the T1 re-evaluation is the authoritative read.
    if stage == PushEvalStage::T1Drain {
        let dnd_state =
            resolve_cached_dnd_state(dnd_reader, candidate.recipient_bare_jid(), dnd_cache).await?;
        if matches!(dnd_state, DndState::Active) {
            return Ok(T1PushDispatchOutcome::Suppressed {
                reason: SuppressedReason::WaddleDnd,
            });
        }
    }
    // XEP-0513 `<active/>` filter — only `ActiveChannelMention`
    // class candidates consult the per-(recipient, conversation)
    // activity projection. Other classes (DM, personal/channel
    // mention, notify-all) are unaffected: the `<active/>` filter is
    // a class-specific gate.
    //
    // Skipped at T0Emit per the recipient-state / fresh-read T1
    // contract: current activity is a T1 read, and consulting it at
    // T0 would conflate "active now" with "active at message-frozen
    // time". The candidate row persists through T0 and the T1 drain
    // either delivers or records the typed `Xep0513ActiveMiss`
    // suppression — same audit trail shape as the other T1-only
    // suppressors.
    if stage == PushEvalStage::T1Drain
        && matches!(candidate.class(), NotificationClass::ActiveChannelMention)
    {
        let activity = resolve_cached_activity(
            activity_reader,
            candidate.recipient_bare_jid(),
            candidate.conversation_jid(),
            activity_cache,
        )
        .await?;
        let now_ms = crate::time::now_ms();
        let is_active = match activity {
            None => false,
            Some(activity) => {
                // `crate::time::now_ms` is `chrono::Utc::now()` — wall-clock,
                // not monotonic. A projection row written by a writer whose
                // clock is ahead of the evaluator's (NTP skew, replica
                // drift, an ingestion path that stamped a future time)
                // would otherwise produce a *negative* `age`, which the
                // `age <= TTL` predicate silently treats as "active" until
                // the wall clock catches up — quietly extending the
                // configured TTL window. Clamp the stored timestamp to
                // `now_ms` before subtracting so the predicate operates on
                // a non-negative `age`; a future-stamped row is treated as
                // "active at `now_ms`" and ages naturally from there.
                let last_active = activity.last_active_at_ms.min(now_ms);
                let age = now_ms.saturating_sub(last_active);
                age <= active_mention_ttl_ms
            }
        };
        if !is_active {
            return Ok(T1PushDispatchOutcome::Suppressed {
                reason: SuppressedReason::Xep0513ActiveMiss,
            });
        }
    }
    let level = settings_projection
        .effective_setting(
            candidate.recipient_bare_jid(),
            candidate.conversation_jid(),
            conversation_kind,
        )
        .await?;
    let decision =
        crate::notification_settings_projection::PushDispatchDecision::evaluate(level, is_mention);
    Ok(match decision {
        crate::notification_settings_projection::PushDispatchDecision::Deliver => {
            let rich = resolve_rich_summary(stage, settings_projection, candidate).await?;
            T1PushDispatchOutcome::Deliver { rich }
        }
        crate::notification_settings_projection::PushDispatchDecision::Suppressed { reason } => {
            T1PushDispatchOutcome::Suppressed {
                reason: suppressed_reason_for_level(reason),
            }
        }
    })
}

/// Resolve the XEP-0357 §5.4 rich summary fields for a delivering
/// candidate.
///
/// The rich summary is a T1 concern: the recipient's XEP-0492
/// `<advanced/>` opt-in is recipient state read fresh at drain, and the
/// minimal default (no rich fields) is correct at T0Emit, where the
/// candidate-persistence decision does not need it.
///
/// When the recipient has opted in:
/// - `last-message-sender` is the candidate's full sender JID — routing
///   metadata present in any delivery, preserved even when a hint
///   strips the body.
/// - `last-message-body` is included only when no XEP-0334
///   `<no-store/>`/`<no-permanent-store/>` hint applies. The hint always
///   wins over the opt-in: shipping the body to a third-party push
///   gateway is a semi-permanent store of the message. (The body is
///   already `None` on hinted candidates — it was never persisted at T0
///   — but the explicit check keeps the XEP-defined precedence visible
///   and testable at the T1 decision point.)
async fn resolve_rich_summary(
    stage: PushEvalStage,
    settings_projection: &crate::notification_settings_projection::NotificationSettingsProjectionStore,
    candidate: &NotificationCandidate,
) -> Result<RichSummary, NotificationOutboxError> {
    if stage != PushEvalStage::T1Drain {
        return Ok(RichSummary::minimal());
    }
    let opt_in = settings_projection
        .effective_rich_payload_opt_in(candidate.recipient_bare_jid(), candidate.conversation_jid())
        .await?;
    if !opt_in {
        return Ok(RichSummary::minimal());
    }
    let body = if candidate.no_store() || candidate.no_permanent_store() {
        None
    } else {
        candidate.last_message_body().map(str::to_owned)
    };
    Ok(RichSummary {
        sender: Some(candidate.sender_jid().clone()),
        body,
    })
}

/// Translates a XEP-0492 [`waddle_xmpp::xep::NotificationLevel`]
/// suppression outcome into the typed [`SuppressedReason`] audit
/// variant.
///
/// `<never/>` always maps to `Xep0492Never`. `<on-mention/>` maps to
/// `Xep0492OnMentionMiss` because the XEP-0492 evaluator only emits
/// the `Suppressed` outcome when `should_notify(is_mention)` is false
/// — and for `OnMention` that means `is_mention == false`. Called
/// only from the `Suppressed` arm of the upstream XEP-0492 reducer
/// (`PushDispatchDecision::evaluate`), which never yields
/// `Suppressed` for `<always/>` — so `Always` is unreachable here
/// and the typed contract makes the missing arm a compile-time
/// error if the reducer ever drifts.
fn suppressed_reason_for_level(level: waddle_xmpp::xep::NotificationLevel) -> SuppressedReason {
    match level {
        waddle_xmpp::xep::NotificationLevel::Never => SuppressedReason::Xep0492Never,
        waddle_xmpp::xep::NotificationLevel::OnMention => SuppressedReason::Xep0492OnMentionMiss,
        waddle_xmpp::xep::NotificationLevel::Always => unreachable!(
            "suppressed_reason_for_level called with NotificationLevel::Always; \
             the XEP-0492 reducer never yields Suppressed for <always/>"
        ),
    }
}

/// Looks up `(owner, conversation)` in the per-batch activity cache,
/// populating on miss. The cached `Option` distinguishes "no row in
/// the projection" (`None`) from "row present" (`Some(activity)`) so
/// the XEP-0513 evaluator can branch on the typed shape without
/// re-querying the database for repeats within the same drain pass.
async fn resolve_cached_activity(
    activity_reader: &dyn NotificationActivityReader,
    owner: &BareJid,
    conversation: &BareJid,
    cache: &mut std::collections::BTreeMap<(BareJid, BareJid), Option<NotificationActivity>>,
) -> Result<Option<NotificationActivity>, NotificationOutboxError> {
    let key = (owner.clone(), conversation.clone());
    if let Some(entry) = cache.get(&key) {
        return Ok(entry.clone());
    }
    let activity = activity_reader.read_activity(owner, conversation).await?;
    cache.insert(key, activity.clone());
    Ok(activity)
}

async fn resolve_cached_dnd_state(
    dnd_reader: &dyn DndReader,
    user: &BareJid,
    cache: &mut std::collections::BTreeMap<BareJid, DndState>,
) -> Result<DndState, NotificationOutboxError> {
    if let Some(state) = cache.get(user) {
        return Ok(*state);
    }
    let state = dnd_reader.dnd_state(user).await?;
    cache.insert(user.clone(), state);
    Ok(state)
}

/// Looks up `room` in the per-batch policy cache, populating on miss.
///
/// On miss the raw `room_members_only` result is handled explicitly:
///
/// - `Ok(Some(true/false))` → cache `Private`/`Public`.
/// - `Ok(None)` → cache `Unknown(NotLive)` — expected/normal.
/// - `Err(error)` → emit a `tracing::warn!` with the error string,
///   then cache `Unknown(LookupError)`. Because the result is cached,
///   the warn fires at most once per (drain batch, room) — every
///   subsequent candidate for the same room in this batch hits the
///   cache silently.
async fn resolve_cached_room_policy(
    room_policy: &dyn RoomPolicyStore,
    room: &BareJid,
    cache: &mut std::collections::BTreeMap<BareJid, RoomPolicyCacheEntry>,
) -> RoomPolicyCacheEntry {
    if let Some(entry) = cache.get(room) {
        return *entry;
    }
    let entry = match room_policy.room_members_only(room).await {
        Ok(Some(true)) => RoomPolicyCacheEntry::Private,
        Ok(Some(false)) => RoomPolicyCacheEntry::Public,
        Ok(None) => RoomPolicyCacheEntry::Unknown(UnknownRoomPolicySource::NotLive),
        Err(error) => {
            tracing::warn!(
                %room,
                %error,
                "RoomPolicyStore::room_members_only failed; deferring T1 candidates for this room in the current drain batch"
            );
            RoomPolicyCacheEntry::Unknown(UnknownRoomPolicySource::LookupError)
        }
    };
    cache.insert(room.clone(), entry);
    entry
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
    rich: &RichSummary,
    now_ms: i64,
) -> Result<(), NotificationOutboxError> {
    // The durable schema stores XML as TEXT; keep protocol context typed until this DB write edge.
    let context_xml = String::from(context);
    for _ in 0..8 {
        let inserted =
            insert_outbox_job_tx(tx, candidate, target, context_xml.as_str(), rich, now_ms).await?;
        if inserted > 0 {
            return Ok(());
        }
        match merge_outbox_job_tx(tx, candidate, target, context_xml.as_str(), rich, now_ms).await?
        {
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
    rich: &RichSummary,
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
                rich_opt_in,
                summary_body,
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
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, 0, 0, NULL, NULL, NULL, NULL, ?, ?, NULL)
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
                i64::from(rich.sender.is_some()),
                rich.body.clone(),
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
    rich: &RichSummary,
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
            rich_opt_in = ?,
            summary_body = ?,
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
                i64::from(rich.sender.is_some()),
                rich.body.clone(),
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

/// Records a typed [`SuppressedReason`] onto a not-yet-outboxed
/// candidate row inside an active transaction. Always called BEFORE
/// [`mark_candidate_outboxed_tx`] in the T1 suppression path so the
/// `suppressed_reason` column persists for the row's lifetime in the
/// outboxed-prune retention window.
async fn record_candidate_suppressed_reason_tx(
    tx: &mut crate::db::Transaction<'_>,
    candidate: &NotificationCandidate,
    reason: SuppressedReason,
) -> Result<u64, NotificationOutboxError> {
    Ok(tx
        .execute(
            r#"
            UPDATE notification_candidates
            SET suppressed_reason = ?
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
                reason.as_db_value(),
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
        noping: row.get::<i64>(9)? != 0,
        no_store: row.get::<i64>(10)? != 0,
        no_permanent_store: row.get::<i64>(11)? != 0,
        last_message_body: row.get::<Option<String>>(12)?,
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
    let rich_opt_in: i64 = row.get(15)?;
    let summary_body: Option<String> = row.get(16)?;
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
    // Reconstruct the T1-resolved rich summary. `last-message-sender` is
    // the routing sender JID, included iff the recipient opted in;
    // `last-message-body` is non-null only when the opt-in held and no
    // XEP-0334 storage hint stripped it.
    let rich_summary = RichSummary {
        sender: (rich_opt_in != 0).then(|| sender_jid.clone()),
        body: summary_body,
    };
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
        rich_summary,
        status: NotificationOutboxStatus::from_db_value(&row.get::<String>(11)?)?,
        attempt_count: row.get(12)?,
        policy_error_count: row.get(13)?,
        claim_token: row.get(14)?,
    })
}

fn build_waddle_context(candidate: &NotificationCandidate) -> Element {
    Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
        .attr(
            minidom::rxml::xml_ncname!("conversation").to_owned(),
            candidate.conversation_jid.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("thread").to_owned(),
            candidate.thread_id.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("class").to_owned(),
            candidate.class.as_db_value(),
        )
        .build()
}

/// Resolved XEP-0357 §5.4 rich summary fields, decided at T1.
///
/// The push decision evaluator resolves these from the recipient's
/// XEP-0492 `<advanced/>` rich-payload opt-in and the message-frozen
/// XEP-0334 storage hints (see [`evaluate_push_gate_at_dispatch`]):
///
/// - `sender` (`last-message-sender`) is set iff the recipient opted in;
///   it is a routing JID present in any delivery and is preserved even
///   when a storage hint strips the body.
/// - `body` (`last-message-body`) is set iff the recipient opted in AND
///   no XEP-0334 `<no-store/>`/`<no-permanent-store/>` hint applies —
///   shipping the body to a third-party push gateway is a semi-permanent
///   store, so the hint always wins over the opt-in.
///
/// The default (`None`/`None`) is the minimal summary: `message-count`
/// plus the Waddle routing context only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichSummary {
    pub sender: Option<Jid>,
    pub body: Option<String>,
}

impl RichSummary {
    /// The minimal default — no rich fields (opt-out).
    pub fn minimal() -> Self {
        Self::default()
    }
}

pub fn build_xep0357_notification_payload(
    message_count: u32,
    rich: &RichSummary,
    context: &Element,
) -> Element {
    Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH)
        .append(build_xep0357_summary_form(message_count, rich))
        .append(context.clone())
        .build()
}

fn build_xep0357_summary_form(message_count: u32, rich: &RichSummary) -> Element {
    // XEP-0357 §4 example shows `<x xmlns='jabber:x:data'>` with NO
    // `type` attribute — the form is a passively-encapsulated summary,
    // not the result of a search/query. XEP-0004 §3.2 reserves
    // `type='result'` for query-response contexts which doesn't apply
    // here; emitting it confused at least one client we tested
    // against. Match the §4 example literally.
    let mut builder = Element::builder("x", NS_DATA_FORMS)
        .append(xdata_hidden_field("FORM_TYPE", XEP0357_SUMMARY_FORM_TYPE))
        .append(xdata_field("message-count", &message_count.to_string()));
    // XEP-0357 §5.4 optional rich fields. Order matches the spec
    // example: sender before body.
    if let Some(sender) = &rich.sender {
        builder = builder.append(xdata_field("last-message-sender", &sender.to_string()));
    }
    if let Some(body) = &rich.body {
        builder = builder.append(xdata_field("last-message-body", body));
    }
    builder.build()
}

fn xdata_hidden_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(value)
                .build(),
        )
        .build()
}

fn xdata_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
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

    /// Process-global mutex used to serialize tests that mutate
    /// environment variables. Mirrors the `env_lock` pattern in
    /// `crate::server::tests`. `std::env::set_var` is process-global
    /// and `cargo test` runs tests on multiple threads by default, so
    /// any test that reads or writes an env var MUST hold this guard
    /// to avoid races with parallel tests.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static ENV_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        ENV_MUTEX
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

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
            false,
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

    #[test]
    fn candidate_snapshots_body_when_unhinted() {
        let candidate = candidate("archive-body")
            .with_last_message_body(Some("Wherefore art thou, Romeo?".to_string()));
        assert_eq!(
            candidate.last_message_body(),
            Some("Wherefore art thou, Romeo?")
        );
    }

    #[test]
    fn candidate_drops_body_when_storage_hint_present() {
        // XEP-0334 storage conformance: an off-the-record body is never
        // persisted onto the candidate row, even temporarily.
        let recipient = bare("alice@example.com");
        let sender_jid: Jid = "bob@example.com/res".parse().expect("jid");
        for hints in [
            NotificationMessageHints::none().with_xep0334(true, false),
            NotificationMessageHints::none().with_xep0334(false, true),
        ] {
            let candidate = NotificationCandidate::direct_message_with_hints(
                recipient.clone(),
                sender_jid.clone(),
                StanzaId::new("archive-hinted", Jid::from(recipient.clone())),
                false,
                hints,
            )
            .expect("candidate")
            .with_last_message_body(Some("secret".to_string()));
            assert_eq!(
                candidate.last_message_body(),
                None,
                "storage hint must drop the snapshotted body"
            );
        }
    }

    #[test]
    fn postgres_reason_constraint_match_accepts_current_definition() {
        let postgres_definition = "CHECK (((reason)::text = ANY ((ARRAY['offline_dm'::character varying, 'offline_dm_mention'::character varying, 'groupchat_personal_mention'::character varying, 'groupchat_channel_mention'::character varying, 'groupchat_active_channel_mention'::character varying, 'groupchat_notify_all'::character varying])::text[])))";
        assert!(notification_candidates_reason_constraint_matches_expected(
            postgres_definition
        ));
    }

    #[test]
    fn postgres_reason_constraint_match_rejects_legacy_definition() {
        let postgres_definition = "CHECK (((reason)::text = 'offline_dm'::character varying))";
        assert!(!notification_candidates_reason_constraint_matches_expected(
            postgres_definition
        ));
    }

    #[test]
    fn postgres_class_constraint_match_accepts_current_definition() {
        let postgres_definition = "CHECK (((class)::text = ANY ((ARRAY['dm'::character varying, 'dm_mention'::character varying, 'personal_mention'::character varying, 'channel_mention'::character varying, 'active_channel_mention'::character varying, 'notify_all'::character varying])::text[])))";
        assert!(notification_candidates_class_constraint_matches_expected(
            postgres_definition
        ));
        assert!(notification_outbox_class_constraint_matches_expected(
            postgres_definition
        ));
    }

    #[test]
    fn postgres_class_constraint_match_rejects_legacy_definition() {
        let postgres_definition = "CHECK (((class)::text = ANY ((ARRAY['dm'::character varying, 'personal_mention'::character varying])::text[])))";
        assert!(!notification_candidates_class_constraint_matches_expected(
            postgres_definition
        ));
    }

    /// Regression: a constraint definition that contains the substring
    /// `dm` only because the longer value `dm_mention` is present (i.e.
    /// `'dm'` is NOT a quoted literal in the IN-list) must be flagged
    /// stale. Earlier code used `definition.contains("dm")` which
    /// false-positively accepted such a constraint and skipped the
    /// migration — leaving a stale CHECK that rejects new
    /// `'dm'` inserts.
    #[test]
    fn postgres_class_constraint_match_rejects_substring_only_definition() {
        let postgres_definition = "CHECK (((class)::text = ANY ((ARRAY['dm_mention'::character varying, 'personal_mention'::character varying, 'channel_mention'::character varying, 'active_channel_mention'::character varying, 'notify_all'::character varying])::text[])))";
        assert!(
            !notification_candidates_class_constraint_matches_expected(postgres_definition),
            "stale constraint missing 'dm' must NOT be treated as current",
        );
        assert!(
            !notification_outbox_class_constraint_matches_expected(postgres_definition),
            "stale outbox constraint missing 'dm' must NOT be treated as current",
        );
    }

    /// Regression: a SQLite `CREATE TABLE` body that contains the
    /// substring `offline_dm` only because the longer value
    /// `offline_dm_mention` is present must be flagged stale for the
    /// reason CHECK migration.
    #[test]
    fn sqlite_reason_constraint_match_rejects_substring_only_definition() {
        let sqlite_create_sql = "CREATE TABLE notification_candidates (reason TEXT NOT NULL CHECK (reason IN ('offline_dm_mention', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all')))";
        assert!(
            !notification_candidates_reason_constraint_matches_expected(sqlite_create_sql),
            "stale reason constraint missing 'offline_dm' must NOT be treated as current",
        );
    }

    /// Regression for the substring-only defect class extended to the
    /// slice 2a `suppressed_reason` matcher. The `SuppressedReason`
    /// enum has overlapping value families — `provider_rejected` is a
    /// substring of `provider_token_expired`, and `xep0492_never` /
    /// `xep0492_on_mention_miss` share the `xep0492_` prefix. A naïve
    /// `definition.contains(value)` matcher would false-positively
    /// accept a CHECK definition that only allows the longer variant
    /// while claiming to cover the shorter, skipping the migration
    /// and leaving inserts of the missing variant to fail at runtime.
    /// The quoted-literal matcher introduced in slice 1
    /// (commit 3f2b2dcd) must catch this for `suppressed_reason` too.
    #[test]
    fn postgres_suppressed_reason_constraint_match_rejects_substring_only_definition() {
        // Stale Postgres-shape definition that lists ONLY the longer
        // overlapping variants (`provider_token_expired`,
        // `xep0492_on_mention_miss`, `xep0357_no_registration`,
        // `xep0357_registration_disabled`) — every shorter prefix
        // (`provider_rejected`, `xep0492_never`, `xep0357_self`, ...)
        // would substring-match falsely under a naïve `contains`.
        let postgres_definition = "CHECK ((((suppressed_reason)::text = ANY ((ARRAY['xep0492_on_mention_miss'::character varying, 'xep0357_no_registration'::character varying, 'xep0357_registration_disabled'::character varying, 'provider_token_expired'::character varying])::text[]))))";
        assert!(
            !notification_candidates_suppressed_reason_constraint_matches_expected(postgres_definition),
            "stale Postgres suppressed_reason constraint missing shorter overlapping values must NOT be treated as current",
        );
    }

    /// SQLite-shape parallel of the substring-only regression. A
    /// `CREATE TABLE` body that only quotes the longer overlapping
    /// `SuppressedReason` variants must be flagged stale.
    #[test]
    fn sqlite_suppressed_reason_constraint_match_rejects_substring_only_definition() {
        let sqlite_create_sql = "CREATE TABLE notification_candidates (suppressed_reason TEXT CHECK (suppressed_reason IS NULL OR suppressed_reason IN ('xep0492_on_mention_miss', 'xep0357_no_registration', 'xep0357_registration_disabled', 'provider_token_expired')))";
        assert!(
            !notification_candidates_suppressed_reason_constraint_matches_expected(sqlite_create_sql),
            "stale SQLite suppressed_reason constraint missing shorter overlapping values must NOT be treated as current",
        );
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

    /// Default activity-reader for tests that do not exercise the
    /// XEP-0513 `<active/>` filter. Returns `Ok(None)` for every
    /// lookup so the T1 evaluator treats every recipient as inactive
    /// — but the XEP-0513 gate is only consulted for the
    /// `ActiveChannelMention` class, so other class tests are
    /// unaffected.
    ///
    /// Static so a `&NoopActivityReader` borrow stays valid for the
    /// duration of every call site without needing a `let binding =`
    /// dance at each invocation.
    static NOOP_ACTIVITY_READER: crate::notification_activity::NoopActivityReader =
        crate::notification_activity::NoopActivityReader;
    fn noop_activity_reader() -> &'static crate::notification_activity::NoopActivityReader {
        &NOOP_ACTIVITY_READER
    }

    /// Convenience constructor for [`NotificationDrainDeps`] that
    /// wires the default no-op activity reader. Used by every slice
    /// 2a test whose recipient class is not
    /// `ActiveChannelMention` — those tests do not exercise the
    /// XEP-0513 `<active/>` filter, so a noop reader is the correct
    /// dependency.
    fn drain_deps_with_noop_activity<'a>(
        room_policy: &'a dyn RoomPolicyStore,
        dnd_reader: &'a dyn DndReader,
        activity_reader: &'a crate::notification_activity::NoopActivityReader,
    ) -> NotificationDrainDeps<'a> {
        NotificationDrainDeps::new(room_policy, dnd_reader, activity_reader)
    }

    /// Cache triple held by every direct-evaluator test call site.
    /// Extracted as a `type` alias to keep clippy's `type_complexity`
    /// lint quiet without leaking `#[allow]` into the codebase.
    type FreshEvalCaches = (
        std::collections::BTreeMap<BareJid, RoomPolicyCacheEntry>,
        std::collections::BTreeMap<BareJid, DndState>,
        std::collections::BTreeMap<(BareJid, BareJid), Option<NotificationActivity>>,
    );

    /// Build a fresh [`PushEvalDeps`] / [`PushEvalCaches`] pair for
    /// unit-testing the typed evaluator function directly. Returns
    /// owned caches so each call site can hold them and pass `&mut`.
    fn fresh_eval_caches() -> FreshEvalCaches {
        (
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
        )
    }

    fn eval_deps_for_test<'a>(
        settings_projection:
            &'a crate::notification_settings_projection::NotificationSettingsProjectionStore,
        room_policy: &'a dyn RoomPolicyStore,
        dnd_reader: &'a dyn DndReader,
        activity_reader: &'a dyn NotificationActivityReader,
    ) -> PushEvalDeps<'a> {
        PushEvalDeps {
            settings_projection,
            room_policy,
            dnd_reader,
            activity_reader,
            active_mention_ttl_ms: 5 * 60 * 1_000,
        }
    }

    /// Test double for [`RoomPolicyStore`] that pretends every room is
    /// public (`members_only = false`). Slice 1's tests do not exercise
    /// private-room dispatch policy; when slice 2 adds those paths it
    /// will grow this stub (or replace it with a richer fixture).
    struct StubRoomPolicy;

    impl StubRoomPolicy {
        fn new() -> Self {
            Self
        }
    }

    #[async_trait::async_trait]
    impl RoomPolicyStore for StubRoomPolicy {
        async fn room_members_only(
            &self,
            _room: &BareJid,
        ) -> Result<Option<bool>, NotificationOutboxError> {
            Ok(Some(false))
        }
    }

    /// Test stub that returns `Ok(None)` — the "room not currently
    /// live" signal that the T1 evaluator must treat as `Unknown`
    /// (defer) rather than `Public` (default-OnMention). The counter
    /// proves the per-batch cache short-circuits repeat lookups.
    struct UnknownRoomPolicy {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl UnknownRoomPolicy {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl RoomPolicyStore for UnknownRoomPolicy {
        async fn room_members_only(
            &self,
            _room: &BareJid,
        ) -> Result<Option<bool>, NotificationOutboxError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(None)
        }
    }

    /// Test stub that returns `Ok(Some(false))` (public) but counts
    /// the calls so the per-batch cache can be asserted as effective.
    struct CountingPublicRoomPolicy {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl CountingPublicRoomPolicy {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl RoomPolicyStore for CountingPublicRoomPolicy {
        async fn room_members_only(
            &self,
            _room: &BareJid,
        ) -> Result<Option<bool>, NotificationOutboxError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(false))
        }
    }

    /// Test stub that always returns a typed `RoomPolicyLookup` error
    /// — models actor mailbox / transport failures. Counts the calls
    /// so we can assert the per-batch cache short-circuits subsequent
    /// lookups (one error → one cache entry → one warn → many silent
    /// reuses, never re-asking the failing dependency).
    struct ErroringRoomPolicy {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ErroringRoomPolicy {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl RoomPolicyStore for ErroringRoomPolicy {
        async fn room_members_only(
            &self,
            room: &BareJid,
        ) -> Result<Option<bool>, NotificationOutboxError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(NotificationOutboxError::RoomPolicyLookup {
                room: room.clone(),
                message: "test-fixture: simulated actor mailbox failure".to_string(),
            })
        }
    }

    async fn settings_projection(
    ) -> crate::notification_settings_projection::NotificationSettingsProjectionStore {
        let storage = crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("settings pubsub storage");
        crate::notification_settings_projection::NotificationSettingsProjectionStore::new(
            storage.database(),
        )
    }

    /// `DndReader` test double that mirrors the shape #367 will land
    /// for the real `urn:waddle:dnd:0` PEP-backed reader: a per-user
    /// persisted set of "currently DnD-active" recipients, queried
    /// fresh at T1 and returning [`DndState::Active`] iff the user's
    /// PEP item is present.
    ///
    /// When #367 lands, only the implementation swaps — the trait
    /// contract this mock exercises (per-user lookup → typed `DndState`,
    /// async + `BareJid`-keyed) is the load-bearing surface and is
    /// locked in slice 2a. Tests using this mock therefore verify the
    /// integration contract independently of #367's persistence layer.
    struct MockPepDndReader {
        active_users: std::sync::Mutex<std::collections::BTreeSet<BareJid>>,
    }
    impl MockPepDndReader {
        fn new() -> Self {
            Self {
                active_users: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            }
        }
        fn set_active(&self, user: BareJid) {
            self.active_users
                .lock()
                .expect("active_users lock")
                .insert(user);
        }
    }
    #[async_trait::async_trait]
    impl DndReader for MockPepDndReader {
        async fn dnd_state(&self, user: &BareJid) -> Result<DndState, NotificationOutboxError> {
            let active = self
                .active_users
                .lock()
                .expect("active_users lock")
                .contains(user);
            Ok(if active {
                DndState::Active
            } else {
                DndState::Inactive
            })
        }
    }

    /// Witness fixture for upstream-storage preservation: an
    /// [`InMemoryInboxStorage`] entry that the test seeds BEFORE the
    /// candidate emission / T1 drain runs, captured as a snapshot.
    /// The notification outbox layer only ever writes to
    /// `notification_candidates` and `notification_outbox`; the
    /// upstream XEP-0430 inbox (and by symmetry XEP-0313 MAM /
    /// XEP-0160 pending delivery / RFC 6121 routing) MUST be untouched
    /// when push is suppressed at T0 or T1.
    ///
    /// This helper seeds one inbox entry and returns both the storage
    /// handle and the entry-as-snapshot so the test can assert the row
    /// is identical (same `last_stanza_id`, `unread`, `last_updated`)
    /// after the candidate-emission code path runs.
    async fn seed_inbox_witness(
        recipient: &BareJid,
        partner: &BareJid,
        stanza_id: &str,
        last_updated: i64,
        unread: u32,
    ) -> (
        waddle_xmpp::inbox::storage::InMemoryInboxStorage,
        waddle_xmpp::inbox::InboxEntry,
    ) {
        let storage = waddle_xmpp::inbox::storage::InMemoryInboxStorage::new();
        // Bring the unread count up to `unread` via repeated
        // `increment_unread=true` upserts so the stored row matches
        // what a real XEP-0430 projection would persist (the in-memory
        // adapter ignores `with_unread` on first insert and instead
        // sets unread = 1 when increment is true).
        use waddle_xmpp::inbox::storage::InboxStorage;
        let entry_template = waddle_xmpp::inbox::InboxEntry::new(
            partner.clone(),
            waddle_xmpp::inbox::ConversationKind::Direct,
            stanza_id.to_string(),
            last_updated,
        );
        let mut last = None;
        for _ in 0..unread {
            last = Some(
                storage
                    .upsert(recipient, entry_template.clone(), true)
                    .await
                    .expect("seed inbox witness increment"),
            );
        }
        let witness = match last {
            Some(entry) => entry,
            None => storage
                .upsert(recipient, entry_template.clone(), false)
                .await
                .expect("seed inbox witness (no unread)"),
        };
        (storage, witness)
    }

    /// Assert the inbox witness seeded by [`seed_inbox_witness`] is
    /// still present and byte-identical — proves no rollback / no
    /// cross-table write happened during the candidate emission /
    /// drain. Implicit corollary: any other upstream artifact
    /// (XEP-0313 MAM row, XEP-0160 pending_delivery row, RFC 6121
    /// online-resource routing effect) that the test SETS UP BEFORE
    /// the candidate emission is preserved by symmetry — the outbox
    /// layer touches only its own two tables.
    async fn assert_inbox_witness_unchanged(
        storage: &waddle_xmpp::inbox::storage::InMemoryInboxStorage,
        recipient: &BareJid,
        expected: &waddle_xmpp::inbox::InboxEntry,
    ) {
        use waddle_xmpp::inbox::storage::InboxStorage;
        let entries = storage.list(recipient).await.expect("list inbox witness");
        assert_eq!(
            entries.len(),
            1,
            "inbox witness must have exactly one entry after suppression; got {entries:?}",
        );
        assert_eq!(
            &entries[0], expected,
            "suppression code path must not mutate upstream inbox row",
        );
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
    async fn store_initialization_migrates_legacy_candidate_reason_check() {
        let db = Database::in_memory("notification-outbox-legacy-candidate-reason")
            .await
            .unwrap();
        let conn = db.guard().await.expect("db guard");
        conn.execute(
            r#"
            CREATE TABLE notification_candidates (
                recipient_bare_jid TEXT NOT NULL,
                conversation_jid TEXT NOT NULL,
                sender_jid TEXT NOT NULL,
                thread_id TEXT NOT NULL DEFAULT '',
                stanza_id_by TEXT NOT NULL,
                stanza_id TEXT NOT NULL,
                class TEXT NOT NULL CHECK (class IN ('dm', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
                reason TEXT NOT NULL CHECK (reason IN ('offline_dm')),
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
        .expect("create legacy candidate table");
        conn.execute(
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
            ) VALUES (
                'bob@example.com',
                'alice@example.com',
                'alice@example.com/web',
                '',
                'bob@example.com',
                'legacy-direct',
                'dm',
                'offline_dm',
                1,
                0,
                NULL,
                NULL
            )
            "#,
            (),
        )
        .await
        .expect("insert legacy direct candidate");
        drop(conn);

        let store = NotificationOutboxStore::new(db)
            .await
            .expect("store initializes and migrates legacy reason check");
        let recipient = bare("charlie@example.com");
        let room = bare("legacy-reason@muc.example.com");
        let groupchat = groupchat_candidate_for(
            &recipient,
            &room,
            "legacy-reason@muc.example.com/alice"
                .parse()
                .expect("room sender jid"),
            "legacy-group",
            NotificationClass::ChannelMention,
        );

        assert_eq!(
            store
                .insert_candidate(&groupchat)
                .await
                .expect("insert groupchat"),
            NotificationCandidateInsertOutcome::Inserted
        );
        let mut rows = store
            .query(
                "SELECT reason FROM notification_candidates ORDER BY stanza_id",
                (),
            )
            .await
            .expect("query migrated candidates");
        let first = rows
            .next()
            .await
            .expect("first row query")
            .expect("legacy row");
        let second = rows
            .next()
            .await
            .expect("second row query")
            .expect("group row");
        assert_eq!(first.get::<String>(0).expect("legacy reason"), "offline_dm");
        assert_eq!(
            second.get::<String>(0).expect("group reason"),
            "groupchat_channel_mention"
        );
    }

    /// Regression for slice 1 of #526: a database created before the
    /// `dm_mention` class variant existed still has the legacy class
    /// CHECK constraint (`dm`/`personal_mention`/`channel_mention`/
    /// `active_channel_mention`/`notify_all`). After
    /// `NotificationOutboxStore::new` runs the class-constraint
    /// migration, the new `dm_mention` variant must be insertable.
    #[tokio::test]
    async fn store_initialization_migrates_legacy_candidate_class_check() {
        let db = Database::in_memory("notification-outbox-legacy-candidate-class")
            .await
            .unwrap();
        let conn = db.guard().await.expect("db guard");
        conn.execute(
            r#"
            CREATE TABLE notification_candidates (
                recipient_bare_jid TEXT NOT NULL,
                conversation_jid TEXT NOT NULL,
                sender_jid TEXT NOT NULL,
                thread_id TEXT NOT NULL DEFAULT '',
                stanza_id_by TEXT NOT NULL,
                stanza_id TEXT NOT NULL,
                class TEXT NOT NULL CHECK (class IN ('dm', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
                reason TEXT NOT NULL CHECK (reason IN ('offline_dm', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all')),
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
        .expect("create legacy candidate table");
        conn.execute(
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
            ) VALUES (
                'bob@example.com',
                'alice@example.com',
                'alice@example.com/web',
                '',
                'bob@example.com',
                'legacy-direct',
                'dm',
                'offline_dm',
                1,
                0,
                NULL,
                NULL
            )
            "#,
            (),
        )
        .await
        .expect("insert legacy direct candidate");
        drop(conn);

        let store = NotificationOutboxStore::new(db)
            .await
            .expect("store initializes and migrates legacy class check");
        let recipient = bare("bob@example.com");
        let sender_bare = bare("alice@example.com");
        let mention_candidate = NotificationCandidate::direct_message(
            recipient.clone(),
            "alice@example.com/web".parse().expect("full sender jid"),
            StanzaId::new("post-migration-mention", Jid::from(recipient.clone())),
            true,
        )
        .expect("dm_mention candidate after migration");
        assert_eq!(
            mention_candidate.class(),
            NotificationClass::DirectMessageMention
        );
        assert_eq!(
            store
                .insert_candidate(&mention_candidate)
                .await
                .expect("insert dm_mention candidate post-migration"),
            NotificationCandidateInsertOutcome::Inserted
        );
        let mut rows = store
            .query(
                "SELECT class FROM notification_candidates ORDER BY stanza_id",
                (),
            )
            .await
            .expect("query migrated candidates");
        let first = rows
            .next()
            .await
            .expect("first row query")
            .expect("legacy row");
        let second = rows
            .next()
            .await
            .expect("second row query")
            .expect("dm_mention row");
        assert_eq!(first.get::<String>(0).expect("legacy class"), "dm");
        assert_eq!(
            second.get::<String>(0).expect("dm_mention class"),
            "dm_mention"
        );
        // Touch unused fields to keep them documented as required
        // identity inputs for the candidate row.
        let _ = sender_bare;
    }

    /// Round-trip regression for the new `DirectMessageMention` class /
    /// `OfflineDirectMessageMention` reason variants introduced in
    /// slice 1 of #526. Inserts a DM candidate with `is_mention=true`
    /// and asserts the persisted row carries the typed `dm_mention`
    /// class and `offline_dm_mention` reason values.
    #[tokio::test]
    async fn direct_message_mention_class_round_trips_through_storage() {
        let db = Database::in_memory("notification-outbox-dm-mention-roundtrip")
            .await
            .unwrap();
        let store = NotificationOutboxStore::new(db).await.expect("store init");
        let recipient = bare("bob@example.com");
        let plain = NotificationCandidate::direct_message(
            recipient.clone(),
            "alice@example.com/web".parse().expect("full sender jid"),
            StanzaId::new("plain-dm", Jid::from(recipient.clone())),
            false,
        )
        .expect("plain dm candidate");
        let mention = NotificationCandidate::direct_message(
            recipient.clone(),
            "alice@example.com/web".parse().expect("full sender jid"),
            StanzaId::new("mention-dm", Jid::from(recipient.clone())),
            true,
        )
        .expect("dm_mention candidate");
        assert_eq!(plain.class(), NotificationClass::DirectMessage);
        assert_eq!(plain.reason(), NotificationReason::OfflineDirectMessage);
        assert_eq!(mention.class(), NotificationClass::DirectMessageMention);
        assert_eq!(
            mention.reason(),
            NotificationReason::OfflineDirectMessageMention
        );
        assert_eq!(
            store.insert_candidate(&plain).await.expect("insert plain"),
            NotificationCandidateInsertOutcome::Inserted
        );
        assert_eq!(
            store
                .insert_candidate(&mention)
                .await
                .expect("insert mention"),
            NotificationCandidateInsertOutcome::Inserted
        );
        let mut rows = store
            .query(
                "SELECT class, reason FROM notification_candidates ORDER BY stanza_id",
                (),
            )
            .await
            .expect("query round-trip candidates");
        let mention_row = rows
            .next()
            .await
            .expect("first row query")
            .expect("mention row");
        let plain_row = rows
            .next()
            .await
            .expect("second row query")
            .expect("plain row");
        assert_eq!(
            mention_row.get::<String>(0).expect("mention class"),
            "dm_mention"
        );
        assert_eq!(
            mention_row.get::<String>(1).expect("mention reason"),
            "offline_dm_mention"
        );
        assert_eq!(plain_row.get::<String>(0).expect("plain class"), "dm");
        assert_eq!(
            plain_row.get::<String>(1).expect("plain reason"),
            "offline_dm"
        );
    }

    /// Postgres-only regression for the anonymous CHECK constraint bug:
    /// schemas created before this PR via inline
    /// `CHECK (class IN (...))` literals in `CREATE TABLE` end up with
    /// **anonymous** CHECK constraints whose name is autogenerated
    /// (e.g. `notification_candidates_class_check1`). The migration
    /// must walk `pg_constraint` and drop every CHECK on the target
    /// column — not just the named one we own — otherwise the
    /// anonymous CHECK keeps rejecting newly-added enum values
    /// (`dm_mention` here) on upgraded deployments.
    ///
    /// Opt-in via `WADDLE_TEST_POSTGRES_URL` since the project's
    /// default test backend is SQLite (which uses a different
    /// CREATE-TABLE-rebuild migration path).
    #[tokio::test]
    async fn store_initialization_drops_anonymous_postgres_class_check_constraint() {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!(
                "skipping: WADDLE_TEST_POSTGRES_URL not set \
                 (postgres-only regression for anonymous CHECK drop)"
            );
            return;
        };

        // Use a UUID-suffixed table name so concurrent runs against
        // the same Postgres do not clobber each other.
        let table_suffix = uuid::Uuid::new_v4().simple().to_string();
        let table = format!("notification_candidates_{table_suffix}");
        let scoped_url = database_url;
        let db = Database::from_config(
            "notification-outbox-anonymous-pg-check",
            &crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, scoped_url),
        )
        .await
        .expect("connect postgres");
        let conn = db.guard().await.expect("db guard");

        // Anonymous CHECK: no `CONSTRAINT name` clause means Postgres
        // generates one (e.g. `<table>_class_check`). The legacy class
        // set deliberately excludes `'dm_mention'` to mirror the
        // pre-#526 schema.
        let create_sql = format!(
            r#"
            CREATE TABLE "{table}" (
                recipient_bare_jid TEXT NOT NULL,
                conversation_jid TEXT NOT NULL,
                sender_jid TEXT NOT NULL,
                thread_id TEXT NOT NULL DEFAULT '',
                stanza_id_by TEXT NOT NULL,
                stanza_id TEXT NOT NULL,
                class TEXT NOT NULL CHECK (class IN ('dm', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
                reason TEXT NOT NULL CHECK (reason IN ('offline_dm', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all')),
                created_at_ms BIGINT NOT NULL,
                policy_error_count INTEGER NOT NULL DEFAULT 0,
                next_attempt_at_ms BIGINT,
                outboxed_at_ms BIGINT,
                PRIMARY KEY (recipient_bare_jid, conversation_jid, thread_id, stanza_id_by, stanza_id, class)
            )
            "#
        );
        conn.execute(&create_sql, ())
            .await
            .expect("create scoped table");

        // Cleanup on test exit: scope-guard pattern via a closure
        // that captures `&db` (we can't use `Drop` because it would
        // require async cleanup).
        let cleanup_table = table.clone();
        let cleanup_db = db.clone();
        let cleanup = async move {
            let conn = cleanup_db.guard().await.expect("cleanup db guard");
            let _ = conn
                .execute(&format!(r#"DROP TABLE IF EXISTS "{cleanup_table}""#), ())
                .await;
        };

        // Sanity check: pre-migration, the anonymous CHECK exists
        // and is named (any non-empty `conname` qualifies — Postgres
        // never emits truly nameless constraints, but autogenerated
        // names are still NOT ours).
        let mut rows = conn
            .query(
                r#"
                SELECT c.conname
                FROM pg_constraint AS c
                JOIN pg_attribute AS a
                  ON a.attrelid = c.conrelid
                 AND a.attname = 'class'
                WHERE c.conrelid = ($1 :: regclass)
                  AND c.contype = 'c'
                  AND c.conkey = ARRAY[a.attnum]::int2[]
                "#,
                crate::db_params![table.as_str()],
            )
            .await
            .expect("pre-migration check constraint query");
        let mut found_anonymous = false;
        while let Some(row) = rows.next().await.expect("row") {
            let conname: String = row.get(0).expect("conname");
            if conname != NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME {
                found_anonymous = true;
            }
        }
        assert!(
            found_anonymous,
            "test fixture must produce an anonymous (non-canonical-name) CHECK on the class column"
        );
        drop(conn);
        drop(rows);

        // Drive the migration helpers directly against our scoped
        // table. This sidesteps the full `NotificationOutboxStore::new`
        // initialization (which targets the hard-coded
        // `notification_candidates` table name). We migrate BOTH the
        // class and reason anonymous CHECK constraints because the
        // post-migration insert below carries `dm_mention` (new class
        // value) AND `offline_dm_mention` (new reason value) — Postgres
        // enforces every constraint on the row, so leaving either
        // anonymous CHECK in place will reject the insert.
        let store = NotificationOutboxStore { db: db.clone() };
        let class_migrate = store
            .migrate_postgres_check_constraint_on_column(
                &table,
                "class",
                NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME,
                NOTIFICATION_CANDIDATES_CLASS_CHECK_SQL,
                notification_candidates_class_constraint_matches_expected,
            )
            .await;
        if let Err(error) = &class_migrate {
            cleanup.await;
            panic!("class migration failed: {error}");
        }
        let reason_migrate = store
            .migrate_postgres_check_constraint_on_column(
                &table,
                "reason",
                NOTIFICATION_CANDIDATES_REASON_CHECK_NAME,
                NOTIFICATION_CANDIDATES_REASON_CHECK_SQL,
                notification_candidates_reason_constraint_matches_expected,
            )
            .await;
        if let Err(error) = &reason_migrate {
            cleanup.await;
            panic!("reason migration failed: {error}");
        }

        // Post-migration: only the canonical named CHECK should
        // remain on the class column, and it should accept the new
        // `dm_mention` value.
        let conn = db.guard().await.expect("db guard");
        let mut rows = conn
            .query(
                r#"
                SELECT c.conname
                FROM pg_constraint AS c
                JOIN pg_attribute AS a
                  ON a.attrelid = c.conrelid
                 AND a.attname = 'class'
                WHERE c.conrelid = ($1 :: regclass)
                  AND c.contype = 'c'
                  AND c.conkey = ARRAY[a.attnum]::int2[]
                "#,
                crate::db_params![table.as_str()],
            )
            .await
            .expect("post-migration check constraint query");
        let mut remaining: Vec<String> = Vec::new();
        while let Some(row) = rows.next().await.expect("row") {
            remaining.push(row.get(0).expect("conname"));
        }
        let canonical_present = remaining
            .iter()
            .any(|n| n == NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME);
        let anonymous_present = remaining
            .iter()
            .any(|n| n != NOTIFICATION_CANDIDATES_CLASS_CHECK_NAME);
        let dm_mention_insert = conn
            .execute(
                &format!(
                    r#"
                    INSERT INTO "{table}" (
                        recipient_bare_jid,
                        conversation_jid,
                        sender_jid,
                        thread_id,
                        stanza_id_by,
                        stanza_id,
                        class,
                        reason,
                        created_at_ms,
                        policy_error_count
                    ) VALUES (
                        'bob@example.com',
                        'alice@example.com',
                        'alice@example.com/web',
                        '',
                        'bob@example.com',
                        'post-migration-mention',
                        'dm_mention',
                        'offline_dm_mention',
                        1,
                        0
                    )
                    "#
                ),
                (),
            )
            .await;
        cleanup.await;
        assert!(
            canonical_present,
            "named CHECK constraint must remain after migration; saw {remaining:?}"
        );
        assert!(
            !anonymous_present,
            "anonymous CHECK constraint(s) must be dropped by migration; saw {remaining:?}"
        );
        dm_mention_insert.expect("dm_mention insert must succeed post-migration");
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
            enqueue_outbox_job_tx(
                &mut tx,
                candidate,
                target,
                &context,
                &RichSummary::minimal(),
                now_ms,
            )
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
            false,
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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

    /// T1 race-window regression: a candidate inserted while the
    /// recipient's XEP-0492 setting said "deliver" must still be
    /// suppressed at drain time if the setting flipped to `<never/>`
    /// between T0 emission and T1 dispatch.
    ///
    /// The T0 emission gate (in `offline_delivery.rs` /
    /// `groupchat_inbox.rs`) catches the common case where the
    /// setting was already `<never/>` at message-arrival time and
    /// short-circuits the insert — that case is covered by
    /// `xep0492_direct_chat_*_persists_no_candidate_row*` over in
    /// `tests/messages.rs`. This test exercises the *other*
    /// invocation moment of the same shared evaluator function: a
    /// row already exists, and the recipient's effective level has
    /// since changed.
    ///
    /// Expected behaviour: `drain_pending_candidates_into_outbox`
    /// re-evaluates against the fresh projection, marks the row
    /// outboxed without enqueuing a job, and returns `processed = 1`.
    /// The row exists only briefly during the race window — push
    /// output is preserved per the locked Q2 design.
    #[tokio::test]
    async fn t1_drain_reevaluates_xep0492_when_projection_changes_after_insert() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let candidate = candidate("archive-t1-race-window");
        register_push_target(&push_store, candidate.recipient_bare_jid(), &target).await;
        // Insert with no projection — defaults to `<always/>`, so a
        // T0 evaluator at this moment would say "deliver". The
        // candidate row gets persisted.
        store
            .insert_candidate(&candidate)
            .await
            .expect("candidate insert");
        assert_eq!(
            store
                .count_all_candidates()
                .await
                .expect("post-insert row count"),
            1,
            "T0 must have persisted the candidate row when projection said deliver"
        );

        // Race window: between T0 emission and T1 drain, the
        // recipient's XEP-0492 setting flips to `<never/>`.
        projection
            .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
                owner_bare_jid: candidate.recipient_bare_jid().clone(),
                conversation_jid: candidate.conversation_jid().clone(),
                conversation_kind:
                    crate::notification_settings_projection::ConversationKind::Direct,
                mode: waddle_xmpp::xep::NotificationLevel::Never,
                rich_payload_opt_in: false,
                source_version: 1,
                updated_at_ms: crate::time::now_ms(),
                source:
                    crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: candidate.conversation_jid().clone(),
            })
            .await
            .expect("flip xep-0492 setting to <never/>");

        // T1 drain re-evaluates against the now-`<never/>`
        // projection and suppresses the candidate.
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            1,
            "T1 race-window guard MUST count the candidate as processed via suppression"
        );

        // No outbox job — the suppression path goes
        // `mark_candidate_outboxed_tx` WITHOUT `enqueue_outbox_job_tx`.
        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty());
        // The candidate row still exists, now marked outboxed —
        // this is the documented race-window exception. The
        // compliance rule is "no row for the common case"; the
        // race-window row is acceptable per locked Q2.
        let mut rows = store
            .query(
                "SELECT outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["archive-t1-race-window"],
            )
            .await
            .expect("candidate marker query");
        let row = rows
            .next()
            .await
            .expect("candidate marker row")
            .expect("candidate marker row");
        assert!(
            row.get::<Option<i64>>(0)
                .expect("outboxed marker")
                .is_some(),
            "T1 race-window suppression MUST mark the candidate outboxed"
        );
    }

    #[tokio::test]
    async fn candidate_worker_marks_malformed_bare_sender_candidate_terminal() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
            rich_summary: RichSummary::minimal(),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
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
            .attr(
                minidom::rxml::xml_ncname!("conversation").to_owned(),
                "bob@example.com",
            )
            .attr(minidom::rxml::xml_ncname!("thread").to_owned(), "")
            .attr(minidom::rxml::xml_ncname!("class").to_owned(), "dm")
            .build();
        let payload = build_xep0357_notification_payload(3, &RichSummary::minimal(), &context);

        assert!(payload.is("notification", waddle_xmpp::xep::xep0357::NS_PUSH));
        let summary = payload
            .children()
            .find(|child| child.is("x", NS_DATA_FORMS))
            .expect("summary form");
        // XEP-0357 §4 example shows `<x xmlns='jabber:x:data'>` with
        // no `type` attribute — the form is a passively-encapsulated
        // summary, not a query response.
        assert_eq!(summary.attr("type"), None);
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

    #[test]
    fn xep0357_summary_form_emits_rich_fields_when_opted_in() {
        let context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
            .attr(
                minidom::rxml::xml_ncname!("conversation").to_owned(),
                "juliet@capulet.example",
            )
            .attr(minidom::rxml::xml_ncname!("thread").to_owned(), "")
            .attr(minidom::rxml::xml_ncname!("class").to_owned(), "dm")
            .build();
        let rich = RichSummary {
            sender: Some("juliet@capulet.example/balcony".parse().expect("jid")),
            body: Some("Wherefore art thou, Romeo?".to_string()),
        };
        let payload = build_xep0357_notification_payload(1, &rich, &context);

        let summary = payload
            .children()
            .find(|child| child.is("x", NS_DATA_FORMS))
            .expect("summary form");
        let field_value = |var: &str| -> Option<String> {
            summary
                .children()
                .find(|field| field.is("field", NS_DATA_FORMS) && field.attr("var") == Some(var))
                .and_then(|field| {
                    field
                        .children()
                        .find(|value| value.is("value", NS_DATA_FORMS))
                })
                .map(|value| value.text())
        };
        assert_eq!(field_value("message-count").as_deref(), Some("1"));
        assert_eq!(
            field_value("last-message-sender").as_deref(),
            Some("juliet@capulet.example/balcony")
        );
        assert_eq!(
            field_value("last-message-body").as_deref(),
            Some("Wherefore art thou, Romeo?")
        );
    }

    #[test]
    fn xep0357_summary_form_strips_body_but_keeps_sender_when_hint_stripped() {
        let context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
            .attr(
                minidom::rxml::xml_ncname!("conversation").to_owned(),
                "juliet@capulet.example",
            )
            .attr(minidom::rxml::xml_ncname!("thread").to_owned(), "")
            .attr(minidom::rxml::xml_ncname!("class").to_owned(), "dm")
            .build();
        // Sender preserved, body stripped (XEP-0334 hint precedence).
        let rich = RichSummary {
            sender: Some("juliet@capulet.example/balcony".parse().expect("jid")),
            body: None,
        };
        let payload = build_xep0357_notification_payload(1, &rich, &context);
        let summary = payload
            .children()
            .find(|child| child.is("x", NS_DATA_FORMS))
            .expect("summary form");
        assert!(summary
            .children()
            .any(|field| field.attr("var") == Some("last-message-sender")));
        assert!(!summary
            .children()
            .any(|field| field.attr("var") == Some("last-message-body")));
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

    /// Regression for self-DM structural-validity rejection at the
    /// `NotificationCandidate::direct_message` constructor (#506
    /// compliance: no push candidate/outbox entry for self-directed
    /// notifications). Self-DM is *input validation*, not recipient-
    /// state suppression, so it lives at the typed constructor
    /// boundary alongside `require_full_sender_jid` and
    /// `ArchiveStanzaIdOwnerMismatch`. No candidate row is ever
    /// persisted, satisfying both:
    ///   (a) #506 Q3: T0 has no recipient-state reads — sender vs
    ///       recipient JID comparison is message-intrinsic provenance.
    ///   (b) compliance: self-notifications produce no candidate or
    ///       outbox entry.
    #[tokio::test]
    async fn self_directed_dm_candidate_is_rejected_at_constructor() {
        let recipient = bare("alice@example.com");
        let result = NotificationCandidate::direct_message(
            recipient.clone(),
            "alice@example.com/desktop"
                .parse()
                .expect("full self sender"),
            StanzaId::new("self-dm-archive", Jid::from(recipient.clone())),
            false,
        );
        assert!(matches!(
            result,
            Err(NotificationOutboxError::SelfDirectedNotificationCandidate(jid))
                if jid == recipient
        ));
    }

    /// Regression that the offline-delivery path silently drops self-
    /// directed notification attempts without persisting anything to
    /// `notification_candidates` or `notification_outbox`. End-to-end
    /// surface of the constructor rejection above: insert is attempted
    /// once, fails fast as a typed error, and the candidate table
    /// stays empty.
    #[tokio::test]
    async fn self_directed_dm_inserts_no_candidate_row() {
        let store = store().await;
        let recipient = bare("alice@example.com");
        let result = NotificationCandidate::direct_message(
            recipient.clone(),
            "alice@example.com/desktop"
                .parse()
                .expect("full self sender"),
            StanzaId::new("self-dm-archive", Jid::from(recipient.clone())),
            false,
        );
        assert!(matches!(
            result,
            Err(NotificationOutboxError::SelfDirectedNotificationCandidate(
                _
            ))
        ));
        // No candidate insert attempted because the constructor refused
        // to produce one. Verify the candidate table is empty.
        assert!(
            store
                .pending_candidates(16)
                .await
                .expect("pending candidates")
                .is_empty(),
            "self-DM must not persist a candidate row"
        );
        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "self-DM must not persist an outbox job"
        );
    }

    /// Regression for the unknown-room-policy deferral behavior. When
    /// the [`RoomPolicyStore`] returns `Ok(None)` (room actor not
    /// currently live), the T1 evaluator MUST defer the candidate via
    /// the policy-error backoff rather than silently defaulting to
    /// public — see [`T1PushDispatchOutcome::DeferUnknownRoomPolicy`].
    /// Dropped pushes for members-only rooms (`Always` default level
    /// → `NotifyAll` candidates SHOULD push) would otherwise be the
    /// blast radius.
    #[tokio::test]
    async fn unknown_room_policy_defers_groupchat_candidate_at_t1() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = UnknownRoomPolicy::new();
        let recipient = bare("alice@example.com");
        let room = bare("team@muc.example.com");
        register_push_target(&push_store, &recipient, &target).await;

        let candidate = groupchat_candidate_for(
            &recipient,
            &room,
            "team@muc.example.com/bob".parse().expect("room occupant"),
            "archive-unknown-policy",
            NotificationClass::NotifyAll,
        );
        store
            .insert_candidate(&candidate)
            .await
            .expect("groupchat insert");

        // Drain returns 0 processed because the candidate deferred
        // (not marked outboxed, not enqueued).
        assert_eq!(
            store
                .drain_pending_candidates_into_outbox(
                    &push_store,
                    &blocking,
                    &projection,
                    drain_deps_with_noop_activity(
                        &room_policy,
                        &NoopDndReader,
                        noop_activity_reader()
                    ),
                    &bare("push.example.com"),
                    16,
                )
                .await
                .expect("drain candidates"),
            0,
            "unknown room policy must NOT count as a processed candidate",
        );

        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "unknown room policy must NOT enqueue a push job",
        );
        // The candidate is still un-outboxed but has its
        // policy_error_count incremented and next_attempt_at_ms set
        // in the future, so it is NOT pending right now but WILL be
        // retried by the next drain pass after the backoff elapses.
        let mut rows = store
            .query(
                "SELECT policy_error_count, next_attempt_at_ms, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["archive-unknown-policy"],
            )
            .await
            .expect("candidate row query");
        let row = rows
            .next()
            .await
            .expect("candidate row read")
            .expect("candidate row");
        let policy_error_count: i64 = row.get(0).expect("policy_error_count");
        let next_attempt: Option<i64> = row.get(1).expect("next_attempt_at_ms");
        let outboxed: Option<i64> = row.get(2).expect("outboxed_at_ms");
        assert_eq!(
            policy_error_count, 1,
            "deferral must bump policy_error_count",
        );
        assert!(
            next_attempt.is_some(),
            "deferral must schedule a retry via next_attempt_at_ms",
        );
        assert!(
            outboxed.is_none(),
            "deferral must NOT mark the candidate outboxed",
        );
    }

    /// The per-batch room-policy cache MUST collapse repeat lookups
    /// for the same room into a single [`RoomPolicyStore`]
    /// round-trip. With one room and N candidates, only one actor
    /// call is permitted.
    #[tokio::test]
    async fn room_policy_lookup_is_cached_within_drain_batch() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = CountingPublicRoomPolicy::new();
        let recipient = bare("alice@example.com");
        let room = bare("team@muc.example.com");
        register_push_target(&push_store, &recipient, &target).await;

        // Three distinct groupchat candidates for the *same* room
        // (different archive ids, all PersonalMention so they hit
        // the room-policy path).
        for id in ["arc-1", "arc-2", "arc-3"] {
            let candidate = groupchat_candidate_for(
                &recipient,
                &room,
                "team@muc.example.com/bob".parse().expect("room occupant"),
                id,
                NotificationClass::PersonalMention,
            );
            store
                .insert_candidate(&candidate)
                .await
                .expect("groupchat insert");
        }

        let _ = store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        assert_eq!(
            room_policy.call_count(),
            1,
            "per-batch room-policy cache must collapse repeat lookups for the same room",
        );
    }

    /// The per-batch deferral cache MUST short-circuit
    /// `RoomPolicyCacheEntry::Unknown` once observed — subsequent
    /// candidates for the same room in the same batch SHOULD reuse
    /// the deferral outcome instead of re-asking a failing actor.
    #[tokio::test]
    async fn unknown_room_policy_lookup_is_cached_within_drain_batch() {
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = UnknownRoomPolicy::new();
        let recipient = bare("alice@example.com");
        let room = bare("team@muc.example.com");
        register_push_target(&push_store, &recipient, &target).await;

        for id in ["arc-1", "arc-2", "arc-3"] {
            let candidate = groupchat_candidate_for(
                &recipient,
                &room,
                "team@muc.example.com/bob".parse().expect("room occupant"),
                id,
                NotificationClass::NotifyAll,
            );
            store
                .insert_candidate(&candidate)
                .await
                .expect("groupchat insert");
        }

        let _ = store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        assert_eq!(
            room_policy.call_count(),
            1,
            "deferral outcomes must be cached per-batch — failing actor must NOT be re-queried per candidate",
        );
    }

    /// A `RoomPolicyStore::room_members_only` returning a typed
    /// `RoomPolicyLookup` error MUST classify the cache entry as
    /// `LookupError`, not `NotLive`. The dispatch outcome remains
    /// `DeferUnknownRoomPolicy` either way, but the source split is
    /// what gives operators an actionable signal vs routine dormancy.
    /// Caching still applies: a single failing lookup populates one
    /// `Unknown(LookupError)` entry and every subsequent candidate
    /// for that room reuses it.
    #[tokio::test]
    async fn room_policy_lookup_error_classifies_as_lookup_error_and_caches() {
        let room_policy = ErroringRoomPolicy::new();
        let room = bare("team@muc.example.com");
        let mut cache = std::collections::BTreeMap::<BareJid, RoomPolicyCacheEntry>::new();

        let first = resolve_cached_room_policy(&room_policy, &room, &mut cache).await;
        assert_eq!(
            first,
            RoomPolicyCacheEntry::Unknown(UnknownRoomPolicySource::LookupError),
            "typed RoomPolicyLookup error must classify as LookupError, not NotLive"
        );

        let second = resolve_cached_room_policy(&room_policy, &room, &mut cache).await;
        assert_eq!(
            second, first,
            "second lookup MUST hit the cache and return the same typed entry"
        );

        assert_eq!(
            room_policy.call_count(),
            1,
            "cache MUST short-circuit subsequent lookups — failing actor is never re-asked in the same batch",
        );
    }

    /// A `RoomPolicyStore::room_members_only` returning `Ok(None)`
    /// MUST classify the cache entry as `NotLive`, not `LookupError`.
    /// Distinguishing these is the whole point of the typed source —
    /// `NotLive` is routine dormancy and stays at `debug!` level in
    /// the drain loop, whereas `LookupError` triggers the once-per-
    /// batch `warn!` for operators to triage.
    #[tokio::test]
    async fn room_policy_ok_none_classifies_as_not_live() {
        let room_policy = UnknownRoomPolicy::new();
        let room = bare("team@muc.example.com");
        let mut cache = std::collections::BTreeMap::<BareJid, RoomPolicyCacheEntry>::new();

        let entry = resolve_cached_room_policy(&room_policy, &room, &mut cache).await;
        assert_eq!(
            entry,
            RoomPolicyCacheEntry::Unknown(UnknownRoomPolicySource::NotLive),
            "Ok(None) must classify as NotLive, distinct from LookupError"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // Slice 2a — SuppressedReason audit + new suppressors (#526).
    // ─────────────────────────────────────────────────────────────

    /// `SuppressedReason` is the canonical audit shape. Every variant
    /// MUST round-trip through `as_db_value` / `from_db_value` so a
    /// row written today can be decoded tomorrow without ambiguity.
    /// The closed-set discipline is what keeps the CHECK constraint
    /// + the labeled prometheus counter in lockstep.
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

    /// Wire-contract lockstep guard: every `SuppressedReason` variant
    /// MUST have its `as_db_value()` listed in
    /// `waddle_xmpp::prometheus::push_suppressed_reasons()`. The
    /// prometheus parallel constant lives upstream of this enum (in
    /// `waddle-xmpp`) and cannot import the typed enum, so the
    /// invariant is enforced from this side. Drift here means an
    /// `increment_push_suppressed(...)` call for the missing variant
    /// would hit the `waddle_push_suppressed_unknown_reason_total`
    /// catch-all instead of the typed counter — observable but
    /// incorrect.
    #[test]
    fn suppressed_reason_wire_contract_matches_prometheus_parallel_constant() {
        let wire = waddle_xmpp::prometheus::push_suppressed_reasons();
        for reason in SuppressedReason::ALL.iter().copied() {
            let db = reason.as_db_value();
            assert!(
                wire.contains(&db),
                "`SuppressedReason::{reason:?}` (db value `{db}`) is missing from \
                 `waddle_xmpp::prometheus::PUSH_SUPPRESSED_REASONS`; the parallel \
                 constant has drifted from the typed enum"
            );
        }
        // Reverse direction: any string in the parallel constant that
        // does NOT round-trip through `SuppressedReason::from_db_value`
        // is dead weight in the metrics surface.
        for label in wire.iter().copied() {
            assert!(
                SuppressedReason::from_db_value(label).is_ok(),
                "`PUSH_SUPPRESSED_REASONS` entry `{label}` is not a known \
                 `SuppressedReason::as_db_value()`; the parallel constant has drifted"
            );
        }
        assert_eq!(
            wire.len(),
            SuppressedReason::ALL.len(),
            "wire-contract length must match the typed enum cardinality"
        );
    }

    #[test]
    fn postgres_suppressed_reason_constraint_match_accepts_current_definition() {
        let postgres_definition = "CHECK (((suppressed_reason IS NULL) OR ((suppressed_reason)::text = ANY ((ARRAY['xep0357_self'::character varying, 'xep0357_no_registration'::character varying, 'xep0357_registration_disabled'::character varying, 'xep0492_never'::character varying, 'xep0492_on_mention_miss'::character varying, 'xep0191_blocked'::character varying, 'xep0513_noping'::character varying, 'xep0513_active_miss'::character varying, 'waddle_dnd'::character varying, 'provider_rejected'::character varying, 'provider_token_expired'::character varying, 'xep0357_push_service_degraded'::character varying])::text[]))))";
        assert!(
            notification_candidates_suppressed_reason_constraint_matches_expected(
                postgres_definition
            )
        );
    }

    #[test]
    fn sqlite_suppressed_reason_constraint_match_rejects_partial_definition() {
        // A schema that advertises only some of the typed reasons must
        // be flagged stale so the migration rebuilds the CHECK.
        let sqlite_create_sql = "CREATE TABLE notification_candidates (suppressed_reason TEXT CHECK (suppressed_reason IS NULL OR suppressed_reason IN ('xep0492_never')))";
        assert!(
            !notification_candidates_suppressed_reason_constraint_matches_expected(
                sqlite_create_sql
            ),
        );
    }

    /// On a fresh store, cold-init MUST advertise the
    /// `suppressed_reason` column with the CHECK constraint that
    /// accepts the full closed-set of typed reasons. A direct INSERT
    /// using each typed db value MUST succeed; an INSERT with a
    /// nonsense value MUST be rejected by the CHECK.
    #[tokio::test]
    async fn suppressed_reason_check_constraint_accepts_every_typed_value() {
        let store = store().await;
        let recipient = bare("alice@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
        // Iterate the closed-set `ALL` array so a future enum extension
        // joins this audit automatically — no hand-maintained parallel
        // list to drift.
        for (idx, reason) in SuppressedReason::ALL.iter().enumerate() {
            let stanza_id = format!("audit-{idx}");
            let candidate = NotificationCandidate::direct_message(
                recipient.clone(),
                sender_jid.clone(),
                StanzaId::new(stanza_id.clone(), Jid::from(recipient.clone())),
                false,
            )
            .expect("candidate");
            assert_eq!(
                store
                    .insert_candidate(&candidate)
                    .await
                    .expect("insert candidate"),
                NotificationCandidateInsertOutcome::Inserted,
            );
            let mut tx = store.db.begin().await.expect("begin tx");
            record_candidate_suppressed_reason_tx(&mut tx, &candidate, *reason)
                .await
                .expect("record reason");
            tx.commit().await.expect("commit");
        }

        // A nonsense value MUST be rejected by the CHECK.
        let insert_result = store
            .execute(
                r#"
                INSERT INTO notification_candidates (
                    recipient_bare_jid, conversation_jid, sender_jid, thread_id,
                    stanza_id_by, stanza_id, class, reason, created_at_ms,
                    policy_error_count, next_attempt_at_ms, outboxed_at_ms,
                    suppressed_reason, noping, no_store, no_permanent_store
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?, 0, 0, 0)
                "#,
                crate::db_params![
                    "alice@example.com",
                    "bob@example.com",
                    "bob@example.com/web",
                    "",
                    "alice@example.com",
                    "bad-value",
                    "dm",
                    "offline_dm",
                    1_i64,
                    0_i64,
                    "not-a-real-reason",
                ],
            )
            .await;
        assert!(
            insert_result.is_err(),
            "CHECK constraint must reject nonsense suppressed_reason"
        );
    }

    /// XEP-0492 `<never/>` suppression at T1 MUST persist
    /// `Xep0492Never` onto the candidate row's `suppressed_reason`
    /// column, NOT enqueue a job, and increment the metric counter
    /// labeled by the typed db value.
    #[tokio::test]
    async fn t1_xep0492_never_records_typed_suppressed_reason() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");

        // Persist a `<never/>` notification setting for the recipient
        // against `sender`'s DM conversation.
        projection
            .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
                owner_bare_jid: recipient.clone(),
                conversation_jid: sender.clone(),
                conversation_kind:
                    crate::notification_settings_projection::ConversationKind::Direct,
                mode: waddle_xmpp::xep::NotificationLevel::Never,
                source:
                    crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: sender.clone(),
                updated_at_ms: 1,
                rich_payload_opt_in: false,
                source_version: 1,
            })
            .await
            .expect("seed never level");

        let candidate = candidate_for(&recipient, &sender, "t1-never");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["t1-never"],
            )
            .await
            .expect("query suppressed_reason");
        let row = rows.next().await.expect("row").expect("row exists");
        let reason: Option<String> = row.get(0).expect("reason");
        let outboxed: Option<i64> = row.get(1).expect("outboxed_at_ms");
        assert_eq!(reason.as_deref(), Some("xep0492_never"));
        assert!(
            outboxed.is_some(),
            "T1 suppression must mark candidate outboxed"
        );
        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "T1 suppression MUST NOT enqueue a job",
        );

        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            rendered.contains("waddle_push_suppressed_total{reason=\"xep0492_never\"} 1"),
            "metric counter for xep0492_never must increment; rendered={rendered}",
        );
    }

    /// XEP-0492 `<on-mention/>` setting with a non-mention candidate
    /// (DM without explicit mention) MUST suppress at T1 with the
    /// typed `Xep0492OnMentionMiss` audit reason.
    #[tokio::test]
    async fn t1_xep0492_on_mention_miss_records_typed_suppressed_reason() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");

        projection
            .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
                owner_bare_jid: recipient.clone(),
                conversation_jid: sender.clone(),
                conversation_kind:
                    crate::notification_settings_projection::ConversationKind::Direct,
                mode: waddle_xmpp::xep::NotificationLevel::OnMention,
                source:
                    crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: sender.clone(),
                updated_at_ms: 1,
                rich_payload_opt_in: false,
                source_version: 1,
            })
            .await
            .expect("seed on-mention level");

        let candidate = candidate_for(&recipient, &sender, "t1-on-mention");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["t1-on-mention"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row exists");
        let reason: Option<String> = row.get(0).expect("reason");
        assert_eq!(reason.as_deref(), Some("xep0492_on_mention_miss"));

        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            rendered.contains("waddle_push_suppressed_total{reason=\"xep0492_on_mention_miss\"} 1"),
        );
    }

    /// XEP-0191 blocking at T1 MUST record `Xep0191Blocked` onto the
    /// candidate row before marking it outboxed-without-job.
    #[tokio::test]
    async fn t1_xep0191_blocked_records_typed_suppressed_reason() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");

        // Block the sender on the recipient's blocklist.
        blocking.set_blocklist(recipient.clone(), vec![sender.clone()]);

        let candidate = candidate_for(&recipient, &sender, "t1-blocked");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["t1-blocked"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row exists");
        let reason: Option<String> = row.get(0).expect("reason");
        assert_eq!(reason.as_deref(), Some("xep0191_blocked"));
        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(rendered.contains("waddle_push_suppressed_total{reason=\"xep0191_blocked\"} 1"),);
    }

    /// Stage-split contract: the same hinted candidate MUST yield
    /// `Deliver` when evaluated at [`PushEvalStage::T0Emit`] (so the
    /// row gets persisted with its hint bits) and `Suppressed` at
    /// [`PushEvalStage::T1Drain`] (where the typed `suppressed_reason`
    /// audit fires). Without this split, hinted candidates would
    /// disappear at T0 with no audit trail.
    #[tokio::test]
    async fn evaluator_stage_split_defers_hint_suppressors_to_t1() {
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
        let noping_candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid.clone(),
            StanzaId::new("stage-split-noping", Jid::from(recipient.clone())),
            false,
            NotificationMessageHints::none().with_noping(true),
        )
        .expect("candidate");
        let activity_reader = noop_activity_reader();
        let eval_deps = eval_deps_for_test(&projection, &room_policy, &dnd_reader, activity_reader);
        let (mut room_policy_cache, mut dnd_cache, mut activity_cache) = fresh_eval_caches();
        let mut eval_caches = PushEvalCaches {
            room_policy: &mut room_policy_cache,
            dnd: &mut dnd_cache,
            activity: &mut activity_cache,
        };

        // T0Emit MUST NOT suppress on noping — the row must persist
        // so T1 records the audit.
        let t0 = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            eval_deps,
            &noping_candidate,
            &mut eval_caches,
        )
        .await
        .expect("t0 eval");
        assert!(
            matches!(t0, T1PushDispatchOutcome::Deliver { .. }),
            "T0Emit must NOT suppress on message-frozen `<noping/>` so the candidate persists; got {t0:?}"
        );

        // T1Drain MUST suppress with the typed Xep0513Noping reason.
        let t1 = evaluate_push_gate_at_dispatch(
            PushEvalStage::T1Drain,
            eval_deps,
            &noping_candidate,
            &mut eval_caches,
        )
        .await
        .expect("t1 eval");
        assert!(
            matches!(
                t1,
                T1PushDispatchOutcome::Suppressed {
                    reason: SuppressedReason::Xep0513Noping
                }
            ),
            "T1Drain must suppress noping with the typed Xep0513Noping reason; got {t1:?}"
        );

        // Contrast: XEP-0334 storage hints are NOT push suppressors.
        // Per XEP-0334 §3/§8 they scope to message storage, not push
        // delivery, so a `<no-store/>` candidate delivers a (minimal)
        // push at both stages — the hint only strips the rich body.
        let no_store_candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid,
            StanzaId::new("stage-split-no-store", Jid::from(recipient.clone())),
            false,
            NotificationMessageHints::none().with_xep0334(true, false),
        )
        .expect("candidate");
        let t0_no_store = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            eval_deps,
            &no_store_candidate,
            &mut eval_caches,
        )
        .await
        .expect("t0 eval");
        assert!(matches!(t0_no_store, T1PushDispatchOutcome::Deliver { .. }));
        let t1_no_store = evaluate_push_gate_at_dispatch(
            PushEvalStage::T1Drain,
            eval_deps,
            &no_store_candidate,
            &mut eval_caches,
        )
        .await
        .expect("t1 eval");
        assert!(
            matches!(t1_no_store, T1PushDispatchOutcome::Deliver { .. }),
            "XEP-0334 <no-store/> must not suppress the push; got {t1_no_store:?}"
        );
    }

    /// XEP-0513 `<noping/>` carried on the candidate row MUST suppress
    /// at T1 with the typed `Xep0513Noping` reason. Tests the
    /// message-frozen path: candidate is constructed with the noping
    /// bit set, persisted, then the drain reads it back and suppresses.
    #[tokio::test]
    async fn t1_noping_records_typed_suppressed_reason() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
        let candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid,
            StanzaId::new("t1-noping", Jid::from(recipient.clone())),
            false,
            NotificationMessageHints::none().with_noping(true),
        )
        .expect("candidate");
        assert!(candidate.noping());
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason, noping FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["t1-noping"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            Some("xep0513_noping")
        );
        assert_eq!(row.get::<i64>(1).expect("noping"), 1);
        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(rendered.contains("waddle_push_suppressed_total{reason=\"xep0513_noping\"} 1"),);
    }

    /// Upsert a per-conversation rich-payload opt-in for a DM so the
    /// drain's T1 evaluator resolves rich XEP-0357 summaries.
    async fn opt_in_rich_payload(
        projection: &crate::notification_settings_projection::NotificationSettingsProjectionStore,
        recipient: &BareJid,
        conversation: &BareJid,
    ) {
        projection
            .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
                owner_bare_jid: recipient.clone(),
                conversation_jid: conversation.clone(),
                conversation_kind:
                    crate::notification_settings_projection::ConversationKind::Direct,
                mode: waddle_xmpp::xep::NotificationLevel::Always,
                rich_payload_opt_in: true,
                source_version: 1,
                updated_at_ms: 1,
                source:
                    crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: conversation.clone(),
            })
            .await
            .expect("opt-in upsert");
    }

    /// #719: with the rich-payload opt-in set and no XEP-0334 storage
    /// hint, the drained push carries the full XEP-0357 §5.4 summary —
    /// both `last-message-sender` and `last-message-body`.
    #[tokio::test]
    async fn t1_opt_in_without_hint_emits_rich_summary() {
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let conversation = bare("bob@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
        opt_in_rich_payload(&projection, &recipient, &conversation).await;
        register_push_target(&push_store, &recipient, &target()).await;
        let candidate = NotificationCandidate::direct_message(
            recipient.clone(),
            sender_jid.clone(),
            StanzaId::new("t1-rich", Jid::from(recipient.clone())),
            false,
        )
        .expect("candidate")
        .with_last_message_body(Some("Wherefore art thou, Romeo?".to_string()));
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].rich_summary().sender.as_ref(), Some(&sender_jid));
        assert_eq!(
            jobs[0].rich_summary().body.as_deref(),
            Some("Wherefore art thou, Romeo?")
        );
        // End-to-end: the dispatched XEP-0357 §5.4 wire shape carries
        // both optional fields.
        let item = jobs[0].to_xep0357_pubsub_item();
        let payload = item.payload.expect("payload");
        let summary = payload
            .children()
            .find(|child| child.is("x", NS_DATA_FORMS))
            .expect("summary form");
        assert!(summary
            .children()
            .any(|field| field.attr("var") == Some("last-message-sender")));
        assert!(summary
            .children()
            .any(|field| field.attr("var") == Some("last-message-body")));
    }

    /// #719 / XEP-0334 §3 precedence: even with the rich-payload opt-in
    /// set, a `<no-store/>` candidate delivers a push whose summary
    /// carries NO `last-message-body` — the sender is preserved, and the
    /// body is never persisted onto the candidate row.
    #[tokio::test]
    async fn t1_no_store_strips_body_but_still_delivers_with_opt_in() {
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let conversation = bare("bob@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
        opt_in_rich_payload(&projection, &recipient, &conversation).await;
        register_push_target(&push_store, &recipient, &target()).await;
        let candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid,
            StanzaId::new("t1-no-store", Jid::from(recipient.clone())),
            false,
            NotificationMessageHints::none().with_xep0334(true, false),
        )
        .expect("candidate")
        .with_last_message_body(Some("secret".to_string()));
        // Storage conformance: the body is never even persisted.
        assert_eq!(candidate.last_message_body(), None);
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        // The push is delivered (no suppression recorded)...
        let mut rows = store
            .query(
                "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["t1-no-store"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            None,
            "<no-store/> must NOT suppress the push under #719",
        );
        // ...but the summary carries the sender and NOT the body.
        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].rich_summary().sender.is_some());
        assert_eq!(jobs[0].rich_summary().body, None);
    }

    /// #719 / XEP-0334 §3 precedence for `<no-permanent-store/>`: same
    /// as `<no-store/>` — body stripped, push delivered.
    #[tokio::test]
    async fn t1_no_permanent_store_strips_body_but_still_delivers_with_opt_in() {
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let conversation = bare("bob@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
        opt_in_rich_payload(&projection, &recipient, &conversation).await;
        register_push_target(&push_store, &recipient, &target()).await;
        let candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid,
            StanzaId::new("t1-no-perm-store", Jid::from(recipient.clone())),
            false,
            NotificationMessageHints::none().with_xep0334(false, true),
        )
        .expect("candidate")
        .with_last_message_body(Some("secret".to_string()));
        assert_eq!(candidate.last_message_body(), None);
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["t1-no-perm-store"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            None,
            "<no-permanent-store/> must NOT suppress the push under #719",
        );
        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].rich_summary().sender.is_some());
        assert_eq!(jobs[0].rich_summary().body, None);
    }

    /// `NoopDndReader` MUST report every user as `Inactive` so slice
    /// 2a's defaulted call sites never trigger DnD suppression while
    /// the real impl is still pending (#367).
    #[tokio::test]
    async fn noop_dnd_reader_reports_inactive() {
        let reader = NoopDndReader;
        let user = bare("alice@example.com");
        let state = reader.dnd_state(&user).await.expect("noop dnd");
        assert_eq!(state, DndState::Inactive);
    }

    /// A `DndReader` that reports `Active` MUST suppress at T1 with
    /// `WaddleDnd`, even when the recipient has no other suppressors
    /// in play.
    #[tokio::test]
    async fn t1_active_dnd_suppresses_with_typed_reason() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();

        struct ActiveDndReader;
        #[async_trait::async_trait]
        impl DndReader for ActiveDndReader {
            async fn dnd_state(
                &self,
                _user: &BareJid,
            ) -> Result<DndState, NotificationOutboxError> {
                Ok(DndState::Active)
            }
        }

        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = ActiveDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");
        let candidate = candidate_for(&recipient, &sender, "t1-dnd");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["t1-dnd"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            Some("waddle_dnd"),
        );
        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(rendered.contains("waddle_push_suppressed_total{reason=\"waddle_dnd\"} 1"));
    }

    /// Schema regression: cold-init MUST produce a
    /// `notification_candidates.suppressed_reason` column with the
    /// named CHECK constraint accepting every typed db value.
    #[tokio::test]
    async fn cold_init_creates_suppressed_reason_column_and_check() {
        let store = store().await;
        // Insert a row, then update suppressed_reason to every typed
        // db value in turn — all MUST succeed.
        let recipient = bare("alice@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
        let candidate = NotificationCandidate::direct_message(
            recipient.clone(),
            sender_jid,
            StanzaId::new("schema-probe", Jid::from(recipient.clone())),
            false,
        )
        .expect("candidate");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");
        for reason_db in NOTIFICATION_CANDIDATES_SUPPRESSED_REASON_VALUES {
            store
                .execute(
                    "UPDATE notification_candidates SET suppressed_reason = ? WHERE stanza_id = ?",
                    crate::db_params![reason_db, "schema-probe"],
                )
                .await
                .expect("update suppressed_reason");
        }
        // Reset to NULL also OK (unsuppressed/delivered shape).
        store
            .execute(
                "UPDATE notification_candidates SET suppressed_reason = NULL WHERE stanza_id = ?",
                crate::db_params!["schema-probe"],
            )
            .await
            .expect("reset to NULL");
    }

    /// Legacy upgrade regression: a database created with an
    /// older schema that lacks the `suppressed_reason`, `noping`,
    /// `no_store`, and `no_permanent_store` columns MUST upgrade
    /// cleanly when `NotificationOutboxStore::new` runs the
    /// `add_column_if_missing` + suppressed-reason migration.
    /// Both legacy rows AND newly-inserted rows are insertable
    /// and decodable after migration.
    #[tokio::test]
    async fn legacy_schema_upgrade_adds_suppressed_reason_and_hints() {
        let db = Database::in_memory("notification-outbox-legacy-suppressed")
            .await
            .unwrap();
        let conn = db.guard().await.expect("db guard");
        conn.execute(
            r#"
            CREATE TABLE notification_candidates (
                recipient_bare_jid TEXT NOT NULL,
                conversation_jid TEXT NOT NULL,
                sender_jid TEXT NOT NULL,
                thread_id TEXT NOT NULL DEFAULT '',
                stanza_id_by TEXT NOT NULL,
                stanza_id TEXT NOT NULL,
                class TEXT NOT NULL CHECK (class IN ('dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all')),
                reason TEXT NOT NULL CHECK (reason IN ('offline_dm', 'offline_dm_mention', 'groupchat_personal_mention', 'groupchat_channel_mention', 'groupchat_active_channel_mention', 'groupchat_notify_all')),
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
        .expect("create legacy candidate table");
        conn.execute(
            r#"
            INSERT INTO notification_candidates (
                recipient_bare_jid, conversation_jid, sender_jid, thread_id,
                stanza_id_by, stanza_id, class, reason, created_at_ms,
                policy_error_count, next_attempt_at_ms, outboxed_at_ms
            ) VALUES (
                'alice@example.com', 'bob@example.com', 'bob@example.com/web', '',
                'alice@example.com', 'legacy-row', 'dm', 'offline_dm', 1, 0, NULL, NULL
            )
            "#,
            (),
        )
        .await
        .expect("insert legacy candidate");
        drop(conn);

        let store = NotificationOutboxStore::new(db)
            .await
            .expect("store migrates legacy schema");
        // Insert a new candidate with the noping bit set; the column
        // must exist and accept the value.
        let recipient = bare("alice@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");
        let new_candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid,
            StanzaId::new("post-upgrade", Jid::from(recipient.clone())),
            true,
            NotificationMessageHints::none()
                .with_noping(true)
                .with_xep0334(true, true),
        )
        .expect("post-upgrade candidate");
        store
            .insert_candidate(&new_candidate)
            .await
            .expect("insert post-upgrade candidate");
        let mut rows = store
            .query(
                "SELECT suppressed_reason, noping, no_store, no_permanent_store FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["post-upgrade"],
            )
            .await
            .expect("query post-upgrade row");
        let row = rows.next().await.expect("row").expect("row exists");
        let reason: Option<String> = row.get(0).expect("reason");
        assert!(reason.is_none());
        assert_eq!(row.get::<i64>(1).expect("noping"), 1);
        assert_eq!(row.get::<i64>(2).expect("no_store"), 1);
        assert_eq!(row.get::<i64>(3).expect("no_permanent_store"), 1);

        // Legacy row's hint columns must default to 0.
        let mut rows = store
            .query(
                "SELECT noping, no_store, no_permanent_store, suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["legacy-row"],
            )
            .await
            .expect("query legacy row");
        let row = rows.next().await.expect("row").expect("legacy row");
        assert_eq!(row.get::<i64>(0).expect("noping"), 0);
        assert_eq!(row.get::<i64>(1).expect("no_store"), 0);
        assert_eq!(row.get::<i64>(2).expect("no_permanent_store"), 0);
        assert!(row.get::<Option<String>>(3).expect("reason").is_none());
    }

    // ---------------------------------------------------------------
    // Storage-preservation regressions for slice 2a suppressors
    // ---------------------------------------------------------------
    //
    // Contract: when push is suppressed at T0 (compliance gate) or T1
    // (audit gate) by ANY suppressor — XEP-0191 blocking, XEP-0492
    // `<never/>`/`<on-mention/>` miss, XEP-0513 `<noping/>`, XEP-0334
    // hints, or Waddle DnD — the suppressor only affects the XEP-0357
    // push fanout. The message MUST still be archived (XEP-0313 MAM),
    // projected into the recipient's XEP-0430 inbox, queued in
    // XEP-0160 offline storage when applicable, and delivered to
    // online resources per RFC 6121. None of those upstream writes
    // belong to the notification-outbox layer; the candidate
    // emission code path only writes to `notification_candidates`
    // and `notification_outbox`. The tests below pre-seed an
    // inbox-storage witness BEFORE the candidate emission and verify
    // the witness is byte-identical afterwards — proving the outbox
    // layer never rolls back or mutates upstream artifacts. By
    // symmetry, MAM and pending_delivery (likewise written upstream,
    // never by this layer) are preserved by the same invariant. The
    // websocket-integration test `xep0357_suppression_preserves_mam_inbox_and_audit`
    // in `server::routes::websocket::tests::messages` covers the
    // full upstream surface (MAM + inbox + pending_delivery) in one
    // wire-level shot for the dominant DM `<never/>` path.

    /// XEP-0492 `<never/>` is a compliance-required suppressor that
    /// runs at T0Emit: the candidate row MUST NOT be persisted (per
    /// the existing T0 contract in `enqueue_xep0357_notification_candidate_for_message`),
    /// but any upstream artifact (here: an inbox row the recipient
    /// already has for this conversation) MUST be untouched. The
    /// typed metric counter MUST tick once for the suppression audit.
    #[tokio::test]
    async fn xep0492_never_suppression_preserves_pending_delivery_and_audit_via_metric() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let projection = settings_projection().await;
        let room_policy = NoopRoomPolicy;
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");

        // Seed XEP-0430 inbox witness BEFORE candidate emission.
        let (inbox, witness) =
            seed_inbox_witness(&recipient, &sender, "archive-never-witness", 42, 3).await;

        // Recipient has explicitly muted this conversation.
        projection
            .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
                owner_bare_jid: recipient.clone(),
                conversation_jid: sender.clone(),
                conversation_kind:
                    crate::notification_settings_projection::ConversationKind::Direct,
                mode: waddle_xmpp::xep::NotificationLevel::Never,
                source:
                    crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: sender.clone(),
                updated_at_ms: 1,
                rich_payload_opt_in: false,
                source_version: 1,
            })
            .await
            .expect("seed never level");

        // Drive the T0 evaluator the same way
        // `enqueue_xep0357_notification_candidate_for_message` does.
        let candidate = NotificationCandidate::direct_message(
            recipient.clone(),
            sender_jid,
            StanzaId::new("never-t0", Jid::from(recipient.clone())),
            false,
        )
        .expect("candidate");
        let activity_reader = noop_activity_reader();
        let eval_deps = eval_deps_for_test(&projection, &room_policy, &dnd_reader, activity_reader);
        let (mut room_policy_cache, mut dnd_cache, mut activity_cache) = fresh_eval_caches();
        let mut eval_caches = PushEvalCaches {
            room_policy: &mut room_policy_cache,
            dnd: &mut dnd_cache,
            activity: &mut activity_cache,
        };
        let outcome = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            eval_deps,
            &candidate,
            &mut eval_caches,
        )
        .await
        .expect("t0 eval");
        assert!(
            matches!(
                outcome,
                T1PushDispatchOutcome::Suppressed {
                    reason: SuppressedReason::Xep0492Never
                }
            ),
            "T0 MUST suppress <never/> with the typed Xep0492Never audit; got {outcome:?}"
        );
        // Mirror the T0 emission contract: tick the metric, do NOT
        // persist a candidate row.
        waddle_xmpp::prometheus::increment_push_suppressed(
            SuppressedReason::Xep0492Never.as_db_value(),
        );

        // Push surface invariants: no candidate row, no outbox job.
        let candidates = store.count_all_candidates().await.expect("count");
        assert_eq!(
            candidates, 0,
            "T0 <never/> MUST NOT persist a candidate row"
        );
        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "T0 <never/> MUST NOT enqueue a job",
        );

        // Upstream-storage invariant: the inbox witness survives.
        assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;

        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            rendered.contains("waddle_push_suppressed_total{reason=\"xep0492_never\"} 1"),
            "T0 suppression metric must tick exactly once; rendered={rendered}",
        );
    }

    /// XEP-0492 `<on-mention/>` for a non-mention DM is the second
    /// T0 compliance suppressor. Same upstream-preservation contract
    /// as `<never/>`: no candidate row, inbox witness intact.
    #[tokio::test]
    async fn xep0492_on_mention_miss_preserves_pending_delivery_for_non_mention_dm() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let projection = settings_projection().await;
        let room_policy = NoopRoomPolicy;
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");

        let (inbox, witness) =
            seed_inbox_witness(&recipient, &sender, "archive-on-mention-witness", 7, 1).await;

        projection
            .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
                owner_bare_jid: recipient.clone(),
                conversation_jid: sender.clone(),
                conversation_kind:
                    crate::notification_settings_projection::ConversationKind::Direct,
                mode: waddle_xmpp::xep::NotificationLevel::OnMention,
                source:
                    crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: sender.clone(),
                updated_at_ms: 1,
                rich_payload_opt_in: false,
                source_version: 1,
            })
            .await
            .expect("seed on-mention level");

        // `is_mention = false` matches the dispatch path for a plain
        // DM that does NOT name the recipient via XEP-0513.
        let candidate = NotificationCandidate::direct_message(
            recipient.clone(),
            sender_jid,
            StanzaId::new("on-mention-miss-t0", Jid::from(recipient.clone())),
            false,
        )
        .expect("candidate");
        let activity_reader = noop_activity_reader();
        let eval_deps = eval_deps_for_test(&projection, &room_policy, &dnd_reader, activity_reader);
        let (mut room_policy_cache, mut dnd_cache, mut activity_cache) = fresh_eval_caches();
        let mut eval_caches = PushEvalCaches {
            room_policy: &mut room_policy_cache,
            dnd: &mut dnd_cache,
            activity: &mut activity_cache,
        };
        let outcome = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            eval_deps,
            &candidate,
            &mut eval_caches,
        )
        .await
        .expect("t0 eval");
        assert!(
            matches!(
                outcome,
                T1PushDispatchOutcome::Suppressed {
                    reason: SuppressedReason::Xep0492OnMentionMiss,
                }
            ),
            "T0 MUST suppress <on-mention/> miss with typed Xep0492OnMentionMiss; got {outcome:?}"
        );
        waddle_xmpp::prometheus::increment_push_suppressed(
            SuppressedReason::Xep0492OnMentionMiss.as_db_value(),
        );

        assert_eq!(
            store.count_all_candidates().await.expect("count"),
            0,
            "T0 <on-mention/> miss MUST NOT persist a candidate row",
        );
        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "T0 <on-mention/> miss MUST NOT enqueue a job",
        );
        assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;

        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            rendered.contains("waddle_push_suppressed_total{reason=\"xep0492_on_mention_miss\"} 1"),
            "metric counter for xep0492_on_mention_miss must increment; rendered={rendered}",
        );
    }

    /// XEP-0191 blocking suppresses at T1: the candidate row IS
    /// persisted (so the audit row exists), then the drain marks it
    /// outboxed-without-job with `xep0191_blocked`. Upstream storage
    /// (here: pre-existing inbox row) MUST be intact.
    #[tokio::test]
    async fn xep0191_blocked_t1_suppression_keeps_pending_delivery_intact() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");

        let (inbox, witness) =
            seed_inbox_witness(&recipient, &sender, "archive-blocked-witness", 11, 2).await;

        blocking.set_blocklist(recipient.clone(), vec![sender.clone()]);

        let candidate = candidate_for(&recipient, &sender, "blocked-t1");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["blocked-t1"],
            )
            .await
            .expect("query suppressed_reason");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            Some("xep0191_blocked"),
        );
        assert!(
            row.get::<Option<i64>>(1).expect("outboxed").is_some(),
            "T1 suppression must mark candidate outboxed",
        );
        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "T1 XEP-0191 suppression MUST NOT enqueue a job",
        );

        assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;

        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            rendered.contains("waddle_push_suppressed_total{reason=\"xep0191_blocked\"} 1"),
            "metric counter for xep0191_blocked must increment; rendered={rendered}",
        );
    }

    /// XEP-0513 `<noping/>` is a message-frozen hint suppressed at
    /// T1 (per the f898e54c stage-split): the candidate row persists
    /// with the noping bit, then T1 records `xep0513_noping`. Upstream
    /// storage is preserved across this audit-only suppression.
    #[tokio::test]
    async fn xep0513_noping_t1_suppression_persists_candidate_and_keeps_storage() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");

        let (inbox, witness) =
            seed_inbox_witness(&recipient, &sender, "archive-noping-witness", 13, 1).await;

        let candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid,
            StanzaId::new("noping-t1", Jid::from(recipient.clone())),
            true,
            NotificationMessageHints::none().with_noping(true),
        )
        .expect("candidate");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason, outboxed_at_ms, noping FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["noping-t1"],
            )
            .await
            .expect("query suppressed_reason");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            Some("xep0513_noping"),
        );
        assert!(
            row.get::<Option<i64>>(1).expect("outboxed").is_some(),
            "T1 noping suppression must mark candidate outboxed",
        );
        assert_eq!(
            row.get::<i64>(2).expect("noping"),
            1,
            "candidate row must persist the noping hint bit",
        );
        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "T1 XEP-0513 noping suppression MUST NOT enqueue a job",
        );

        assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;

        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            rendered.contains("waddle_push_suppressed_total{reason=\"xep0513_noping\"} 1"),
            "metric counter for xep0513_noping must increment; rendered={rendered}",
        );
    }

    /// #719 regression: a `<no-store/>` candidate is NOT push-suppressed
    /// — the candidate row persists with the no_store bit, T1 records NO
    /// `suppressed_reason`, an outbox job is enqueued, and upstream
    /// storage is untouched. With the default (opt-out) the summary stays
    /// minimal.
    #[tokio::test]
    async fn xep0334_no_store_delivers_minimal_push_and_keeps_storage() {
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");

        let (inbox, witness) =
            seed_inbox_witness(&recipient, &sender, "archive-no-store-witness", 17, 1).await;
        register_push_target(&push_store, &recipient, &target()).await;

        let candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid,
            StanzaId::new("no-store-t1", Jid::from(recipient.clone())),
            false,
            NotificationMessageHints::none().with_xep0334(true, false),
        )
        .expect("candidate")
        .with_last_message_body(Some("secret".to_string()));
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason, outboxed_at_ms, no_store, last_message_body FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["no-store-t1"],
            )
            .await
            .expect("query suppressed_reason");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            None,
            "<no-store/> must not push-suppress under #719",
        );
        assert!(row.get::<Option<i64>>(1).expect("outboxed").is_some());
        assert_eq!(row.get::<i64>(2).expect("no_store"), 1);
        // Off-the-record body was never persisted onto the candidate.
        assert_eq!(row.get::<Option<String>>(3).expect("body"), None);

        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1, "the minimal push must still be enqueued");
        // Default opt-out → minimal summary, no rich fields.
        assert_eq!(jobs[0].rich_summary(), &RichSummary::minimal());

        assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;
    }

    /// #719 parallel of the `<no-store/>` regression for
    /// `<no-permanent-store/>`: delivered, not suppressed, storage
    /// preserved.
    #[tokio::test]
    async fn xep0334_no_permanent_store_delivers_minimal_push_and_keeps_storage() {
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");
        let sender_jid: Jid = "bob@example.com/web".parse().expect("full sender");

        let (inbox, witness) =
            seed_inbox_witness(&recipient, &sender, "archive-no-perm-store-witness", 23, 1).await;
        register_push_target(&push_store, &recipient, &target()).await;

        let candidate = NotificationCandidate::direct_message_with_hints(
            recipient.clone(),
            sender_jid,
            StanzaId::new("no-perm-store-t1", Jid::from(recipient.clone())),
            false,
            NotificationMessageHints::none().with_xep0334(false, true),
        )
        .expect("candidate")
        .with_last_message_body(Some("secret".to_string()));
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason, no_permanent_store, last_message_body FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["no-perm-store-t1"],
            )
            .await
            .expect("query suppressed_reason");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            None,
            "<no-permanent-store/> must not push-suppress under #719",
        );
        assert_eq!(row.get::<i64>(1).expect("no_permanent_store"), 1);
        assert_eq!(row.get::<Option<String>>(2).expect("body"), None);

        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].rich_summary(), &RichSummary::minimal());

        assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;
    }

    /// Waddle DnD suppression at T1 via the `DndReader` trait. Uses
    /// the [`MockPepDndReader`] fixture that mirrors #367's
    /// PEP-backed shape (per-user `Active`/`Inactive` lookup against
    /// persisted state). Upstream inbox witness is preserved across
    /// the DnD-driven audit.
    #[tokio::test]
    async fn waddle_dnd_t1_suppression_persists_audit_and_keeps_storage() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = MockPepDndReader::new();
        let recipient = bare("alice@example.com");
        let sender = bare("bob@example.com");
        dnd_reader.set_active(recipient.clone());

        let (inbox, witness) =
            seed_inbox_witness(&recipient, &sender, "archive-dnd-witness", 29, 5).await;

        let candidate = candidate_for(&recipient, &sender, "dnd-t1");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["dnd-t1"],
            )
            .await
            .expect("query suppressed_reason");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            Some("waddle_dnd"),
        );
        assert!(row.get::<Option<i64>>(1).expect("outboxed").is_some());
        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "T1 Waddle DnD suppression MUST NOT enqueue a job",
        );

        assert_inbox_witness_unchanged(&inbox, &recipient, &witness).await;

        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            rendered.contains("waddle_push_suppressed_total{reason=\"waddle_dnd\"} 1"),
            "metric counter for waddle_dnd must increment; rendered={rendered}",
        );
    }

    /// Integration shape that #367 will fulfill: the real
    /// `urn:waddle:dnd:0` PEP-backed `DndReader` is queried per-user
    /// at T1 with the recipient's `BareJid`, and the typed
    /// `DndState::Active` / `Inactive` outcome decides suppression.
    /// This test exercises the contract with [`MockPepDndReader`]
    /// (a per-user persisted set of "active" recipients) — once
    /// #367 ships, only the reader implementation swaps; the trait
    /// surface this test pins is locked in slice 2a.
    ///
    /// Scenario: two DM candidates drain in one batch — Alice (DnD
    /// Active) MUST be suppressed with `waddle_dnd`, Bob (DnD
    /// Inactive) MUST be delivered through to a job. Metric counter
    /// MUST tick by exactly one (Alice's row only).
    #[tokio::test]
    async fn dnd_integration_with_pep_shaped_reader_suppresses_push_only() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = MockPepDndReader::new();
        let alice = bare("alice@example.com");
        let bob = bare("bob@example.com");
        let carol = bare("carol@example.com");
        let push_service_jid = bare("push.example.com");

        // Mirrors #367: Alice has a `urn:waddle:dnd:0` PEP item set
        // (modelled here as membership in the active_users set);
        // Bob does not.
        dnd_reader.set_active(alice.clone());

        // Register a push device for each recipient so the
        // non-suppressed candidate can enqueue a real outbox job
        // (proves the suppression scope is per-recipient, not global).
        for recipient in [&alice, &bob] {
            push_store
                .register(waddle_xmpp::push::PushSubscription {
                    user_jid: recipient.to_string(),
                    service_jid: push_service_jid.to_string(),
                    node: Some(format!("{recipient}-node")),
                    publish_options: None,
                    endpoint: None,
                    p256dh: None,
                    auth_key: None,
                })
                .await
                .expect("register push subscription");
        }

        let alice_candidate = candidate_for(&alice, &carol, "dnd-integration-alice");
        let bob_candidate = candidate_for(&bob, &carol, "dnd-integration-bob");
        store
            .insert_candidate(&alice_candidate)
            .await
            .expect("insert alice candidate");
        store
            .insert_candidate(&bob_candidate)
            .await
            .expect("insert bob candidate");

        let deps = drain_deps_with_noop_activity(&room_policy, &dnd_reader, noop_activity_reader());
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &push_service_jid,
                16,
            )
            .await
            .expect("drain candidates");

        // Alice's candidate is suppressed with the typed audit.
        let mut alice_rows = store
            .query(
                "SELECT suppressed_reason, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["dnd-integration-alice"],
            )
            .await
            .expect("query alice");
        let alice_row = alice_rows
            .next()
            .await
            .expect("alice row")
            .expect("alice exists");
        assert_eq!(
            alice_row
                .get::<Option<String>>(0)
                .expect("alice reason")
                .as_deref(),
            Some("waddle_dnd"),
            "Alice (DnD Active) MUST be suppressed with the typed waddle_dnd audit",
        );
        assert!(
            alice_row
                .get::<Option<i64>>(1)
                .expect("alice outboxed")
                .is_some(),
            "Alice's candidate must be marked outboxed-without-job",
        );

        // Bob's candidate is delivered through to a real outbox job.
        let mut bob_rows = store
            .query(
                "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["dnd-integration-bob"],
            )
            .await
            .expect("query bob");
        let bob_row = bob_rows.next().await.expect("bob row").expect("bob exists");
        assert!(
            bob_row
                .get::<Option<String>>(0)
                .expect("bob reason")
                .is_none(),
            "Bob (DnD Inactive) MUST NOT be suppressed",
        );

        let jobs = store
            .pending_outbox_jobs()
            .await
            .expect("pending outbox jobs");
        assert_eq!(
            jobs.len(),
            1,
            "exactly one outbox job — Bob's. Alice's DnD suppression MUST be per-recipient",
        );
        assert_eq!(
            jobs[0].recipient_bare_jid(),
            &bob,
            "the surviving job belongs to Bob",
        );

        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            rendered.contains("waddle_push_suppressed_total{reason=\"waddle_dnd\"} 1"),
            "metric for waddle_dnd must increment by exactly 1 (Alice only); rendered={rendered}",
        );
    }

    // ─────────────────────────────────────────────────────────────
    // Slice 2b — `notification_activity` projection + XEP-0513
    // `<active/>` push filter (#526).
    // ─────────────────────────────────────────────────────────────

    /// Builds an [`ActiveChannelMention`] candidate for the given
    /// (recipient, room, sender) triple — slice 2b's gate operates
    /// exclusively on this class.
    fn active_channel_mention_candidate_for(
        recipient: &BareJid,
        room: &BareJid,
        sender: &BareJid,
        id: &str,
    ) -> NotificationCandidate {
        groupchat_candidate_for(
            recipient,
            room,
            format!("{room}/{}", sender.node().expect("sender node"))
                .parse()
                .expect("sender occupant jid"),
            id,
            NotificationClass::ActiveChannelMention,
        )
    }

    async fn activity_store() -> crate::notification_activity::NotificationActivityStore {
        crate::notification_activity::NotificationActivityStore::new(
            Database::in_memory("notification-activity-eval")
                .await
                .expect("activity db"),
        )
        .await
        .expect("activity store")
    }

    /// A [`NotificationActivityReader`] test double that counts
    /// per-`(owner, conversation)` read calls so the slice 2b T0/T1
    /// stage-split and per-batch cache can be asserted.
    struct CountingActivityReader {
        inner: crate::notification_activity::NotificationActivityStore,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl CountingActivityReader {
        async fn new() -> Self {
            Self {
                inner: activity_store().await,
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl NotificationActivityReader for CountingActivityReader {
        async fn read_activity(
            &self,
            owner: &BareJid,
            conversation: &BareJid,
        ) -> Result<Option<NotificationActivity>, NotificationActivityError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.read_activity(owner, conversation).await
        }
    }

    /// XEP-0513 hit: recipient was active within the TTL window →
    /// `ActiveChannelMention` candidate MUST deliver. Seeds the
    /// activity projection with a `last_active_at_ms = now()` row
    /// and asserts the T1 drain enqueues the push job.
    #[tokio::test]
    async fn t1_active_channel_mention_with_recent_activity_delivers() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let activity_reader = activity_store().await;

        let recipient = bare("alice@example.com");
        let room = bare("room@muc.example.com");
        let sender = bare("bob@example.com");
        register_push_target(&push_store, &recipient, &target).await;

        // Record recent activity for the recipient on this room —
        // the chat-state ingest mirrors a fresh XEP-0085 update.
        activity_reader
            .record_chat_state(
                &recipient,
                &room,
                crate::notification_activity::NotificationChatState::Active,
                crate::time::now_ms(),
            )
            .await
            .expect("seed activity");

        let candidate =
            active_channel_mention_candidate_for(&recipient, &room, &sender, "active-hit");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &activity_reader);
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(
            jobs.len(),
            1,
            "active recipient within TTL MUST receive the push",
        );
        assert_eq!(jobs[0].class(), NotificationClass::ActiveChannelMention,);
    }

    /// Clock-skew regression: a `last_active_at_ms` value stamped in
    /// the FUTURE relative to the evaluator's `now_ms` (NTP drift,
    /// replica clock skew, ingestion path using a writer with a
    /// faster wall clock) MUST NOT silently extend the configured
    /// TTL window. The evaluator clamps the stored timestamp to
    /// `now_ms` so `age` stays non-negative — a future-stamped row
    /// is treated as "active at now" and ages from there. Without
    /// the clamp, the unsigned-style `age <= TTL` predicate would
    /// silently treat any future timestamp as active until the
    /// wall clock caught up, even past the TTL.
    #[tokio::test]
    async fn t1_active_channel_mention_future_timestamp_does_not_extend_ttl_window() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let target = target();
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let activity_reader = activity_store().await;

        let recipient = bare("alice@example.com");
        let room = bare("room-future@muc.example.com");
        let sender = bare("bob@example.com");
        register_push_target(&push_store, &recipient, &target).await;

        // Stamp activity at `now_ms + 1h` — a pathological future
        // timestamp from a skewed writer clock. The candidate is
        // emitted at the evaluator's `now_ms`; without the clamp the
        // raw `now - last_active` would be hugely negative and the
        // `<= TTL` predicate would fire as "active". With the clamp
        // it normalizes to `age = 0 <= TTL`, which also delivers —
        // but that's the desired outcome: a fresh-looking (clamped-
        // to-now) activity row is correctly treated as active.
        let future_ms = crate::time::now_ms().saturating_add(3_600_000);
        activity_reader
            .record_chat_state(
                &recipient,
                &room,
                crate::notification_activity::NotificationChatState::Active,
                future_ms,
            )
            .await
            .expect("seed future-stamped activity");

        let candidate =
            active_channel_mention_candidate_for(&recipient, &room, &sender, "future-clamp");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &activity_reader);
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        // Behavior we lock: a future timestamp is clamped to `now_ms`
        // and the evaluator treats it as fresh activity → delivery.
        // The clamp's protective value isn't the immediate outcome
        // (both clamped-fresh and unclamped-negative would deliver
        // under `<= TTL`); it's that the predicate operates on a
        // non-negative `age`, so future signed-integer refactors
        // can't silently re-introduce a "negative age = always
        // active" bug.
        let jobs = store.pending_outbox_jobs().await.expect("jobs");
        assert_eq!(
            jobs.len(),
            1,
            "future-stamped activity must clamp to now and deliver as fresh, not produce a negative age",
        );
        // Verify the row passes the gate (no suppression metric ticked).
        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            !rendered.contains("waddle_push_suppressed_total{reason=\"xep0513_active_miss\"} 1"),
            "future-stamped activity must not suppress with Xep0513ActiveMiss",
        );
    }

    /// XEP-0513 miss (stale): recipient's last activity is older than
    /// the configured TTL → suppress with `Xep0513ActiveMiss`. Also
    /// asserts the audit column persists and the metric ticks.
    #[tokio::test]
    async fn t1_active_channel_mention_with_stale_activity_suppresses_with_xep0513_active_miss() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let activity_reader = activity_store().await;

        let recipient = bare("alice@example.com");
        let room = bare("room-stale@muc.example.com");
        let sender = bare("bob@example.com");

        // Stale activity: 1 hour ago, well outside the default 5min
        // TTL window the evaluator clamps to.
        let now_ms = crate::time::now_ms();
        let stale_ms = now_ms.saturating_sub(60 * 60 * 1_000);
        activity_reader
            .record_outbound_message(&recipient, &room, stale_ms)
            .await
            .expect("seed stale");

        let candidate =
            active_channel_mention_candidate_for(&recipient, &room, &sender, "active-stale");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &activity_reader);
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["active-stale"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            Some("xep0513_active_miss"),
        );
        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "T1 XEP-0513 miss MUST NOT enqueue a job",
        );
        let rendered = waddle_xmpp::prometheus::render_metrics();
        assert!(
            rendered.contains("waddle_push_suppressed_total{reason=\"xep0513_active_miss\"} 1"),
            "metric for xep0513_active_miss must increment; rendered={rendered}",
        );
    }

    /// XEP-0513 miss (no row): recipient has never recorded any
    /// activity on the conversation → suppress with
    /// `Xep0513ActiveMiss` (no row in the projection is treated the
    /// same as "stale activity").
    #[tokio::test]
    async fn t1_active_channel_mention_with_no_activity_record_suppresses() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let activity_reader = activity_store().await;

        let recipient = bare("alice@example.com");
        let room = bare("room-never@muc.example.com");
        let sender = bare("bob@example.com");

        let candidate =
            active_channel_mention_candidate_for(&recipient, &room, &sender, "active-never");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &activity_reader);
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        let mut rows = store
            .query(
                "SELECT suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["active-never"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            Some("xep0513_active_miss"),
        );
    }

    /// Stage-split contract: at `PushEvalStage::T0Emit` the XEP-0513
    /// `<active/>` filter MUST NOT consult the activity reader.
    /// Exercises the stage split with a counting reader fixture.
    #[tokio::test]
    async fn t0_active_channel_mention_does_not_consult_activity_reader() {
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let counting = CountingActivityReader::new().await;

        let recipient = bare("alice@example.com");
        let room = bare("room@muc.example.com");
        let sender = bare("bob@example.com");
        let candidate =
            active_channel_mention_candidate_for(&recipient, &room, &sender, "t0-no-touch");

        let eval_deps = eval_deps_for_test(&projection, &room_policy, &dnd_reader, &counting);
        let (mut room_policy_cache, mut dnd_cache, mut activity_cache) = fresh_eval_caches();
        let mut eval_caches = PushEvalCaches {
            room_policy: &mut room_policy_cache,
            dnd: &mut dnd_cache,
            activity: &mut activity_cache,
        };

        let outcome = evaluate_push_gate_at_dispatch(
            PushEvalStage::T0Emit,
            eval_deps,
            &candidate,
            &mut eval_caches,
        )
        .await
        .expect("t0 eval");
        // T0Emit must NOT suppress on the XEP-0513 filter (skipped at
        // T0). Class falls through to the XEP-0492 evaluator with the
        // public-group `OnMention` default → mention bit on
        // `ActiveChannelMention` is `true`, so the gate Delivers.
        assert!(
            matches!(outcome, T1PushDispatchOutcome::Deliver { .. }),
            "T0Emit MUST NOT suppress with Xep0513ActiveMiss; got {outcome:?}"
        );
        assert_eq!(
            counting.call_count(),
            0,
            "T0Emit MUST NOT consult the activity reader",
        );

        // Same candidate at T1Drain DOES consult the reader.
        let outcome_t1 = evaluate_push_gate_at_dispatch(
            PushEvalStage::T1Drain,
            eval_deps,
            &candidate,
            &mut eval_caches,
        )
        .await
        .expect("t1 eval");
        assert!(
            matches!(
                outcome_t1,
                T1PushDispatchOutcome::Suppressed {
                    reason: SuppressedReason::Xep0513ActiveMiss
                }
            ),
            "T1Drain MUST suppress when activity is missing; got {outcome_t1:?}"
        );
        assert_eq!(
            counting.call_count(),
            1,
            "T1Drain MUST consult the activity reader exactly once",
        );
    }

    /// Storage-preservation regression mirroring slice 2a: a T1
    /// XEP-0513 `<active/>` miss MUST persist the typed audit reason
    /// onto the candidate row and MUST NOT touch upstream storage
    /// (inbox, MAM, pending delivery).
    #[tokio::test]
    async fn xep0513_active_miss_t1_suppression_persists_audit_and_keeps_storage() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let activity_reader = activity_store().await;

        let recipient = bare("alice@example.com");
        let partner = bare("bob@example.com");
        let room = bare("room-storage@muc.example.com");
        // Seed an inbox row so we can witness it's untouched after
        // the T1 suppression. The recipient/partner pairing on the
        // inbox witness is intentionally independent of the
        // ActiveChannelMention candidate's room — both must survive
        // identically since push suppression touches neither.
        let (inbox_storage, inbox_witness) =
            seed_inbox_witness(&recipient, &partner, "witness-stanza", 7_000, 3).await;

        let sender = bare("bob@example.com");
        let candidate =
            active_channel_mention_candidate_for(&recipient, &room, &sender, "active-storage");
        store
            .insert_candidate(&candidate)
            .await
            .expect("insert candidate");

        let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &activity_reader);
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        // Audit column written.
        let mut rows = store
            .query(
                "SELECT suppressed_reason, outboxed_at_ms FROM notification_candidates WHERE stanza_id = ?",
                crate::db_params!["active-storage"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row exists");
        assert_eq!(
            row.get::<Option<String>>(0).expect("reason").as_deref(),
            Some("xep0513_active_miss"),
        );
        assert!(
            row.get::<Option<i64>>(1).expect("outboxed_at_ms").is_some(),
            "suppressed candidate MUST be marked outboxed",
        );
        assert!(
            store.pending_outbox_jobs().await.expect("jobs").is_empty(),
            "no push job MUST be enqueued",
        );

        // Inbox witness untouched.
        use waddle_xmpp::inbox::storage::InboxStorage;
        let after = inbox_storage
            .list(&recipient)
            .await
            .expect("list inbox after T1 suppression");
        assert_eq!(after.len(), 1, "inbox row count MUST be unchanged");
        assert_eq!(
            after[0].last_stanza_id, inbox_witness.last_stanza_id,
            "inbox last_stanza_id MUST be unchanged by push suppression",
        );
        assert_eq!(
            after[0].unread, inbox_witness.unread,
            "inbox unread MUST be unchanged by push suppression",
        );
    }

    /// Per-batch cache: multiple ActiveChannelMention candidates for
    /// the same (recipient, conversation) MUST trigger exactly one
    /// activity-reader call. Exercises the cache-population path in
    /// `resolve_cached_activity`.
    #[tokio::test]
    async fn t1_active_channel_mention_cache_collapses_same_recipient_lookups() {
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();
        let store = store().await;
        let push_store = waddle_xmpp::push::InMemoryPushStore::new();
        let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
        let projection = settings_projection().await;
        let room_policy = StubRoomPolicy::new();
        let dnd_reader = NoopDndReader;
        let counting = CountingActivityReader::new().await;

        let recipient = bare("alice@example.com");
        let room = bare("room-cache@muc.example.com");
        let _sender = bare("bob@example.com");

        // Seed activity so the candidates all pass the gate — the
        // assertion is purely about call-count economy, so the
        // outcome doesn't matter as long as the reader gets consulted.
        counting
            .inner
            .record_outbound_message(&recipient, &room, crate::time::now_ms())
            .await
            .expect("seed activity");

        for (idx, stanza_id) in ["cache-1", "cache-2", "cache-3"].iter().enumerate() {
            // Sender must be `<room>/<nick>` per
            // `NotificationCandidate::groupchat`'s `SenderConversationMismatch`
            // guard — groupchat candidates carry the occupant JID, not
            // the raw user JID.
            let candidate = NotificationCandidate::groupchat(
                recipient.clone(),
                room.clone(),
                format!("{room}/bob-conn-{idx}")
                    .parse()
                    .expect("occupant jid"),
                NotificationThreadId::root(),
                StanzaId::new(stanza_id.to_string(), Jid::from(room.clone())),
                NotificationClass::ActiveChannelMention,
            )
            .expect("candidate");
            store.insert_candidate(&candidate).await.expect("insert");
        }

        let deps = NotificationDrainDeps::new(&room_policy, &dnd_reader, &counting);
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                deps,
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates");

        assert_eq!(
            counting.call_count(),
            1,
            "per-batch activity cache MUST collapse repeats for the same (owner, conversation)",
        );
    }

    /// XEP-0085 ingestion: a writer call persists the typed chat
    /// state and is readable via the projection store's reader trait.
    /// Per CLAUDE.md per-XEP test discipline.
    #[tokio::test]
    async fn xep0085_chat_state_writer_persists_typed_token() {
        let store = activity_store().await;
        let owner = bare("alice@example.com");
        let conversation = bare("room@muc.example.com");
        store
            .record_chat_state(
                &owner,
                &conversation,
                crate::notification_activity::NotificationChatState::Composing,
                42,
            )
            .await
            .expect("record chat-state");
        let activity = store
            .read_activity(&owner, &conversation)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(activity.last_active_at_ms, 42);
        assert_eq!(
            activity.last_chat_state,
            Some(crate::notification_activity::NotificationChatState::Composing),
        );
    }

    /// XEP-0490 ingestion: a read-marker writer persists the typed
    /// last_read_at_ms timestamp and updates `last_active_at_ms`.
    /// Per CLAUDE.md per-XEP test discipline.
    #[tokio::test]
    async fn xep0490_read_marker_writer_persists_typed_timestamp() {
        let store = activity_store().await;
        let owner = bare("alice@example.com");
        let conversation = bare("room@muc.example.com");
        store
            .record_read_marker(&owner, &conversation, 11_000)
            .await
            .expect("record marker");
        let activity = store
            .read_activity(&owner, &conversation)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(activity.last_active_at_ms, 11_000);
        assert_eq!(activity.last_read_at_ms, Some(11_000));
    }

    /// Outbound message commit: writer call updates the sender's
    /// activity row for the conversation. Per CLAUDE.md per-XEP test
    /// discipline.
    #[tokio::test]
    async fn outbound_message_writer_persists_activity_for_sender() {
        let store = activity_store().await;
        let owner = bare("alice@example.com");
        let conversation = bare("bob@example.com");
        store
            .record_outbound_message(&owner, &conversation, 9_999)
            .await
            .expect("record outbound");
        let activity = store
            .read_activity(&owner, &conversation)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(activity.last_active_at_ms, 9_999);
    }

    /// XEP-0045 ingestion: presence available + unavailable both bump
    /// `last_active_at_ms`; the show is preserved on available and
    /// cleared on unavailable. Per CLAUDE.md per-XEP test discipline.
    #[tokio::test]
    async fn xep0045_presence_writer_persists_show_and_clears_on_unavailable() {
        let store = activity_store().await;
        let owner = bare("alice@example.com");
        let room = bare("room@muc.example.com");
        store
            .record_presence_available(
                &owner,
                &room,
                Some(crate::notification_activity::NotificationPresenceShow::Dnd),
                1_000,
            )
            .await
            .expect("available");
        let after_available = store
            .read_activity(&owner, &room)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(after_available.last_active_at_ms, 1_000);
        assert_eq!(
            after_available.presence_show,
            Some(crate::notification_activity::NotificationPresenceShow::Dnd)
        );

        store
            .record_presence_unavailable(&owner, &room, 2_000)
            .await
            .expect("unavailable");
        let after_unavailable = store
            .read_activity(&owner, &room)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(after_unavailable.last_active_at_ms, 2_000);
        assert!(after_unavailable.presence_show.is_none());
    }

    /// Operator-tunable TTL: env-driven helper clamps to the
    /// [`MIN_ACTIVE_MENTION_TTL_SECONDS`,
    /// `MAX_ACTIVE_MENTION_TTL_SECONDS`] window and falls back to
    /// the default on unparseable input. Tests via direct env
    /// manipulation; serialized against other env-mutating tests in
    /// this module via [`env_lock`] (Codex review on PR #731).
    #[test]
    fn active_mention_ttl_env_var_clamps_to_window() {
        // SAFETY: `env_lock` serializes against every other
        // env-mutating test in this module; `std::env::set_var` is
        // process-global but no other thread will read this var while
        // the guard is held.
        let _guard = env_lock();
        // Save and restore the operator-set value (if any) so the
        // test is a no-op for the parent environment.
        let previous = std::env::var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV).ok();
        unsafe { std::env::remove_var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV) };
        assert_eq!(
            active_mention_ttl_ms_from_env(),
            (DEFAULT_ACTIVE_MENTION_TTL_SECONDS as i64) * 1_000,
        );

        unsafe { std::env::set_var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV, "0") };
        assert_eq!(
            active_mention_ttl_ms_from_env(),
            (MIN_ACTIVE_MENTION_TTL_SECONDS as i64) * 1_000,
        );

        unsafe { std::env::set_var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV, "999999999") };
        assert_eq!(
            active_mention_ttl_ms_from_env(),
            (MAX_ACTIVE_MENTION_TTL_SECONDS as i64) * 1_000,
        );

        match previous {
            Some(value) => unsafe { std::env::set_var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV, value) },
            None => unsafe { std::env::remove_var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV) },
        }
    }
}
