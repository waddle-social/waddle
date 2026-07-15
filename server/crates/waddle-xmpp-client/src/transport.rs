use jid::BareJid;
use minidom::Element;

use crate::config::ClientConfig;
use crate::state::StreamId;

mod codec;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
mod native;
#[cfg(test)]
mod tests;

pub use codec::{decode_message, encode_message};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use native::{
    DefaultTransportFactory, TransportWriteFailure, TransportWriteResponsibility,
    TransportWriteResult, WebSocketTransport, WebSocketTransportFactory,
};

/// Transport kind supported by the native client runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    WebSocket,
}

/// Connection state surfaced by the transport adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    Idle,
    Connecting,
    Open,
    Closing,
    Closed,
    Failed,
}

/// Static feature flags reported by a connected transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportCapabilities {
    pub stream_management: bool,
    pub resumable: bool,
}

impl Default for TransportCapabilities {
    fn default() -> Self {
        Self {
            stream_management: true,
            resumable: true,
        }
    }
}

/// RFC 7395 `<open/>` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOpen {
    pub to: Option<BareJid>,
    pub from: Option<BareJid>,
    pub id: Option<StreamId>,
    pub language: Option<String>,
    pub version: &'static str,
}

impl StreamOpen {
    pub const VERSION: &'static str = "1.0";

    pub fn from_config(config: &ClientConfig) -> Self {
        Self {
            to: Some(config.connection.server.clone()),
            from: None,
            id: None,
            language: config.session.language.clone(),
            version: Self::VERSION,
        }
    }

    pub fn from_server(from: BareJid) -> Self {
        Self {
            to: None,
            from: Some(from),
            id: None,
            language: None,
            version: Self::VERSION,
        }
    }
}

/// RFC 7395 `<close/>` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamClose;

/// Typed transport payloads exchanged across the runtime boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportMessage {
    Open(StreamOpen),
    Element(Element),
    Close(StreamClose),
}

/// Transport-level events surfaced back into the session runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    StateChanged(TransportState),
    MessageReceived(TransportMessage),
    MessageSent(TransportMessage),
    Closed,
}

impl TransportEvent {
    pub fn transport_state(&self) -> TransportState {
        match self {
            Self::StateChanged(state) => *state,
            Self::MessageReceived(TransportMessage::Close(_))
            | Self::MessageSent(TransportMessage::Close(_)) => TransportState::Closing,
            Self::MessageReceived(_) | Self::MessageSent(_) => TransportState::Open,
            Self::Closed => TransportState::Closed,
        }
    }
}
