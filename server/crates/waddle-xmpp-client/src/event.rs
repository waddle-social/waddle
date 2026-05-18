use minidom::Element;

use crate::bootstrap::{
    AuthenticationRequest, ResourceBindingRequest, SaslFailure, StreamFeatures,
};
use crate::inbox::InboxStreamEntry;
use crate::mam::ArchivedMessage;
use crate::messaging::MessagingEvent;
use crate::pep::PepItem;
use crate::request::StanzaId;
use crate::request::{PendingRequest, RequestCorrelation};
use crate::state::{SessionBinding, SessionSnapshot};
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
    /// Typed MAM archived message from an active history query.
    MamResult(ArchivedMessage),
    /// Typed streamed XEP-0430 inbox `<entry/>` from an active inbox
    /// query. Correlated to the pending inbox request via
    /// [`InboxStreamEntry::query_id`].
    InboxStreamEntry(InboxStreamEntry),
    /// Typed PEP user-state event (mood, activity, tune).
    PepEvent(PepItem),
    /// Transport-level delivery status for outbound message stanzas.
    MessageDelivery(MessageDeliveryEvent),
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
    Enabled { previd: Option<String> },
    AckReceived { h: u32 },
    AckRequested,
    Resumed { h: u32 },
    Failed,
}

/// Delivery status for outbound `<message/>` stanzas tracked through
/// XEP-0198 stream-management acknowledgements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageDeliveryEvent {
    Acked { stanza_id: StanzaId },
    Failed { stanza_id: StanzaId },
}
