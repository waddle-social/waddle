//! Lifecycle, connection, delivery, and stream-management events exposed to hosts.

use minidom::Element;

use crate::avatar::AvatarChanged;
use crate::bootstrap::{
    AuthenticationRequest, ResourceBindingRequest, SaslFailure, StreamFeatures,
};
use crate::inbox::InboxStreamEntry;
use crate::mam::ArchivedMessage;
use crate::messaging::{InboundCallEvent, MessageRejection, MessagingEvent};
use crate::pep::PepItem;
use crate::pubsub_event::{PubsubAttachmentSummaryUpdate, PubsubRetractedItems};
use crate::request::StanzaId;
use crate::request::{PendingRequest, RequestCorrelation};
use crate::state::{SessionBinding, SessionSnapshot, StreamId};
use crate::stream_management::SmResumeState;
use crate::transport::{StreamOpen, TransportEvent, TransportMessage};

/// Public event surface emitted by the client runtime.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// The client session changed state, completed binding, or became ready.
    Lifecycle(LifecycleEvent),
    /// A connection negotiation milestone or outbound transport instruction.
    Connection(ConnectionEvent),
    /// A client request was registered and is awaiting completion.
    RequestQueued(PendingRequest),
    /// A pending request completed and released its correlation identifiers.
    RequestResolved(RequestCorrelation),
    /// A transport event was applied to the client runtime.
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
    /// XEP-0060 §7.2.2.1 item retraction pushed from a pubsub/PEP
    /// node. Emitted alongside the [`ClientEvent::Messaging`] event
    /// that carried the notification. No consumer maps it yet: the
    /// wasm dispatcher deliberately swallows it (web consumes the
    /// message-attached `pubsub_events` instead) and the upcoming
    /// community-surface FFI verbs will route it to Kotlin/Swift
    /// listeners so they can drop retracted items without digging
    /// through the message envelope.
    PubsubItemsRetracted(PubsubRetractedItems),
    /// XEP-0470 attachment-summary update pushed from a summary node.
    /// Emitted alongside the [`ClientEvent::Messaging`] event that
    /// carried the notification; same consumer story as
    /// [`ClientEvent::PubsubItemsRetracted`].
    PubsubAttachmentSummary(PubsubAttachmentSummaryUpdate),
    /// XEP-0084 metadata transition announced by a peer's PEP service.
    AvatarChanged(AvatarChanged),
    /// ADR-010 cross-device manual status: another resource of this
    /// account published a pick to its own `urn:waddle:status-preference:0`
    /// PEP node. Emitted alongside the [`ClientEvent::Messaging`] event
    /// that carried the notification, which the wasm client still reads.
    StatusPreferenceChanged(waddle_xmpp_core::waddle_status_preference::StatusPreference),
    /// Transport-level delivery status for outbound message stanzas.
    MessageDelivery(MessageDeliveryEvent),
    /// A parsed message error. Echoed bodies and extensions are never dispatched.
    MessageRejected(Box<MessageRejection>),
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
        /// Identifier used to correlate this value with its protocol or domain record.
        id: String,
        /// Parsed XML element retained at the protocol boundary.
        element: Element,
    },
    /// A persisted XEP-0198 tail cannot safely replay this IQ on a fresh
    /// stream. Drivers must complete the matching caller instead of leaving
    /// its correlation responder pending indefinitely.
    IqCancelled {
        /// Identifier used for stanza correlation.
        stanza_id: StanzaId,
    },
    /// Post-bootstrap stanza that no built-in handler claimed.
    UnhandledStanza(Element),
}

/// Session lifecycle changes exposed to app-facing integrations.
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    /// The application-facing session snapshot changed.
    StateChanged(SessionSnapshot),
    /// The account received its bound full JID and stream identity.
    SessionBound(SessionBinding),
    /// The bound account session is ready for application stanzas.
    SessionReady(SessionBinding),
}

/// Typed connection-level milestones and outbound transport instructions.
#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    /// The host must write this XMPP frame to its transport.
    OutboundMessage(TransportMessage),
    /// Retire this connection without ending its XMPP session. Hosts close
    /// only the WebSocket, preserve the resume snapshot, and stop the driver.
    CloseTransport,
    /// The runtime requested opening a new XMPP stream.
    StreamOpening(StreamOpen),
    /// The peer accepted the stream and returned its opening frame.
    StreamOpened(StreamOpen),
    /// The peer terminated the stream with a protocol error.
    StreamError {
        /// XMPP error condition identifying the protocol failure.
        condition: StreamErrorCondition,
        /// Additional structured details accompanying the error.
        detail: Option<StreamErrorDetail>,
    },
    /// The peer advertised authentication and session bootstrap capabilities.
    FeaturesAdvertised(StreamFeatures),
    /// The runtime requested transmission of an authentication exchange.
    AuthenticationRequested(AuthenticationRequest),
    /// The peer accepted the supplied authentication credentials.
    AuthenticationSucceeded,
    /// The peer rejected authentication with a typed SASL failure condition.
    AuthenticationFailed(SaslFailure),
    /// The runtime requested a resource-binding IQ exchange.
    ResourceBindingRequested(ResourceBindingRequest),
    /// The peer assigned a full account JID to the current stream.
    ResourceBound(SessionBinding),
    /// The bound account session is ready for application stanzas.
    SessionReady(SessionBinding),
    /// The stream-management negotiation, counters, or acknowledgement policy changed.
    StreamManagement(StreamManagementEvent),
}

