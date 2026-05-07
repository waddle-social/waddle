use jid::{BareJid, FullJid};
use minidom::Element;
use tracing::debug;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

use crate::XmppError;

use super::NS_MUC_USER;

/// Namespace for MUC protocol (join request).
pub const NS_MUC: &str = "http://jabber.org/protocol/muc";

/// History request from a joining user (XEP-0045 §7.1.16).
#[derive(Debug, Clone, Default)]
pub struct HistoryRequest {
    /// Maximum number of stanzas to send
    pub maxstanzas: Option<u32>,
    /// Maximum number of characters to send
    pub maxchars: Option<u32>,
    /// Only send messages from the last N seconds
    pub seconds: Option<u64>,
    /// Only send messages since this timestamp (ISO 8601)
    pub since: Option<chrono::DateTime<chrono::Utc>>,
}

impl HistoryRequest {
    /// Create a default history request (server decides amount).
    pub fn default_request() -> Self {
        Self {
            maxstanzas: Some(25), // Reasonable default
            ..Default::default()
        }
    }

    /// Whether history is disabled (maxchars=0 or maxstanzas=0).
    pub fn is_disabled(&self) -> bool {
        self.maxchars == Some(0) || self.maxstanzas == Some(0)
    }
}

/// Parse a <history/> element from a MUC join presence.
fn parse_history_element(elem: &Element) -> HistoryRequest {
    let maxstanzas = elem.attr("maxstanzas").and_then(|s| s.parse().ok());
    let maxchars = elem.attr("maxchars").and_then(|s| s.parse().ok());
    let seconds = elem.attr("seconds").and_then(|s| s.parse().ok());
    let since = elem
        .attr("since")
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    HistoryRequest {
        maxstanzas,
        maxchars,
        seconds,
        since,
    }
}

/// Parsed MUC join request.
#[derive(Debug, Clone)]
pub struct MucJoinRequest {
    /// The room JID (bare)
    pub room_jid: BareJid,
    /// The requested nickname
    pub nick: String,
    /// The sender's full JID
    pub sender_jid: FullJid,
    /// Optional password for room entry
    pub password: Option<String>,
    /// Optional history request parameters
    pub history: Option<HistoryRequest>,
}

/// Parsed MUC leave request.
#[derive(Debug, Clone)]
pub struct MucLeaveRequest {
    /// The room JID (bare)
    pub room_jid: BareJid,
    /// The nickname leaving
    pub nick: String,
    /// The sender's full JID
    pub sender_jid: FullJid,
    /// Optional status message
    pub status: Option<String>,
}

/// Parsed in-room MUC presence update request.
#[derive(Debug, Clone)]
pub struct MucPresenceUpdateRequest {
    /// The room JID (bare)
    pub room_jid: BareJid,
    /// Nickname from the addressed MUC full JID resource
    pub nick: String,
    /// The sender's full JID
    pub sender_jid: FullJid,
}

/// Result of parsing a presence stanza for MUC purposes.
#[derive(Debug)]
pub enum MucPresenceAction {
    /// User is joining a room
    Join(MucJoinRequest),
    /// Occupant is updating in-room presence state
    Update(MucPresenceUpdateRequest),
    /// User is leaving a room
    Leave(MucLeaveRequest),
    /// Not a MUC presence (regular presence update)
    NotMuc,
}

/// Parse a presence stanza to determine if it's a MUC action.
///
/// A MUC join is identified by:
/// - Presence to room@muc.domain/nickname (full JID with nick as resource)
/// - Contains <x xmlns="http://jabber.org/protocol/muc"/> element
///
/// A MUC leave is identified by:
/// - Presence type="unavailable" to room@muc.domain/nickname
pub fn parse_muc_presence(
    presence: &Presence,
    sender_jid: &FullJid,
    muc_domain: &str,
) -> Result<MucPresenceAction, XmppError> {
    let to_jid = match &presence.to {
        Some(jid) => jid,
        None => return Ok(MucPresenceAction::NotMuc),
    };

    let to_full = match to_jid.clone().try_into_full() {
        Ok(full) => full,
        Err(_bare) => {
            return Ok(MucPresenceAction::NotMuc);
        }
    };

    let room_jid = to_full.to_bare();
    if room_jid.domain().as_str() != muc_domain {
        return Ok(MucPresenceAction::NotMuc);
    }

    let nick = to_full.resource().to_string();

    match presence.type_ {
        PresenceType::Unavailable => {
            let status = presence.statuses.values().next().cloned();

            debug!(
                room = %room_jid,
                nick = %nick,
                sender = %sender_jid,
                "Parsed MUC leave request"
            );

            Ok(MucPresenceAction::Leave(MucLeaveRequest {
                room_jid,
                nick,
                sender_jid: sender_jid.clone(),
                status,
            }))
        }
        PresenceType::None => {
            let has_muc_element = presence
                .payloads
                .iter()
                .any(|payload| payload.is("x", NS_MUC) || payload.is("x", NS_MUC_USER));

            let (password, history) = presence
                .payloads
                .iter()
                .find_map(|payload| {
                    if payload.is("x", NS_MUC) {
                        let password = payload.get_child("password", NS_MUC).map(|p| p.text());
                        let history = payload
                            .get_child("history", NS_MUC)
                            .map(parse_history_element);
                        Some((password, history))
                    } else {
                        None
                    }
                })
                .unwrap_or((None, None));

            if has_muc_element {
                debug!(
                    room = %room_jid,
                    nick = %nick,
                    sender = %sender_jid,
                    has_password = password.is_some(),
                    has_history = history.is_some(),
                    "Parsed MUC join request"
                );

                Ok(MucPresenceAction::Join(MucJoinRequest {
                    room_jid,
                    nick,
                    sender_jid: sender_jid.clone(),
                    password,
                    history,
                }))
            } else {
                debug!(
                    room = %room_jid,
                    nick = %nick,
                    sender = %sender_jid,
                    "Parsed MUC in-room presence update request"
                );

                Ok(MucPresenceAction::Update(MucPresenceUpdateRequest {
                    room_jid,
                    nick,
                    sender_jid: sender_jid.clone(),
                }))
            }
        }
        _ => Ok(MucPresenceAction::NotMuc),
    }
}
