use minidom::Element;

use crate::bootstrap::{
    AuthenticationRequest, ResourceBindingRequest, SaslFailure, StreamFeatures,
};
use crate::inbox::InboxStreamEntry;
use crate::mam::ArchivedMessage;
use crate::messaging::{InboundCallEvent, MessagingEvent};
use crate::pep::PepItem;
use crate::request::StanzaId;
use crate::request::{PendingRequest, RequestCorrelation};
use crate::state::{SessionBinding, SessionSnapshot};
use crate::stream_management::SmResumeState;
use crate::transport::{StreamOpen, TransportEvent, TransportMessage};

/// Public event surface emitted by the client runtime.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    Lifecycle(LifecycleEvent),
    Connection(ConnectionEvent),
    RequestQueued(PendingRequest),
    RequestResolved(RequestCorrelation),
    Transport(TransportEvent),
    /// Typed inbound message or presence event.
    Messaging(MessagingEvent),
    /// Typed inbound A/V call control event.
    Call(Box<InboundCallEvent>),
    /// Typed MAM archived message from an active history query.
    ///
    /// Boxed: `ArchivedMessage` ballooned to ~480 bytes after the
    /// xmpp-parsers 0.22 `Iq`/payload restructuring, which would
    /// otherwise drag the whole `ClientEvent` enum size up.
    MamResult(Box<ArchivedMessage>),
    /// Typed XEP-0430 inbox `<entry/>`. Query response entries are
    /// correlated to pending inbox requests by query id when available;
    /// unsolicited headline pushes are source-marked and surfaced to
    /// application callbacks.
    InboxStreamEntry(InboxStreamEntry),
    /// Typed PEP user-state event (mood, activity, tune).
    PepEvent(PepItem),
    /// Transport-level delivery status for outbound message stanzas.
    MessageDelivery(MessageDeliveryEvent),
    /// XEP-0198 resume snapshot changed. Broadcast by the native driver
    /// whenever the runtime's resumable state differs from the last
    /// published snapshot — the same hook points the wasm driver uses
    /// for its cell-based snapshot. `None` after an explicit disconnect
    /// (resume MUST NOT be attempted across a clean `</stream>` close)
    /// or when the session carries no resumable SM state.
    ResumeStateChanged(Option<SmResumeState>),
    /// Correlated IQ result or error for in-flight `send_iq` calls.
    ///
    /// Consumed by the native driver to resolve the pending IQ request; never
    /// broadcast on the public event bus.
    IqResult {
        id: String,
        element: Element,
    },
    /// Post-bootstrap stanza that no built-in handler claimed.
    UnhandledStanza(Element),
}

/// Session lifecycle changes exposed to app-facing integrations.
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    StateChanged(SessionSnapshot),
    SessionBound(SessionBinding),
    SessionReady(SessionBinding),
}

/// Typed connection-level milestones and outbound transport instructions.
#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    OutboundMessage(TransportMessage),
    StreamOpening(StreamOpen),
    StreamOpened(StreamOpen),
    StreamError {
        condition: StreamErrorCondition,
        detail: Option<StreamErrorDetail>,
    },
    FeaturesAdvertised(StreamFeatures),
    AuthenticationRequested(AuthenticationRequest),
    AuthenticationSucceeded,
    AuthenticationFailed(SaslFailure),
    ResourceBindingRequested(ResourceBindingRequest),
    ResourceBound(SessionBinding),
    SessionReady(SessionBinding),
    StreamManagement(StreamManagementEvent),
}

/// XEP-0198 stream management lifecycle notifications.
#[derive(Debug, Clone)]
pub enum StreamManagementEvent {
    Enabled {
        previd: Option<String>,
    },
    AckReceived {
        h: u32,
    },
    /// The client emitted `<r/>` for its outstanding client-to-server
    /// stanza window. Carries counters only; no stanza or stream identity.
    AckRequestSent {
        attempt: u32,
        unacked: u32,
    },
    /// A valid server `<a/>` released the request-in-flight latch.
    AckObserved {
        progressed: bool,
        latency_ms: Option<u64>,
        unacked: u32,
    },
    /// The server did not answer `<r/>` within the bounded response window.
    AckRequestTimedOut {
        unacked: u32,
    },
    /// The server's handled count has not advanced for the bounded progress
    /// window. Drivers use this typed signal to abort the XML stream
    /// uncleanly so normal reconnect/resume can take over.
    AckProgressStalled {
        unacked: u32,
        elapsed_ms: u64,
    },
    AckRequested,
    Resumed {
        h: u32,
    },
    Failed,
}

/// Typed stream-error extension detail carried alongside the RFC 6120
/// condition. Some XMPP extensions, including XEP-0198, intentionally use
/// `<undefined-condition/>` with an extension child that names the concrete
/// failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorDetail {
    HandledCountTooHigh { h: u32, send_count: u32 },
}

