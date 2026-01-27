//! MUC Presence Types
//!
//! Types and utilities for handling MUC room join/leave presence stanzas
//! per XEP-0045.

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use tracing::debug;
use xmpp_parsers::muc::user::{Affiliation as MucAffiliation, Item, MucUser, Role as MucRole, Status};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

use crate::types::{Affiliation, Role};
use crate::XmppError;

/// Namespace for MUC user protocol.
pub const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";

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
    let maxstanzas = elem.attr("maxstanzas")
        .and_then(|s| s.parse().ok());
    let maxchars = elem.attr("maxchars")
        .and_then(|s| s.parse().ok());
    let seconds = elem.attr("seconds")
        .and_then(|s| s.parse().ok());
    let since = elem.attr("since")
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

/// Result of parsing a presence stanza for MUC purposes.
#[derive(Debug)]
pub enum MucPresenceAction {
    /// User is joining a room
    Join(MucJoinRequest),
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
    // Check if presence has a 'to' attribute
    let to_jid = match &presence.to {
        Some(jid) => jid,
        None => return Ok(MucPresenceAction::NotMuc),
    };

    // Try to get a full JID (room@domain/nick)
    let to_full = match to_jid.clone().try_into_full() {
        Ok(full) => full,
        Err(_bare) => {
            // No resource means no nickname - not a MUC join/leave
            return Ok(MucPresenceAction::NotMuc);
        }
    };

    // Check if the domain is our MUC domain
    let room_jid = to_full.to_bare();
    if room_jid.domain().as_str() != muc_domain {
        return Ok(MucPresenceAction::NotMuc);
    }

    let nick = to_full.resource().to_string();

    // Check presence type
    match presence.type_ {
        PresenceType::Unavailable => {
            // This is a leave request
            let status = presence
                .statuses
                .values()
                .next()
                .cloned();

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
            // Check for MUC element in payloads (indicates join)
            let has_muc_element = presence.payloads.iter().any(|payload| {
                // Check if this is a MUC join element
                payload.is("x", NS_MUC) || payload.is("x", NS_MUC_USER)
            });

            // Extract password and history from MUC element
            let (password, history) = presence.payloads.iter().find_map(|payload| {
                if payload.is("x", NS_MUC) {
                    let password = payload
                        .get_child("password", NS_MUC)
                        .map(|p| p.text());
                    let history = payload
                        .get_child("history", NS_MUC)
                        .map(parse_history_element);
                    Some((password, history))
                } else {
                    None
                }
            }).unwrap_or((None, None));

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
                // Presence to MUC JID but without MUC element
                // Still treat as a join attempt (some clients may not include it)
                debug!(
                    room = %room_jid,
                    nick = %nick,
                    sender = %sender_jid,
                    "Parsed MUC join request (no x element)"
                );

                Ok(MucPresenceAction::Join(MucJoinRequest {
                    room_jid,
                    nick,
                    sender_jid: sender_jid.clone(),
                    password: None,
                    history: None,
                }))
            }
        }
        _ => {
            // Other presence types (error, subscribe, etc.) - not MUC join/leave
            Ok(MucPresenceAction::NotMuc)
        }
    }
}

/// An outbound MUC presence to send to an occupant.
#[derive(Debug, Clone)]
pub struct OutboundMucPresence {
    /// The recipient's full JID
    pub to: FullJid,
    /// The presence to send
    pub presence: Presence,
}

impl OutboundMucPresence {
    /// Create a new outbound presence.
    pub fn new(to: FullJid, presence: Presence) -> Self {
        Self { to, presence }
    }
}

/// Build a MUC presence response for an occupant.
///
/// Creates a presence stanza that includes the MUC user extension
/// with the occupant's role, affiliation, and appropriate status codes.
pub fn build_occupant_presence(
    from_room_jid: &FullJid,    // room@domain/nick of the user being announced
    to_jid: &FullJid,           // recipient's real JID
    affiliation: Affiliation,
    role: Role,
    is_self: bool,              // true if this is the joining user's own presence
    occupant_real_jid: Option<&FullJid>, // real JID to include (semi-anonymous rooms)
) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    // Build the MUC user element
    let mut statuses = Vec::new();

    if is_self {
        // Status code 110: self-presence (tells client this is about themselves)
        statuses.push(Status::SelfPresence);
    }

    // Build the item element
    let item = Item {
        affiliation: affiliation_to_muc(affiliation),
        role: role_to_muc(role),
        jid: occupant_real_jid.cloned(),
        nick: None,
        actor: None,
        continue_: None,
        reason: None,
    };

    let muc_user = MucUser {
        status: statuses,
        items: vec![item],
    };

    // Convert MucUser to Element and add to payloads
    let muc_element: Element = muc_user.into();
    presence.payloads.push(muc_element);

    presence
}

