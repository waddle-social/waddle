//! Session snapshots and validated client and authentication phase transitions.

use std::fmt;

use jid::FullJid;

use crate::transport::TransportState;

/// App-facing state summary derived from the richer session phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    /// The host may write the prepared acknowledgement request.
    Ready,
    /// The transport connection is being established.
    Connecting,
    /// SASL authentication is in progress.
    Authenticating,
    /// The transport connection is established.
    Connected,
    /// The client is recovering a previously established session.
    Recovering,
    /// The session is closing its stream and transport.
    Disconnecting,
    /// No active XMPP session is established.
    Disconnected,
}

/// Connection/session phases owned by the client runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    /// No active XMPP session is established.
    Disconnected,
    /// The transport connection is being established.
    Connecting,
    /// The transport is open and the XMPP stream header is being negotiated.
    OpeningStream,
    /// SASL authentication is in progress.
    Authenticating,
    /// Resource binding is in progress.
    Binding,
    /// Authentication and resource binding are complete.
    Established,
    /// The client is attempting to resume a previous stream.
    Resuming,
    /// The session is closing its stream and transport.
    Disconnecting,
}

impl From<SessionPhase> for ClientState {
    fn from(value: SessionPhase) -> Self {
        match value {
            SessionPhase::Disconnected => Self::Disconnected,
            SessionPhase::Connecting => Self::Connecting,
            SessionPhase::OpeningStream | SessionPhase::Binding => Self::Connected,
            SessionPhase::Authenticating => Self::Authenticating,
            SessionPhase::Established => Self::Ready,
            SessionPhase::Resuming => Self::Recovering,
            SessionPhase::Disconnecting => Self::Disconnecting,
        }
    }
}

/// Bound JID plus stream-management metadata for an established session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBinding {
    /// XMPP address identifying the referenced entity.
    pub jid: FullJid,
    /// Identifier of the stream associated with this value.
    pub stream_id: Option<StreamId>,
    /// Whether the session may be resumed after transport loss.
    pub resumable: bool,
}

/// Stream management identifier retained across reconnect/resume.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct StreamId(String);

impl StreamId {
    /// Wrap the opaque server-assigned stream identifier without additional validation.
    #[must_use]
    pub fn new(stream_id: impl Into<String>) -> Self {
        Self(stream_id.into())
    }

    /// Borrow the stored text without allocating.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("StreamId").field(&self.0).finish()
    }
}

/// Immutable snapshot published to app/runtime observers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    /// Current phase of session establishment or shutdown.
    pub phase: SessionPhase,
    /// WebSocket endpoint, origin, and connection timeout settings.
    pub transport: TransportState,
    /// Established account resource and stream identity.
    pub binding: Option<SessionBinding>,
    /// Number of requests awaiting a correlated response.
    pub pending_requests: usize,
}

impl SessionSnapshot {
    /// Create a disconnected session snapshot without a binding or pending requests.
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase: SessionPhase::Disconnected,
            transport: TransportState::Idle,
            binding: None,
            pending_requests: 0,
        }
    }

    /// Map the authentication/session phase to the application-facing connection state.
    #[must_use]
    pub fn client_state(&self) -> ClientState {
        self.phase.into()
    }
}

impl Default for SessionSnapshot {
    fn default() -> Self {
        Self::new()
    }
}