/// RFC 6120 stream-error conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorCondition {
    BadFormat,
    BadNamespacePrefix,
    Conflict,
    ConnectionTimeout,
    HostGone,
    HostUnknown,
    ImproperAddressing,
    InternalServerError,
    InvalidFrom,
    InvalidNamespace,
    InvalidXml,
    NotAuthorized,
    NotWellFormed,
    PolicyViolation,
    RemoteConnectionFailed,
    Reset,
    ResourceConstraint,
    RestrictedXml,
    SeeOtherHost,
    SystemShutdown,
    UndefinedCondition,
    UnsupportedEncoding,
    UnsupportedFeature,
    UnsupportedStanzaType,
    UnsupportedVersion,
}

impl StreamErrorCondition {
    pub const ALL: [Self; 25] = [
        Self::BadFormat,
        Self::BadNamespacePrefix,
        Self::Conflict,
        Self::ConnectionTimeout,
        Self::HostGone,
        Self::HostUnknown,
        Self::ImproperAddressing,
        Self::InternalServerError,
        Self::InvalidFrom,
        Self::InvalidNamespace,
        Self::InvalidXml,
        Self::NotAuthorized,
        Self::NotWellFormed,
        Self::PolicyViolation,
        Self::RemoteConnectionFailed,
        Self::Reset,
        Self::ResourceConstraint,
        Self::RestrictedXml,
        Self::SeeOtherHost,
        Self::SystemShutdown,
        Self::UndefinedCondition,
        Self::UnsupportedEncoding,
        Self::UnsupportedFeature,
        Self::UnsupportedStanzaType,
        Self::UnsupportedVersion,
    ];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "bad-format" => Some(Self::BadFormat),
            "bad-namespace-prefix" => Some(Self::BadNamespacePrefix),
            "conflict" => Some(Self::Conflict),
            "connection-timeout" => Some(Self::ConnectionTimeout),
            "host-gone" => Some(Self::HostGone),
            "host-unknown" => Some(Self::HostUnknown),
            "improper-addressing" => Some(Self::ImproperAddressing),
            "internal-server-error" => Some(Self::InternalServerError),
            "invalid-from" => Some(Self::InvalidFrom),
            "invalid-namespace" => Some(Self::InvalidNamespace),
            "invalid-xml" => Some(Self::InvalidXml),
            "not-authorized" => Some(Self::NotAuthorized),
            "not-well-formed" => Some(Self::NotWellFormed),
            "policy-violation" => Some(Self::PolicyViolation),
            "remote-connection-failed" => Some(Self::RemoteConnectionFailed),
            "reset" => Some(Self::Reset),
            "resource-constraint" => Some(Self::ResourceConstraint),
            "restricted-xml" => Some(Self::RestrictedXml),
            "see-other-host" => Some(Self::SeeOtherHost),
            "system-shutdown" => Some(Self::SystemShutdown),
            "undefined-condition" => Some(Self::UndefinedCondition),
            "unsupported-encoding" => Some(Self::UnsupportedEncoding),
            "unsupported-feature" => Some(Self::UnsupportedFeature),
            "unsupported-stanza-type" => Some(Self::UnsupportedStanzaType),
            "unsupported-version" => Some(Self::UnsupportedVersion),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadFormat => "bad-format",
            Self::BadNamespacePrefix => "bad-namespace-prefix",
            Self::Conflict => "conflict",
            Self::ConnectionTimeout => "connection-timeout",
            Self::HostGone => "host-gone",
            Self::HostUnknown => "host-unknown",
            Self::ImproperAddressing => "improper-addressing",
            Self::InternalServerError => "internal-server-error",
            Self::InvalidFrom => "invalid-from",
            Self::InvalidNamespace => "invalid-namespace",
            Self::InvalidXml => "invalid-xml",
            Self::NotAuthorized => "not-authorized",
            Self::NotWellFormed => "not-well-formed",
            Self::PolicyViolation => "policy-violation",
            Self::RemoteConnectionFailed => "remote-connection-failed",
            Self::Reset => "reset",
            Self::ResourceConstraint => "resource-constraint",
            Self::RestrictedXml => "restricted-xml",
            Self::SeeOtherHost => "see-other-host",
            Self::SystemShutdown => "system-shutdown",
            Self::UndefinedCondition => "undefined-condition",
            Self::UnsupportedEncoding => "unsupported-encoding",
            Self::UnsupportedFeature => "unsupported-feature",
            Self::UnsupportedStanzaType => "unsupported-stanza-type",
            Self::UnsupportedVersion => "unsupported-version",
        }
    }
}

/// Delivery status for outbound `<message/>` stanzas tracked through
/// XEP-0198 stream-management acknowledgements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageDeliveryEvent {
    Acked { stanza_id: StanzaId },
    Failed { stanza_id: StanzaId },
}
