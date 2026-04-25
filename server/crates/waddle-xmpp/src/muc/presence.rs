//! MUC Presence Types
//!
//! Types and utilities for handling MUC room join/leave presence stanzas
//! per XEP-0045.

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use tracing::debug;
use xmpp_parsers::muc::user::{
    Affiliation as MucAffiliation, Item, MucUser, Role as MucRole, Status,
};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

use crate::types::{Affiliation, Role};
use crate::XmppError;

const OCCUPANT_ID_SECRET: &[u8] = b"waddle-xmpp-occupant-id-v1";

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
            // Check for MUC element in payloads (indicates join)
            let has_muc_element = presence.payloads.iter().any(|payload| {
                // Check if this is a MUC join element
                payload.is("x", NS_MUC) || payload.is("x", NS_MUC_USER)
            });

            // Extract password and history from MUC element
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
                // Directed presence to a MUC full JID without a MUC join payload.
                // This is treated as an in-room presence update and the caller can
                // decide whether to rebroadcast (if occupant exists) or fallback to join.
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
    from_room_jid: &FullJid, // room@domain/nick of the user being announced
    to_jid: &FullJid,        // recipient's real JID
    affiliation: Affiliation,
    role: Role,
    is_self: bool, // true if this is the joining user's own presence
    occupant_real_jid: Option<&FullJid>, // real JID to include (semi-anonymous rooms)
) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    add_muc_user_payload(&mut presence, affiliation, role, is_self, occupant_real_jid);
    add_presence_identity_payloads(
        &mut presence,
        from_room_jid,
        affiliation,
        role,
        occupant_real_jid.map(|jid| jid.to_bare()),
    );

    presence
}

/// Build a rebroadcast in-room presence update with server-trusted MUC identity.
pub fn build_occupant_presence_update(
    incoming_presence: &Presence,
    from_room_jid: &FullJid,
    to_jid: &FullJid,
    affiliation: Affiliation,
    role: Role,
    is_self: bool,
    occupant_real_jid: Option<&FullJid>,
) -> Presence {
    let mut presence = incoming_presence.clone();
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));
    strip_server_controlled_presence_payloads(&mut presence);
    add_muc_user_payload(&mut presence, affiliation, role, is_self, occupant_real_jid);
    add_presence_identity_payloads(
        &mut presence,
        from_room_jid,
        affiliation,
        role,
        occupant_real_jid.map(|jid| jid.to_bare()),
    );

    presence
}

fn add_muc_user_payload(
    presence: &mut Presence,
    affiliation: Affiliation,
    role: Role,
    is_self: bool,
    occupant_real_jid: Option<&FullJid>,
) {
    // Build the MUC user element
    let mut statuses = Vec::new();

    if occupant_real_jid.is_some() {
        // Status code 100: occupants can see real JIDs in this non-anonymous room.
        statuses.push(Status::NonAnonymousRoom);
    }

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
}

fn strip_server_controlled_presence_payloads(presence: &mut Presence) {
    presence
        .payloads
        .retain(|payload| !payload.is("x", NS_MUC_USER));
    crate::xep::xep0317::strip_hats(presence);
    crate::xep::xep0421::strip_occupant_id_from_presence(presence);
}

