//! Common types for the XMPP server.

use serde::{Deserialize, Serialize};

/// Connection state in the XMPP stream lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Initial state, waiting for stream header
    Initial,
    /// Stream opened, negotiating features
    Negotiating,
    /// STARTTLS upgrade in progress
    StartTls,
    /// TLS established, ready for SASL
    TlsEstablished,
    /// SASL authentication in progress
    Authenticating,
    /// Authenticated, binding resource
    Authenticated,
    /// Fully established session
    Established,
    /// Connection closing
    Closing,
    /// Connection closed
    Closed,
}

/// Transport type for the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    /// Plain TCP (pre-STARTTLS)
    Tcp,
    /// TCP with TLS (post-STARTTLS)
    TcpTls,
    /// WebSocket
    WebSocket,
    /// WebSocket with TLS
    WebSocketTls,
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transport::Tcp => write!(f, "tcp"),
            Transport::TcpTls => write!(f, "tcp+tls"),
            Transport::WebSocket => write!(f, "ws"),
            Transport::WebSocketTls => write!(f, "wss"),
        }
    }
}

/// Stanza type for metrics and tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StanzaType {
    /// Message stanza
    Message,
    /// Presence stanza
    Presence,
    /// IQ (info/query) stanza
    Iq,
    /// Stream management stanza
    StreamManagement,
    /// Unknown or internal stanza
    Other,
}

impl std::fmt::Display for StanzaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StanzaType::Message => write!(f, "message"),
            StanzaType::Presence => write!(f, "presence"),
            StanzaType::Iq => write!(f, "iq"),
            StanzaType::StreamManagement => write!(f, "sm"),
            StanzaType::Other => write!(f, "other"),
        }
    }
}

/// MUC room affiliation levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Affiliation {
    /// Banned from the room
    Outcast,
    /// No affiliation
    None,
    /// Room member
    Member,
    /// Room administrator
    Admin,
    /// Room owner
    Owner,
}

impl std::fmt::Display for Affiliation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Affiliation::Outcast => write!(f, "outcast"),
            Affiliation::None => write!(f, "none"),
            Affiliation::Member => write!(f, "member"),
            Affiliation::Admin => write!(f, "admin"),
            Affiliation::Owner => write!(f, "owner"),
        }
    }
}

/// MUC room role (session-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Role {
    /// No role (not in room)
    None,
    /// Visitor (can read, limited send)
    Visitor,
    /// Participant (normal user)
    Participant,
    /// Moderator (can kick, manage)
    Moderator,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::None => write!(f, "none"),
            Role::Visitor => write!(f, "visitor"),
            Role::Participant => write!(f, "participant"),
            Role::Moderator => write!(f, "moderator"),
        }
    }
}
