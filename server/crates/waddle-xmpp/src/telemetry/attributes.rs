//! The metric-attribute allowlist (the cardinality budget's teeth).
//!
//! Every metric attribute is a closed enum here implementing the
//! sealed [`MetricAttribute`] trait. The macros in
//! [`crate::telemetry`] accept only these types, so an unbounded
//! value — a JID, a room JID, a stream id, a message id, user input —
//! cannot become a metric attribute by construction. Adding a new
//! attribute key or value means editing this file, where review can
//! hold the cardinality line.
//! [`crate::metrics`] is a documented pre-existing exception pending migration under #1330.

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

/// `reason` — why an XEP-0357 push outbox job was scheduled for retry.
///
/// The legacy text family only ever rendered
/// `waddle_push_outbox_retry_scheduled_total{reason="unknown"}`, so the
/// OTel successor carries the same closed label shape through the alias
/// cutover. Gains real variants if the outbox ever classifies retry
/// causes; until then `Unknown` is the whole set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushRetryReason {
    /// The retry cause was not classified.
    Unknown,
}

impl PushRetryReason {
    /// Every allowed value. Startup zero-registration (#1436) iterates
    /// this so every label value exists before the first real retry.
    pub const ALL: [Self; 1] = [Self::Unknown];
}

impl sealed::Sealed for PushRetryReason {}
impl MetricAttribute for PushRetryReason {
    fn key(&self) -> &'static str {
        "reason"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
        }
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

/// `route` — an HTTP route **template** from the router table
/// (`/api/auth/token`, `/api/channels/{id}`), never a raw request
/// path. The value space is bounded by the set of `&'static str`
/// templates in the router definition; constructing one from anything
/// but a router-table literal is a review error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpRouteTemplate(&'static str);

impl HttpRouteTemplate {
    /// Wrap a router-table route template. `template` MUST be the
    /// matched route pattern registered with the router, not a
    /// request path.
    #[must_use]
    pub const fn new(template: &'static str) -> Self {
        Self(template)
    }
}

impl sealed::Sealed for HttpRouteTemplate {}
impl MetricAttribute for HttpRouteTemplate {
    fn key(&self) -> &'static str {
        "route"
    }
    fn value(&self) -> &'static str {
        self.0
    }
}

/// `status_class` — HTTP response status bucketed to its class, so
/// the status dimension is exactly five values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpStatusClass {
    Informational,
    Success,
    Redirection,
    ClientError,
    ServerError,
}

impl HttpStatusClass {
    /// Bucket a numeric status code. Out-of-range codes collapse into
    /// the nearest class boundary (sub-100 → 1xx, 600+ → 5xx) so the
    /// label set stays closed.
    #[must_use]
    pub const fn from_status(status: u16) -> Self {
        match status {
            0..=199 => Self::Informational,
            200..=299 => Self::Success,
            300..=399 => Self::Redirection,
            400..=499 => Self::ClientError,
            _ => Self::ServerError,
        }
    }
}

impl sealed::Sealed for HttpStatusClass {}
impl MetricAttribute for HttpStatusClass {
    fn key(&self) -> &'static str {
        "status_class"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::Informational => "1xx",
            Self::Success => "2xx",
            Self::Redirection => "3xx",
            Self::ClientError => "4xx",
            Self::ServerError => "5xx",
        }
    }
}

/// `stage` — which step of the authentication surface a sample
/// belongs to (#1328). One variant per rejection choke point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStage {
    /// OIDC authorization start / redirect handling.
    OidcAuthorization,
    /// OIDC IdP callback (code + state redemption).
    OidcCallback,
    /// OAuth token exchange (`/api/auth/token`, XMPP code exchange).
    TokenExchange,
    /// OIDC userinfo fetch.
    Userinfo,
    /// State / CSRF validation.
    State,
    /// OAuth device flow (verify / poll / approve).
    DeviceFlow,
    /// Native XEP/SASL SCRAM authentication.
    Scram,
}

impl sealed::Sealed for AuthStage {}
impl MetricAttribute for AuthStage {
    fn key(&self) -> &'static str {
        "stage"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::OidcAuthorization => "oidc_authorization",
            Self::OidcCallback => "oidc_callback",
            Self::TokenExchange => "token_exchange",
            Self::Userinfo => "userinfo",
            Self::State => "state",
            Self::DeviceFlow => "device_flow",
            Self::Scram => "scram",
        }
    }
}

/// `error_code` — the enumerated authentication failure classes
/// (#1328). A closed metric-facing set: auth code maps its concrete
/// errors into these buckets before emitting, so an IdP's free-form
/// error string can never become a label value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthErrorCode {
    /// Unknown, reused, or expired `state` (CSRF mismatch included).
    InvalidState,
    /// Authorization code missing, unknown, or already redeemed.
    InvalidCode,
    /// Token exchange rejected the grant.
    InvalidGrant,
    /// Client credentials rejected.
    InvalidClient,
    /// Token or handshake artifact expired.
    Expired,
    /// Userinfo fetch failed or returned an unusable document.
    UserinfoFailed,
    /// The upstream IdP was unreachable or returned a transport error.
    ProviderUnreachable,
    /// SCRAM credential verification failed.
    InvalidCredentials,
    /// The account is unknown.
    UnknownUser,
    /// Structurally malformed request (missing/invalid parameters).
    Malformed,
    /// Any rejection not covered by a specific bucket.
    Other,
}