/// XEP-0198 stream management lifecycle notifications.
#[derive(Debug, Clone)]
pub enum StreamManagementEvent {
    /// Stream management was enabled with the peer's resumption parameters.
    Enabled {
        #[doc = "Identifier of the resumable stream session."]
        previd: Option<StreamId>,
    },
    /// An acknowledgement arrived from the peer.
    AckReceived {
        #[doc = "Peer stanza-handled counter modulo 2^32."]
        h: u32,
        #[doc = "Whether this acknowledgement advanced the peer's handled counter."]
        progressed: bool,
    },
    /// An acknowledgement request has been written to the transport.
    AckRequested {
        #[doc = "Human-readable explanation for the operation."]
        reason: SmAckRequestReason,
    },
    /// The host should retry writing the pending acknowledgement request.
    AckRetry {
        #[doc = "One-based acknowledgement retry attempt."]
        attempt: u8,
    },
    /// The peer did not answer an acknowledgement request within its deadline.
    AckRequestTimedOut,
    /// The current transport cannot continue and the host must reconnect.
    ReconnectRequired,
    /// The peer accepted stream resumption and reports its handled count.
    Resumed {
        #[doc = "Peer stanza-handled counter modulo 2^32."]
        h: u32,
    },
    /// Stream-management negotiation or resumption failed.
    Failed,
}

/// Why this endpoint emitted a typed XEP-0198 acknowledgement request.
///
/// This stays a closed protocol value: hosts must not infer protocol state
/// from XML, counters, or transport diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmAckRequestReason {
    /// An outbound stanza was written and may require an acknowledgement request.
    OutboundStanza,
    /// The resumed session still retains an unacknowledged outbound tail.
    ResumedUnackedTail,
    /// The peer explicitly requested an acknowledgement.
    PeerRequest,
    /// The browser is hiding the page and requests a synchronous acknowledgement probe.
    Pagehide,
}

/// Typed stream-error extension detail carried alongside the RFC 6120
/// condition. Some XMPP extensions, including XEP-0198, intentionally use
/// `<undefined-condition/>` with an extension child that names the concrete
/// failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorDetail {
    /// The peer acknowledged more stanzas than the client has sent.
    HandledCountTooHigh {
        #[doc = "Peer stanza-handled counter modulo 2^32."]
        h: u32,
        #[doc = "Number of outbound stanzas sent modulo 2^32."]
        send_count: u32,
    },
}

/// RFC 6120 stream-error conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorCondition {
    /// The peer's stream XML has an invalid structure.
    BadFormat,
    /// The stream uses an invalid namespace prefix.
    BadNamespacePrefix,
    /// Another stream or resource conflicts with this connection.
    Conflict,
    /// The peer timed out the stream connection.
    ConnectionTimeout,
    /// The destination host is no longer served.
    HostGone,
    /// The destination host is not recognized by the server.
    HostUnknown,
    /// The stream has invalid addressing attributes.
    ImproperAddressing,
    /// The peer encountered an internal error while processing the stream.
    InternalServerError,
    /// The stream's sender identity is invalid.
    InvalidFrom,
    /// The stream uses the wrong XML namespace.
    InvalidNamespace,
    /// The supplied XML cannot be parsed or violates the supported shape.
    InvalidXml,
    /// The peer rejected the authorization attempt.
    NotAuthorized,
    /// The peer's XML is not well formed.
    NotWellFormed,
    /// The peer rejected the stream for violating its policy.
    PolicyViolation,
    /// The peer could not connect to a required remote service.
    RemoteConnectionFailed,
    /// Clear the current state and return to its initial values.
    Reset,
    /// The peer lacks the resources needed to maintain the stream.
    ResourceConstraint,
    /// The stream contains XML constructs prohibited by XMPP.
    RestrictedXml,
    /// The peer requests connection to a different host.
    SeeOtherHost,
    /// The peer is shutting down the service.
    SystemShutdown,
    /// The peer supplied no recognized stream-error condition.
    UndefinedCondition,
    /// The peer does not support the requested character encoding.
    UnsupportedEncoding,
    /// The request requires a feature that this service does not implement.
    UnsupportedFeature,
    /// The peer received a stanza type it cannot process.
    UnsupportedStanzaType,
    /// The peer does not support the requested stream version.
    UnsupportedVersion,
}

impl StreamErrorCondition {
    /// Recognize an RFC 6120 stream-error condition by its local name.
    #[must_use]
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

    /// Borrow the stored text or canonical protocol token without allocating.
    #[must_use]
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
    /// The peer acknowledged handling the outbound stanza.
    Acked {
        #[doc = "Identifier used for stanza correlation."]
        stanza_id: StanzaId,
    },
    /// Stream-management negotiation or resumption failed.
    Failed {
        #[doc = "Identifier used for stanza correlation."]
        stanza_id: StanzaId,
    },
}
