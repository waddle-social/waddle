//! Typed XMPP WebSocket frames, transport events, and transport factory contracts.

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
pub use native::{DefaultTransportFactory, WebSocketTransport, WebSocketTransportFactory};

/// Transport kind supported by the native client runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// RFC 7395 WebSocket transport.
    WebSocket,
}

/// Connection state surfaced by the transport adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    /// No connection or bootstrap operation is in progress.
    Idle,
    /// The transport connection is being established.
    Connecting,
    /// Access is open to any requester.
    Open,
    /// The transport is closing.
    Closing,
    /// The connection has fully closed.
    Closed,
    /// Stream-management negotiation or resumption failed.
    Failed,
}

/// Static feature flags reported by a connected transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportCapabilities {
    /// Whether the server advertised XEP-0198 support.
    pub stream_management: bool,
    /// Whether the session may be resumed after transport loss.
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
    /// XMPP address of the recipient.
    pub to: Option<BareJid>,
    /// XMPP address of the sender.
    pub from: Option<BareJid>,
    /// Identifier used to correlate this value with its protocol or domain record.
    pub id: Option<StreamId>,
    /// Language tag associated with the displayed text.
    pub language: Option<String>,
    /// Version advertised by the extension or protocol entity.
    pub version: &'static str,
}

impl StreamOpen {
    /// Protocol token `1.0`.
    pub const VERSION: &'static str = "1.0";

    /// Construct this bootstrap value from the validated client configuration.
    pub fn from_config(config: &ClientConfig) -> Self {
        Self {
            to: Some(config.connection.server.clone()),
            from: None,
            id: None,
            language: config.session.language.clone(),
            version: Self::VERSION,
        }
    }

    /// Create a server-originated stream-open frame with the supplied sender JID.
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
    /// A stream-open frame starting XMPP negotiation.
    Open(StreamOpen),
    /// A parsed application stanza or stream-control element.
    Element(Element),
    /// A stream-close frame ending the XMPP session.
    Close(StreamClose),
}

/// Transport-level events surfaced back into the session runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    /// The transport changed lifecycle state.
    StateChanged(TransportState),
    /// A complete XMPP frame was received and decoded.
    MessageReceived(TransportMessage),
    /// An XMPP frame was successfully written to the transport.
    MessageSent(TransportMessage),
    /// The connection has fully closed.
    Closed,
}

impl TransportEvent {
    /// Map this transport event to its resulting transport state, if any.
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
