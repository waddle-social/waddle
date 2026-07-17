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

/// `condition` — the RFC 6120 §8.3.3 defined stanza-error conditions.
/// This is the complete allowlist; there is deliberately no
/// catch-all-with-string variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StanzaErrorCondition {
    BadRequest,
    Conflict,
    FeatureNotImplemented,
    Forbidden,
    Gone,
    InternalServerError,
    ItemNotFound,
    JidMalformed,
    NotAcceptable,
    NotAllowed,
    NotAuthorized,
    PolicyViolation,
    RecipientUnavailable,
    Redirect,
    RegistrationRequired,
    RemoteServerNotFound,
    RemoteServerTimeout,
    ResourceConstraint,
    ServiceUnavailable,
    SubscriptionRequired,
    UndefinedCondition,
    UnexpectedRequest,
}

impl sealed::Sealed for StanzaErrorCondition {}
impl MetricAttribute for StanzaErrorCondition {
    fn key(&self) -> &'static str {
        "condition"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::BadRequest => "bad-request",
            Self::Conflict => "conflict",
            Self::FeatureNotImplemented => "feature-not-implemented",
            Self::Forbidden => "forbidden",
            Self::Gone => "gone",
            Self::InternalServerError => "internal-server-error",
            Self::ItemNotFound => "item-not-found",
            Self::JidMalformed => "jid-malformed",
            Self::NotAcceptable => "not-acceptable",
            Self::NotAllowed => "not-allowed",
            Self::NotAuthorized => "not-authorized",
            Self::PolicyViolation => "policy-violation",
            Self::RecipientUnavailable => "recipient-unavailable",
            Self::Redirect => "redirect",
            Self::RegistrationRequired => "registration-required",
            Self::RemoteServerNotFound => "remote-server-not-found",
            Self::RemoteServerTimeout => "remote-server-timeout",
            Self::ResourceConstraint => "resource-constraint",
            Self::ServiceUnavailable => "service-unavailable",
            Self::SubscriptionRequired => "subscription-required",
            Self::UndefinedCondition => "undefined-condition",
            Self::UnexpectedRequest => "unexpected-request",
        }
    }
}