/// Build a MUC unavailable presence for when a user leaves.
pub fn build_leave_presence(
    from_room_jid: &FullJid,    // room@domain/nick of the user leaving
    to_jid: &FullJid,           // recipient's real JID
    affiliation: Affiliation,
    is_self: bool,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    // Build the MUC user element
    let mut statuses = Vec::new();

    if is_self {
        statuses.push(Status::SelfPresence);
    }

    // For leave, role is None
    let item = Item {
        affiliation: affiliation_to_muc(affiliation),
        role: MucRole::None,
        jid: None,
        nick: None,
        actor: None,
        continue_: None,
        reason: None,
    };

    let muc_user = MucUser {
        status: statuses,
        items: vec![item],
    };

    let muc_element: Element = muc_user.into();
    presence.payloads.push(muc_element);

    presence
}

/// Convert internal Affiliation to xmpp_parsers MUC Affiliation.
fn affiliation_to_muc(aff: Affiliation) -> MucAffiliation {
    match aff {
        Affiliation::Owner => MucAffiliation::Owner,
        Affiliation::Admin => MucAffiliation::Admin,
        Affiliation::Member => MucAffiliation::Member,
        Affiliation::None => MucAffiliation::None,
        Affiliation::Outcast => MucAffiliation::Outcast,
    }
}

/// Convert internal Role to xmpp_parsers MUC Role.
fn role_to_muc(role: Role) -> MucRole {
    match role {
        Role::Moderator => MucRole::Moderator,
        Role::Participant => MucRole::Participant,
        Role::Visitor => MucRole::Visitor,
        Role::None => MucRole::None,
    }
}

/// Build a kick presence notification (role changed to none).
///
/// Per XEP-0045 §8.2: When a user is kicked, an unavailable presence is sent
/// with status code 307 to all occupants. The kicked user also receives
/// status code 110 to indicate it's about themselves.
///
/// # Arguments
/// * `from_room_jid` - The room@domain/nick of the kicked user
/// * `to_jid` - The recipient's full JID
/// * `affiliation` - The kicked user's affiliation (unchanged by kick)
/// * `is_self` - True if this presence is going to the kicked user
/// * `reason` - Optional reason for the kick
/// * `actor` - Optional JID of who performed the kick
pub fn build_kick_presence(
    from_room_jid: &FullJid,
    to_jid: &FullJid,
    affiliation: Affiliation,
    is_self: bool,
    reason: Option<&str>,
    actor: Option<&BareJid>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut statuses = vec![Status::Kicked];
    if is_self {
        statuses.push(Status::SelfPresence);
    }

    // Build actor element if provided
    let actor_elem = actor.map(|a| xmpp_parsers::muc::user::Actor {
        jid: Some(a.clone().into()),
        nick: None,
    });

    let item = Item {
        affiliation: affiliation_to_muc(affiliation),
        role: MucRole::None, // Kicked = role none
        jid: None,
        nick: None,
        actor: actor_elem,
        continue_: None,
        reason: reason.map(|r| xmpp_parsers::muc::user::Reason(r.to_string())),
    };

    let muc_user = MucUser {
        status: statuses,
        items: vec![item],
    };

    let muc_element: Element = muc_user.into();
    presence.payloads.push(muc_element);

    presence
}

/// Build a ban presence notification (affiliation changed to outcast).
///
/// Per XEP-0045 §9.1: When a user is banned, an unavailable presence is sent
/// with status code 301 to all occupants. The banned user also receives
/// status code 110 to indicate it's about themselves.
///
/// # Arguments
/// * `from_room_jid` - The room@domain/nick of the banned user
/// * `to_jid` - The recipient's full JID
/// * `is_self` - True if this presence is going to the banned user
/// * `reason` - Optional reason for the ban
/// * `actor` - Optional JID of who performed the ban
pub fn build_ban_presence(
    from_room_jid: &FullJid,
    to_jid: &FullJid,
    is_self: bool,
    reason: Option<&str>,
    actor: Option<&BareJid>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut statuses = vec![Status::Banned];
    if is_self {
        statuses.push(Status::SelfPresence);
    }

    // Build actor element if provided
    let actor_elem = actor.map(|a| xmpp_parsers::muc::user::Actor {
        jid: Some(a.clone().into()),
        nick: None,
    });

    let item = Item {
        affiliation: MucAffiliation::Outcast, // Banned = outcast
        role: MucRole::None, // Banned = role none
        jid: None,
        nick: None,
        actor: actor_elem,
        continue_: None,
        reason: reason.map(|r| xmpp_parsers::muc::user::Reason(r.to_string())),
    };

    let muc_user = MucUser {
        status: statuses,
        items: vec![item],
    };

    let muc_element: Element = muc_user.into();
    presence.payloads.push(muc_element);

    presence
}