impl sealed::Sealed for AuthErrorCode {}
impl MetricAttribute for AuthErrorCode {
    fn key(&self) -> &'static str {
        "error_code"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::InvalidState => "invalid_state",
            Self::InvalidCode => "invalid_code",
            Self::InvalidGrant => "invalid_grant",
            Self::InvalidClient => "invalid_client",
            Self::Expired => "expired",
            Self::UserinfoFailed => "userinfo_failed",
            Self::ProviderUnreachable => "provider_unreachable",
            Self::InvalidCredentials => "invalid_credentials",
            Self::UnknownUser => "unknown_user",
            Self::Malformed => "malformed",
            Self::Other => "other",
        }
    }
}

/// `event` — call-signaling event kinds (#1319): XEP-0353 Jingle
/// Message Initiation verbs plus Muji room join/leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallSignalEvent {
    JmiPropose,
    JmiProceed,
    JmiReject,
    JmiRetract,
    MujiJoin,
    MujiLeave,
}

impl sealed::Sealed for CallSignalEvent {}
impl MetricAttribute for CallSignalEvent {
    fn key(&self) -> &'static str {
        "event"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::JmiPropose => "jmi_propose",
            Self::JmiProceed => "jmi_proceed",
            Self::JmiReject => "jmi_reject",
            Self::JmiRetract => "jmi_retract",
            Self::MujiJoin => "muji_join",
            Self::MujiLeave => "muji_leave",
        }
    }
}

/// `reason` — why the Muji/SFU token gate refused to mint a LiveKit
/// JWT (#1319). Closed metric-facing set; the gate maps its concrete
/// denial into these buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SfuDenialReason {
    /// The requester is not a member/occupant of the room.
    MembershipDenied,
    /// The room does not exist.
    RoomNotFound,
    /// The requester failed authentication/authorization upstream of
    /// membership (bad session, missing identity).
    NotAuthorized,
    /// The gate itself failed (lookup error, dependency failure).
    InternalError,
}

impl sealed::Sealed for SfuDenialReason {}
impl MetricAttribute for SfuDenialReason {
    fn key(&self) -> &'static str {
        "reason"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::MembershipDenied => "membership_denied",
            Self::RoomNotFound => "room_not_found",
            Self::NotAuthorized => "not_authorized",
            Self::InternalError => "internal_error",
        }
    }
}

/// `outcome` — LiveKit webhook ingestion outcome (#1319).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookOutcome {
    /// A fresh, signature-valid event.
    Received,
    /// A replay of an already-processed event id.
    Duplicate,
    /// Signature verification failed.
    SignatureFailed,
    /// The signature was valid, but the body did not decode as an event.
    DecodeFailed,
}

impl sealed::Sealed for WebhookOutcome {}
impl MetricAttribute for WebhookOutcome {
    fn key(&self) -> &'static str {
        "outcome"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Duplicate => "duplicate",
            Self::SignatureFailed => "signature_failed",
            Self::DecodeFailed => "decode_failed",
        }
    }
}

/// `outcome` — generic success/failure for bounded request-shaped
/// operations (token mint, TURN credential issuance, provider send).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOutcome {
    Success,
    Failure,
}

impl sealed::Sealed for RequestOutcome {}
impl MetricAttribute for RequestOutcome {
    fn key(&self) -> &'static str {
        "outcome"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

/// `stage` — where in the push-notification pipeline a sample was
/// taken (#531). One variant per observable pipeline transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushStage {
    /// A notification candidate was created from committed state.
    CandidateCreated,
    /// The candidate was suppressed (see `PushSuppressReason`).
    Suppressed,
    /// The candidate was coalesced into an existing notification.
    Coalesced,
    /// The XEP-0357 publish reached the push service.
    Published,
    /// The downstream provider accepted the notification.
    ProviderSent,
    /// The downstream provider rejected the notification.
    ProviderRejected,
    /// The downstream provider reported an expired token.
    ProviderTokenExpired,
    /// The outbox scheduled a retry.
    RetryScheduled,
    /// The outbox dead-lettered the job.
    DeadLettered,
}

impl sealed::Sealed for PushStage {}
impl MetricAttribute for PushStage {
    fn key(&self) -> &'static str {
        "stage"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::CandidateCreated => "candidate_created",
            Self::Suppressed => "suppressed",
            Self::Coalesced => "coalesced",
            Self::Published => "published",
            Self::ProviderSent => "provider_sent",
            Self::ProviderRejected => "provider_rejected",
            Self::ProviderTokenExpired => "provider_token_expired",
            Self::RetryScheduled => "retry_scheduled",
            Self::DeadLettered => "dead_lettered",
        }
    }
}