/// Build a MUC unavailable presence for when a user leaves.
pub fn build_leave_presence(
    from_room_jid: &FullJid, // room@domain/nick of the user leaving
    to_jid: &FullJid,        // recipient's real JID
    affiliation: Affiliation,
    is_self: bool,
    occupant_real_jid: Option<&FullJid>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    // Build the MUC user element
    let mut statuses = Vec::new();

    if occupant_real_jid.is_some() {
        statuses.push(Status::NonAnonymousRoom);
    }

    if is_self {
        statuses.push(Status::SelfPresence);
    }

    // For leave, role is None
    let item = Item {
        affiliation: affiliation_to_muc(affiliation),
        role: MucRole::None,
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
    add_presence_identity_payloads(&mut presence, from_room_jid, affiliation, Role::None, None);

    presence
}

fn add_presence_identity_payloads(
    presence: &mut Presence,
    from_room_jid: &FullJid,
    affiliation: Affiliation,
    role: Role,
    occupant_bare_jid: Option<BareJid>,
) {
    let affiliation_name = match affiliation {
        Affiliation::Owner => "owner",
        Affiliation::Admin => "admin",
        Affiliation::Member => "member",
        Affiliation::None => "none",
        Affiliation::Outcast => "outcast",
    };
    let mut hats = crate::xep::xep0317::hats_from_affiliation(affiliation_name);
    if role == Role::Moderator && !hats.has_uri(crate::xep::xep0317::well_known::MODERATOR) {
        hats = hats.with_hat(crate::xep::xep0317::Hat::moderator());
    }
    crate::xep::xep0317::set_hats(presence, &hats);

    if let Some(occupant_bare_jid) = occupant_bare_jid {
        let occupant_id = crate::xep::xep0421::generate_occupant_id(
            &occupant_bare_jid.to_string(),
            &from_room_jid.to_bare().to_string(),
            OCCUPANT_ID_SECRET,
        );
        crate::xep::xep0421::set_occupant_id_on_presence(presence, &occupant_id);
    }
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
    occupant_real_jid: Option<&FullJid>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut statuses = vec![Status::Kicked];
    if occupant_real_jid.is_some() {
        statuses.push(Status::NonAnonymousRoom);
    }
    if is_self {
        statuses.push(Status::SelfPresence);
    }

    // Build actor element if provided
    // Actor is an enum with Jid(FullJid) or Nick(String) variants
    // We use the FullJid variant, adding a synthetic resource to the BareJid
    let actor_elem = actor.and_then(|a| {
        a.with_resource_str("admin")
            .ok()
            .map(xmpp_parsers::muc::user::Actor::Jid)
    });

    let item = Item {
        affiliation: affiliation_to_muc(affiliation),
        role: MucRole::None, // Kicked = role none
        jid: occupant_real_jid.cloned(),
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
    occupant_real_jid: Option<&FullJid>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut statuses = vec![Status::Banned];
    if occupant_real_jid.is_some() {
        statuses.push(Status::NonAnonymousRoom);
    }
    if is_self {
        statuses.push(Status::SelfPresence);
    }

    // Build actor element if provided
    // Actor is an enum with Jid(FullJid) or Nick(String) variants
    // We use the FullJid variant, adding a synthetic resource to the BareJid
    let actor_elem = actor.and_then(|a| {
        a.with_resource_str("admin")
            .ok()
            .map(xmpp_parsers::muc::user::Actor::Jid)
    });

    let item = Item {
        affiliation: MucAffiliation::Outcast, // Banned = outcast
        role: MucRole::None,                  // Banned = role none
        jid: occupant_real_jid.cloned(),
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
    if occupant_real_jid.is_some() {
        statuses.push(Status::NonAnonymousRoom);
    }
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
    add_presence_identity_payloads(
        &mut presence,
        from_room_jid,
        new_affiliation,
        role,
        occupant_real_jid.map(|jid| jid.to_bare()),
    );

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
    if occupant_real_jid.is_some() {
        statuses.push(Status::NonAnonymousRoom);
    }
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
    add_presence_identity_payloads(
        &mut presence,
        from_room_jid,
        affiliation,
        new_role,
        occupant_real_jid.map(|jid| jid.to_bare()),
    );

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
    fn test_parse_muc_update_without_x_element() {
        let to_jid: Jid = "room@muc.example.com/nickname".parse().unwrap();
        let mut presence = Presence::new(PresenceType::None);
        presence.to = Some(to_jid);

        let sender = make_sender_jid();
        let result = parse_muc_presence(&presence, &sender, "muc.example.com").unwrap();

        match result {
            MucPresenceAction::Update(req) => {
                assert_eq!(req.room_jid.to_string(), "room@muc.example.com");
                assert_eq!(req.nick, "nickname");
                assert_eq!(req.sender_jid, sender);
            }
            _ => panic!("Expected Update action"),
        }
    }

    #[test]
    fn test_build_occupant_presence() {
        let from: FullJid = "room@muc.example.com/joiner".parse().unwrap();
        let to: FullJid = "user@example.com/resource".parse().unwrap();
        let occupant_jid: FullJid = "joiner@example.com/desktop".parse().unwrap();

        let presence = build_occupant_presence(
            &from,
            &to,
            Affiliation::Member,
            Role::Participant,
            true, // is_self
            Some(&occupant_jid),
        );

        assert_eq!(presence.from, Some(Jid::from(from)));
        assert_eq!(presence.to, Some(Jid::from(to)));
        assert_eq!(presence.type_, PresenceType::None);
        assert!(!presence.payloads.is_empty());
        let muc_user = presence
            .payloads
            .iter()
            .find(|payload| payload.is("x", NS_MUC_USER))
            .expect("MUC user payload");
        let item = muc_user
            .get_child("item", NS_MUC_USER)
            .expect("MUC item payload");
        assert_eq!(item.attr("jid"), Some("joiner@example.com/desktop"));
        assert!(
            muc_user.children().any(|child| {
                child.is("status", NS_MUC_USER) && child.attr("code") == Some("100")
            }),
            "non-anonymous presence must include status 100"
        );
    }

    #[test]
    fn test_build_leave_presence() {
        let from: FullJid = "room@muc.example.com/leaver".parse().unwrap();
        let to: FullJid = "user@example.com/resource".parse().unwrap();
        let occupant_jid: FullJid = "leaver@example.com/phone".parse().unwrap();

        let presence =
            build_leave_presence(&from, &to, Affiliation::Member, true, Some(&occupant_jid));

        assert_eq!(presence.type_, PresenceType::Unavailable);
        assert!(!presence.payloads.is_empty());
        let muc_user = presence
            .payloads
            .iter()
            .find(|payload| payload.is("x", NS_MUC_USER))
            .expect("MUC user payload");
        let item = muc_user
            .get_child("item", NS_MUC_USER)
            .expect("MUC item payload");
        assert_eq!(item.attr("jid"), Some("leaver@example.com/phone"));
    }

    #[test]
    fn test_build_occupant_presence_update_replaces_spoofable_identity_payloads() {
        let from: FullJid = "room@muc.example.com/rawkode".parse().unwrap();
        let to: FullJid = "alice@example.com/resource".parse().unwrap();
        let occupant_jid: FullJid = "rawkode@example.com/desktop".parse().unwrap();
        let mut incoming = Presence::new(PresenceType::None);
        incoming
            .payloads
            .push(Element::builder("x", NS_MUC_USER).build());
        incoming.payloads.push(
            Element::builder("occupant-id", crate::xep::xep0421::NS_OCCUPANT_ID)
                .attr("id", "spoofed")
                .build(),
        );
        incoming
            .statuses
            .insert(String::new(), "coding".to_string());

        let presence = build_occupant_presence_update(
            &incoming,
            &from,
            &to,
            Affiliation::Member,
            Role::Participant,
            false,
            Some(&occupant_jid),
        );

        assert_eq!(presence.statuses.get(""), Some(&"coding".to_string()));
        assert_eq!(presence.from, Some(Jid::from(from)));
        assert_eq!(presence.to, Some(Jid::from(to)));
        assert_eq!(
            presence
                .payloads
                .iter()
                .filter(|payload| payload.is("x", NS_MUC_USER))
                .count(),
            1
        );
        let muc_user = presence
            .payloads
            .iter()
            .find(|payload| payload.is("x", NS_MUC_USER))
            .expect("MUC user payload");
        let item = muc_user
            .get_child("item", NS_MUC_USER)
            .expect("MUC item payload");
        assert_eq!(item.attr("jid"), Some("rawkode@example.com/desktop"));
        assert!(
            presence.payloads.iter().any(|payload| {
                payload.is("occupant-id", crate::xep::xep0421::NS_OCCUPANT_ID)
                    && payload.attr("id") != Some("spoofed")
            }),
            "server-generated occupant-id should replace spoofed client payload"
        );
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
        let muc_element = Element::builder("x", NS_MUC).append(history).build();
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
        let muc_element = Element::builder("x", NS_MUC).append(history).build();
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
