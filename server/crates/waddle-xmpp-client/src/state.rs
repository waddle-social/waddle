use std::fmt;

use jid::FullJid;

use crate::transport::TransportState;

/// App-facing state summary derived from the richer session phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    Ready,
    Connecting,
    Authenticating,
    Connected,
    Recovering,
    Disconnecting,
    Disconnected,
}

/// Connection/session phases owned by the client runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Disconnected,
    Connecting,
    OpeningStream,
    Authenticating,
    Binding,
    Established,
    Resuming,
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
    pub jid: FullJid,
    pub stream_id: Option<StreamId>,
    pub resumable: bool,
}

/// Stream management identifier retained across reconnect/resume.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct StreamId(String);

impl StreamId {
    pub fn new(stream_id: impl Into<String>) -> Self {
        Self(stream_id.into())
    }

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
    pub phase: SessionPhase,
    pub transport: TransportState,
    pub binding: Option<SessionBinding>,
    pub pending_requests: usize,
}

impl SessionSnapshot {
    pub fn new() -> Self {
        Self {
            phase: SessionPhase::Disconnected,
            transport: TransportState::Idle,
            binding: None,
            pending_requests: 0,
        }
    }

    pub fn client_state(&self) -> ClientState {
        self.phase.into()
    }
}

impl Default for SessionSnapshot {
    fn default() -> Self {
        Self::new()
    }
}