/// Build a presence notification for affiliation change.
///
/// Per XEP-0045 §9.6: When a user's affiliation changes, a presence update
/// is sent to all occupants showing the new affiliation.
///
/// # Arguments
/// * `from_room_jid` - The room@domain/nick of the affected user
/// * `to_jid` - The recipient's full JID
/// * `new_affiliation` - The user's new affiliation
/// * `role` - The user's current role
/// * `is_self` - True if this presence is going to the affected user
/// * `occupant_real_jid` - Optional real JID for semi-anonymous rooms
pub fn build_affiliation_change_presence(
    from_room_jid: &FullJid,
    to_jid: &FullJid,
    new_affiliation: Affiliation,
    role: Role,
    is_self: bool,
    occupant_real_jid: Option<&FullJid>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut statuses = Vec::new();
    if is_self {
        statuses.push(Status::SelfPresence);
    }

    let item = Item {
        affiliation: affiliation_to_muc(new_affiliation),
        role: role_to_muc(role),
        jid: occupant_real_jid.cloned(),
        nick: None,
        actor: None,
        continue_: None,
        reason: None,
    };

    let muc_user = MucUser {
        status: statuses,
        items: vec![item],
    };

    let muc_element: Element = muc_user.into();
    presence.payloads.push(muc_element);

    presence
}