/// `provider` — which push provider a sample refers to (#531).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushProvider {
    WebPush,
    Apns,
    Fcm,
}

impl sealed::Sealed for PushProvider {}
impl MetricAttribute for PushProvider {
    fn key(&self) -> &'static str {
        "provider"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::WebPush => "web_push",
            Self::Apns => "apns",
            Self::Fcm => "fcm",
        }
    }
}

/// `reason` — why post-auth session initialization failed (#1454). One
/// variant per `SessionInitializationFailed` return site in the WebSocket
/// registration path, so the `waddle.session.init.failed` rate is
/// attributable without unbounded values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionInitFailureReason {
    /// The XEP-0191 blocklist load at bind failed; the bind fails closed
    /// rather than running the session with an empty blocklist.
    BlocklistLoad,
    /// The authoritative `UserActor` registration (or clustered
    /// remote-resource registration) could not confirm the resource; the
    /// DashMap registration was rolled back and the bind failed closed
    /// (ADR-0017 Phase 1: the two views must never disagree in the
    /// miss-a-resource direction).
    AuthoritativeRegistration,
}

impl sealed::Sealed for SessionInitFailureReason {}
impl MetricAttribute for SessionInitFailureReason {
    fn key(&self) -> &'static str {
        "reason"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::BlocklistLoad => "blocklist_load",
            Self::AuthoritativeRegistration => "authoritative_registration",
        }
    }
}

/// `deny_reason` — why a MUC join was refused (#1440). The stanza
/// error condition alone collapses several very different operational
/// faults into `resource-constraint`/`internal-server-error`, so every
/// denial also carries the concrete refusal site. One variant per
/// join-denial return site in the WebSocket presence handler; the set
/// is closed by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucJoinDenyReason {
    /// The managed-channel lookup for the room failed.
    ManagedChannelLookup,
    /// The room-registry lookup performed before the join failed.
    RoomLookup,
    /// Snapshotting the existing room actor before the join failed.
    RoomSnapshot,
    /// A managed channel was joined without an authenticated session.
    SessionMissing,
    /// The authenticated session carried an unparsable identity.
    SessionIdentityMalformed,
    /// The permission graph reports the joiner as a channel outcast.
    ChannelBan,
    /// The managed channel is members-only and the joiner has no
    /// membership.
    MembershipRequired,
    /// The managed-channel affiliation resolver itself failed.
    AffiliationResolver,
    /// Another cluster node currently holds this room's claim and the
    /// ordered relay could not proxy the join.
    OwnershipHeldByAnotherNode,
    /// This room's ownership claim is mid-reconciliation.
    OwnershipReconciling,
    /// The room actor could not resolve its ownership at all.
    OwnershipUnavailable,
    /// The room's durable state had not finished restoring.
    DurableRestorePending,
    /// Getting or creating the room actor failed.
    RoomCreate,
    /// The room actor was evicted underneath the join, twice.
    RoomEvicted,
    /// The admission revision kept changing under the join.
    StaleAdmissionRevision,
    /// The room actor reports the joiner as banned from the room.
    RoomBan,
    /// The room actor is members-only and the joiner is unaffiliated.
    RoomMembersOnly,
    /// The room has reached its configured occupant limit.
    RoomFull,
    /// The room actor refused the join with an otherwise unclassified
    /// error (typed fail-safe).
    RoomActorError,
}

impl sealed::Sealed for MucJoinDenyReason {}
impl MetricAttribute for MucJoinDenyReason {
    fn key(&self) -> &'static str {
        "deny_reason"
    }
    fn value(&self) -> &'static str {
        match self {
            Self::ManagedChannelLookup => "managed_channel_lookup",
            Self::RoomLookup => "room_lookup",
            Self::RoomSnapshot => "room_snapshot",
            Self::SessionMissing => "session_missing",
            Self::SessionIdentityMalformed => "session_identity_malformed",
            Self::ChannelBan => "channel_ban",
            Self::MembershipRequired => "membership_required",
            Self::AffiliationResolver => "affiliation_resolver",
            Self::OwnershipHeldByAnotherNode => "ownership_held_by_another_node",
            Self::OwnershipReconciling => "ownership_reconciling",
            Self::OwnershipUnavailable => "ownership_unavailable",
            Self::DurableRestorePending => "durable_restore_pending",
            Self::RoomCreate => "room_create",
            Self::RoomEvicted => "room_evicted",
            Self::StaleAdmissionRevision => "stale_admission_revision",
            Self::RoomBan => "room_ban",
            Self::RoomMembersOnly => "room_members_only",
            Self::RoomFull => "room_full",
            Self::RoomActorError => "room_actor_error",
        }
    }
}
