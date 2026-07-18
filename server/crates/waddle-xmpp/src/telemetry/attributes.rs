//! The metric-attribute allowlist (the cardinality budget's teeth).
//!
//! Every metric attribute is a closed enum here implementing the
//! sealed [`MetricAttribute`] trait. The macros in
//! [`crate::telemetry`] accept only these types, so an unbounded
//! value — a JID, a room JID, a stream id, a message id, user input —
//! cannot become a metric attribute by construction. Adding a new
//! attribute key or value means editing this file, where review can
//! hold the cardinality line.

use opentelemetry::KeyValue;

mod sealed {
    pub trait Sealed {}
}

/// A typed metric attribute: a static key and a value drawn from a
/// closed enum. Sealed — only the allowlist in this module implements
/// it.
pub trait MetricAttribute: sealed::Sealed {
    /// The attribute key, e.g. `"kind"`.
    fn key(&self) -> &'static str;
    /// The enumerated attribute value, e.g. `"muc_pm"`.
    fn value(&self) -> &'static str;
    /// The OTel key/value pair for this attribute.
    fn key_value(&self) -> KeyValue {
        KeyValue::new(self.key(), self.value())
    }
}

/// `kind` — what flavor of message traffic a sample counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// One-to-one chat message.
    Dm,
    /// Groupchat message fanned out through a MUC room.
    Muc,
    /// Private message addressed to a single MUC occupant.
    MucPm,
}

impl sealed::Sealed for MessageKind {}
impl MetricAttribute for MessageKind {
    fn key(&self) -> &'static str {
        "kind"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::Dm => "dm",
            Self::Muc => "muc",
            Self::MucPm => "muc_pm",
        }
    }
}

/// `janitor` — which background sweep loop a sample belongs to.
/// One variant per janitor loop in `waddle-server`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Janitor {
    /// Expired detached XEP-0198 stream-management sessions.
    SmExpiry,
    /// Orphaned SM/room claims after node loss (clustering).
    OrphanReaper,
    /// Orphaned/aged-out pending-delivery claims.
    PendingDeliveryClaim,
    /// XEP-0357 push publish-job queue.
    PushPublishJob,
    /// Notification outbox drain/retry/dead-letter.
    NotificationOutbox,
    /// Expired OAuth/device auth state.
    AuthState,
    /// Dormant MUC room eviction.
    RoomDormancy,
    /// Empty user-actor reaping.
    UserActorReaper,
    /// Remote MUC membership reconciliation (clustering).
    RemoteMucMembership,
}

impl sealed::Sealed for Janitor {}
impl MetricAttribute for Janitor {
    fn key(&self) -> &'static str {
        "janitor"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::SmExpiry => "sm_expiry",
            Self::OrphanReaper => "orphan_reaper",
            Self::PendingDeliveryClaim => "pending_delivery_claim",
            Self::PushPublishJob => "push_publish_job",
            Self::NotificationOutbox => "notification_outbox",
            Self::AuthState => "auth_state",
            Self::RoomDormancy => "room_dormancy",
            Self::UserActorReaper => "user_actor_reaper",
            Self::RemoteMucMembership => "remote_muc_membership",
        }
    }
}

/// `outcome` — how a janitor sweep (or comparable bounded operation)
/// ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepOutcome {
    /// The sweep ran to completion, including zero-work sweeps.
    Completed,
    /// The sweep aborted on an error.
    Failed,
}

impl sealed::Sealed for SweepOutcome {}
impl MetricAttribute for SweepOutcome {
    fn key(&self) -> &'static str {
        "outcome"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// `reason` — why an XEP-0357 push notification was suppressed.
///
/// This is the metric-facing closed set. `waddle-server` converts its
/// persisted `SuppressedReason` audit value into this enum before emitting,
/// so metric labels never cross the crate boundary as strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushSuppressReason {
    /// The notification would target its sender.
    Xep0357Self,
    /// The recipient has no push registration.
    Xep0357NoRegistration,
    /// The recipient's push registration is disabled.
    Xep0357RegistrationDisabled,
    /// XEP-0492 resolved to `<never/>`.
    Xep0492Never,
    /// XEP-0492 `<on-mention/>` did not match.
    Xep0492OnMentionMiss,
    /// XEP-0191 blocked the sender or conversation.
    Xep0191Blocked,
    /// XEP-0513 `<noping/>` opted out of notification.
    Xep0513Noping,
    /// XEP-0513 `<active/>` did not match.
    Xep0513ActiveMiss,
    /// Waddle DND is active for the recipient.
    WaddleDnd,
    /// The downstream push provider rejected the notification.
    ProviderRejected,
    /// The downstream push provider reported an expired token.
    ProviderTokenExpired,
    /// The XEP-0357 push service is degraded.
    Xep0357PushServiceDegraded,
    /// The unread count reached zero before publish.
    UnreadZeroAtPublish,
    /// Policy evaluation exhausted its retry budget.
    PolicyRetriesExhausted,
    /// The message is an XEP-0444 reaction-only notification.
    Xep0444Reaction,
}

impl PushSuppressReason {
    /// Every allowed value, in stable legacy-renderer order.
    pub const ALL: [Self; 15] = [
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

    /// Every allowed label value, in stable legacy-renderer order.
    pub const VALUES: [&'static str; 15] = [
        Self::Xep0357Self.as_str(),
        Self::Xep0357NoRegistration.as_str(),
        Self::Xep0357RegistrationDisabled.as_str(),
        Self::Xep0492Never.as_str(),
        Self::Xep0492OnMentionMiss.as_str(),
        Self::Xep0191Blocked.as_str(),
        Self::Xep0513Noping.as_str(),
        Self::Xep0513ActiveMiss.as_str(),
        Self::WaddleDnd.as_str(),
        Self::ProviderRejected.as_str(),
        Self::ProviderTokenExpired.as_str(),
        Self::Xep0357PushServiceDegraded.as_str(),
        Self::UnreadZeroAtPublish.as_str(),
        Self::PolicyRetriesExhausted.as_str(),
        Self::Xep0444Reaction.as_str(),
    ];

    /// The byte-stable legacy label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
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

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Xep0357Self => 0,
            Self::Xep0357NoRegistration => 1,
            Self::Xep0357RegistrationDisabled => 2,
            Self::Xep0492Never => 3,
            Self::Xep0492OnMentionMiss => 4,
            Self::Xep0191Blocked => 5,
            Self::Xep0513Noping => 6,
            Self::Xep0513ActiveMiss => 7,
            Self::WaddleDnd => 8,
            Self::ProviderRejected => 9,
            Self::ProviderTokenExpired => 10,
            Self::Xep0357PushServiceDegraded => 11,
            Self::UnreadZeroAtPublish => 12,
            Self::PolicyRetriesExhausted => 13,
            Self::Xep0444Reaction => 14,
        }
    }
}

impl sealed::Sealed for PushSuppressReason {}
impl MetricAttribute for PushSuppressReason {
    fn key(&self) -> &'static str {
        "reason"
    }

    fn value(&self) -> &'static str {
        self.as_str()
    }
}

/// `condition` — the RFC 6120 §8.3.3 defined stanza-error conditions,
/// reusing the crate's existing typed enum (its `as_str()` already
/// yields the hyphenated wire names). Re-exported here so metric call
/// sites can name it through the allowlist module.
pub use crate::error::StanzaErrorCondition;

impl sealed::Sealed for StanzaErrorCondition {}
impl MetricAttribute for StanzaErrorCondition {
    fn key(&self) -> &'static str {
        "condition"
    }
    fn value(&self) -> &'static str {
        self.as_str()
    }
}