/// Build a presence notification for role change.
///
/// Per XEP-0045 §8.4: When a user's role changes (e.g., voice granted/revoked),
/// a presence update is sent to all occupants showing the new role.
///
/// # Arguments
/// * `from_room_jid` - The room@domain/nick of the affected user
/// * `to_jid` - The recipient's full JID
/// * `affiliation` - The user's affiliation
/// * `new_role` - The user's new role
/// * `is_self` - True if this presence is going to the affected user
/// * `occupant_real_jid` - Optional real JID for semi-anonymous rooms
pub fn build_role_change_presence(
    from_room_jid: &FullJid,
    to_jid: &FullJid,
    affiliation: Affiliation,
    new_role: Role,
    is_self: bool,
    occupant_real_jid: Option<&FullJid>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut statuses = Vec::new();
    if is_self {
        statuses.push(Status::SelfPresence);
    }

    let item = Item {
        affiliation: affiliation_to_muc(affiliation),
        role: role_to_muc(new_role),
        jid: occupant_real_jid.cloned(),
        nick: None,
        actor: None,
        continue_: None,
        reason: None,
    };

    let muc_user = MucUser {
        status: statuses,
        items: vec![item],
    };

    let muc_element: Element = muc_user.into();
    presence.payloads.push(muc_element);

    presence
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sender_jid() -> FullJid {
        "user@example.com/resource".parse().unwrap()
    }

    fn make_join_presence(to: &str) -> Presence {
        let to_jid: Jid = to.parse().unwrap();
        let mut presence = Presence::new(PresenceType::None);
        presence.to = Some(to_jid);

        // Add MUC join element
        let muc_element = Element::builder("x", NS_MUC).build();
        presence.payloads.push(muc_element);

        presence
    }

    fn make_leave_presence(to: &str) -> Presence {
        let to_jid: Jid = to.parse().unwrap();
        let mut presence = Presence::new(PresenceType::Unavailable);
        presence.to = Some(to_jid);
        presence
    }

    #[test]
    fn test_parse_muc_join() {
        let presence = make_join_presence("room@muc.example.com/nickname");
        let sender = make_sender_jid();

        let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();

        match result {
            MucPresenceAction::Join(req) => {
                assert_eq!(req.room_jid.to_string(), "room@muc.example.com");
                assert_eq!(req.nick, "nickname");
                assert_eq!(req.sender_jid, sender);
                assert!(req.password.is_none());
            }
            _ => panic!("Expected Join action"),
        }
    }

    #[test]
    fn test_parse_muc_leave() {
        let presence = make_leave_presence("room@muc.example.com/nickname");
        let sender = make_sender_jid();

        let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();

        match result {
            MucPresenceAction::Leave(req) => {
                assert_eq!(req.room_jid.to_string(), "room@muc.example.com");
                assert_eq!(req.nick, "nickname");
                assert_eq!(req.sender_jid, sender);
            }
            _ => panic!("Expected Leave action"),
        }
    }

    #[test]
    fn test_parse_non_muc_presence() {
        // Presence without 'to' attribute
        let mut presence = Presence::new(PresenceType::None);
        let sender = make_sender_jid();

        let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();
        assert!(matches!(result, MucPresenceAction::NotMuc));

        // Presence to non-MUC domain
        let to_jid: Jid = "user@example.com/resource".parse().unwrap();
        presence.to = Some(to_jid);

        let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();
        assert!(matches!(result, MucPresenceAction::NotMuc));
    }

    #[test]
    fn test_parse_muc_join_without_x_element() {
        // Some clients don't include the x element
        let to_jid: Jid = "room@muc.example.com/nickname".parse().unwrap();
        let mut presence = Presence::new(PresenceType::None);
        presence.to = Some(to_jid);
        // No x element!

        let sender = make_sender_jid();
        let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();

        // Should still be treated as a join
        match result {
            MucPresenceAction::Join(req) => {
                assert_eq!(req.room_jid.to_string(), "room@muc.example.com");
                assert_eq!(req.nick, "nickname");
            }
            _ => panic!("Expected Join action"),
        }
    }

    #[test]
    fn test_build_occupant_presence() {
        let from: FullJid = "room@muc.example.com/joiner".parse().unwrap();
        let to: FullJid = "user@example.com/resource".parse().unwrap();

        let presence = build_occupant_presence(
            &from,
            &to,
            Affiliation::Member,
            Role::Participant,
            true,  // is_self
            None,
        );

        assert_eq!(presence.from, Some(Jid::from(from)));
        assert_eq!(presence.to, Some(Jid::from(to)));
        assert_eq!(presence.type_, PresenceType::None);
        assert!(!presence.payloads.is_empty());
    }

    #[test]
    fn test_build_leave_presence() {
        let from: FullJid = "room@muc.example.com/leaver".parse().unwrap();
        let to: FullJid = "user@example.com/resource".parse().unwrap();

        let presence = build_leave_presence(
            &from,
            &to,
            Affiliation::Member,
            true,
        );

        assert_eq!(presence.type_, PresenceType::Unavailable);
        assert!(!presence.payloads.is_empty());
    }

    #[test]
    fn test_parse_muc_join_with_history() {
        let to_jid: Jid = "room@muc.example.com/nickname".parse().unwrap();
        let mut presence = Presence::new(PresenceType::None);
        presence.to = Some(to_jid);

        // Add MUC element with history request
        let history = Element::builder("history", NS_MUC)
            .attr("maxstanzas", "50")
            .attr("seconds", "3600")
            .build();
        let muc_element = Element::builder("x", NS_MUC)
            .append(history)
            .build();
        presence.payloads.push(muc_element);

        let sender = make_sender_jid();
        let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();

        match result {
            MucPresenceAction::Join(req) => {
                assert!(req.history.is_some());
                let history = req.history.unwrap();
                assert_eq!(history.maxstanzas, Some(50));
                assert_eq!(history.seconds, Some(3600));
                assert!(history.maxchars.is_none());
                assert!(history.since.is_none());
            }
            _ => panic!("Expected Join action"),
        }
    }

    #[test]
    fn test_parse_muc_join_with_history_disabled() {
        let to_jid: Jid = "room@muc.example.com/nickname".parse().unwrap();
        let mut presence = Presence::new(PresenceType::None);
        presence.to = Some(to_jid);

        // Add MUC element with history disabled (maxchars=0)
        let history = Element::builder("history", NS_MUC)
            .attr("maxchars", "0")
            .build();
        let muc_element = Element::builder("x", NS_MUC)
            .append(history)
            .build();
        presence.payloads.push(muc_element);

        let sender = make_sender_jid();
        let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();

        match result {
            MucPresenceAction::Join(req) => {
                assert!(req.history.is_some());
                let history = req.history.unwrap();
                assert!(history.is_disabled());
            }
            _ => panic!("Expected Join action"),
        }
    }

    #[test]
    fn test_history_request_default() {
        let default = HistoryRequest::default_request();
        assert_eq!(default.maxstanzas, Some(25));
        assert!(!default.is_disabled());
    }
}
