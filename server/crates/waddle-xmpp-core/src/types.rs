//! Shared protocol and domain enums.

use serde::{Deserialize, Serialize};

/// Connection state in the XMPP stream lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Initial state, waiting for stream header
    Initial,
    /// Stream opened, negotiating features
    Negotiating,
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
    /// WebSocket
    WebSocket,
    /// WebSocket with TLS
    WebSocketTls,
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub enum Affiliation {
    /// Banned from the room
    Outcast,
    /// No affiliation
    #[default]
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

/// Whether a room enforces XEP-0045 moderation. The visitor/voice
/// distinction is only meaningful in a moderated room (§Terminology
/// defines both "in a moderated room"), so every voice decision needs
/// this alongside the role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moderation {
    Moderated,
    Unmoderated,
}

impl Moderation {
    /// Lift a [`crate::types::Moderation`] out of a room-config flag.
    pub fn from_moderated_flag(moderated: bool) -> Self {
        if moderated {
            Self::Moderated
        } else {
            Self::Unmoderated
        }
    }
}

/// XEP-0045 "voice": the right to send to all occupants. The single
/// authority for this predicate across the codebase — text broadcast
/// (§7.5) and SFU media grants MUST agree, so both derive from
/// [`Role::voice`] rather than testing roles ad hoc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voice {
    Voiced,
    Muted,
}

impl Voice {
    pub fn is_voiced(self) -> bool {
        matches!(self, Self::Voiced)
    }
}

impl Role {
    /// Resolve this role's voice in a room with the given moderation.
    ///
    /// Per XEP-0045 §Terminology a visitor is "an occupant who does
    /// not have voice" *in a moderated room*; the §5.1.2 roles table
    /// footnote adds that an implementation MAY grant voice to
    /// visitors in unmoderated rooms — which Waddle does, so an
    /// unmoderated room's visitor keeps voice. `Role::None` is not an
    /// occupant at all and never has voice.
    pub fn voice(self, moderation: Moderation) -> Voice {
        match self {
            Role::None => Voice::Muted,
            Role::Visitor => match moderation {
                Moderation::Moderated => Voice::Muted,
                Moderation::Unmoderated => Voice::Voiced,
            },
            Role::Participant | Role::Moderator => Voice::Voiced,
        }
    }
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

#[cfg(test)]
mod voice_tests {
    use super::{Moderation, Role, Voice};

    /// XEP-0045 §Terminology defines Visitor as "an occupant who does
    /// not have voice" *in a moderated room*, and the §5.1.2 roles
    /// table footnote lets an implementation grant voice to visitors
    /// in unmoderated rooms. Pin both halves — the media-grant
    /// derivation and the §7.5 text gate both read this table, so a
    /// change here changes both.
    #[test]
    fn voice_depends_on_role_and_moderation() {
        for moderation in [Moderation::Moderated, Moderation::Unmoderated] {
            assert_eq!(Role::Moderator.voice(moderation), Voice::Voiced);
            assert_eq!(Role::Participant.voice(moderation), Voice::Voiced);
            assert_eq!(
                Role::None.voice(moderation),
                Voice::Muted,
                "role=none is not an occupant and never has voice"
            );
        }
        assert_eq!(
            Role::Visitor.voice(Moderation::Moderated),
            Voice::Muted,
            "a visitor in a moderated room is precisely an occupant without voice"
        );
        assert_eq!(
            Role::Visitor.voice(Moderation::Unmoderated),
            Voice::Voiced,
            "XEP-0045 §5.1.2 footnote: visitors MAY have voice in an unmoderated room"
        );
    }

    #[test]
    fn moderation_lifts_from_config_flag() {
        assert_eq!(Moderation::from_moderated_flag(true), Moderation::Moderated);
        assert_eq!(
            Moderation::from_moderated_flag(false),
            Moderation::Unmoderated
        );
    }
}
