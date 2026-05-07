//! MUC Presence Types
//!
//! Types and utilities for handling MUC room join/leave presence stanzas
//! per XEP-0045.

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use xmpp_parsers::muc::user::{
    Affiliation as MucAffiliation, Item, MucUser, Role as MucRole, Status,
};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

use crate::types::{Affiliation, Role};
use crate::xep::xep0421::OccupantIdentity;

mod outbound;
mod parser;
#[cfg(test)]
mod tests;

pub use outbound::OutboundMucPresence;
pub use parser::{
    parse_muc_presence, HistoryRequest, MucJoinRequest, MucLeaveRequest, MucPresenceAction,
    MucPresenceUpdateRequest, NS_MUC,
};

/// Namespace for MUC user protocol.
pub const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";

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
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    add_muc_user_payload(&mut presence, affiliation, role, is_self, identity.real_jid);
    add_presence_identity_payloads(&mut presence, from_room_jid, affiliation, role, identity);

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
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = incoming_presence.clone();
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));
    strip_server_controlled_presence_payloads(&mut presence);
    add_muc_user_payload(&mut presence, affiliation, role, is_self, identity.real_jid);
    add_presence_identity_payloads(&mut presence, from_room_jid, affiliation, role, identity);

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
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    // Build the MUC user element
    let mut statuses = Vec::new();

    if identity.real_jid.is_some() {
        statuses.push(Status::NonAnonymousRoom);
    }

    if is_self {
        statuses.push(Status::SelfPresence);
    }

    // For leave, role is None
    let item = Item {
        affiliation: affiliation_to_muc(affiliation),
        role: MucRole::None,
        jid: identity.real_jid.cloned(),
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
        Role::None,
        identity,
    );

    presence
}

fn add_presence_identity_payloads(
    presence: &mut Presence,
    from_room_jid: &FullJid,
    affiliation: Affiliation,
    role: Role,
    identity: &OccupantIdentity<'_>,
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

    // XEP-0421 §"Business Rules": occupant-id MUST be on every emitted
    // MUC presence regardless of whether the real JID is disclosed.
    // The room service knows `bare_jid` even in fully-anonymous rooms;
    // disclosure is governed independently by `identity.real_jid`.
    let occupant_id = crate::xep::xep0421::generate_occupant_id(
        identity.bare_jid,
        &from_room_jid.to_bare(),
        identity.secret,
    );
    crate::xep::xep0421::set_occupant_id_on_presence(presence, &occupant_id);
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
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut statuses = vec![Status::Kicked];
    if identity.real_jid.is_some() {
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
        jid: identity.real_jid.cloned(),
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
    add_presence_identity_payloads(
        &mut presence,
        from_room_jid,
        affiliation,
        Role::None,
        identity,
    );

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
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut statuses = vec![Status::Banned];
    if identity.real_jid.is_some() {
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
        jid: identity.real_jid.cloned(),
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
    add_presence_identity_payloads(
        &mut presence,
        from_room_jid,
        Affiliation::Outcast,
        Role::None,
        identity,
    );

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
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut statuses = Vec::new();
    if identity.real_jid.is_some() {
        statuses.push(Status::NonAnonymousRoom);
    }
    if is_self {
        statuses.push(Status::SelfPresence);
    }

    let item = Item {
        affiliation: affiliation_to_muc(new_affiliation),
        role: role_to_muc(role),
        jid: identity.real_jid.cloned(),
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
        identity,
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
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut statuses = Vec::new();
    if identity.real_jid.is_some() {
        statuses.push(Status::NonAnonymousRoom);
    }
    if is_self {
        statuses.push(Status::SelfPresence);
    }

    let item = Item {
        affiliation: affiliation_to_muc(affiliation),
        role: role_to_muc(new_role),
        jid: identity.real_jid.cloned(),
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
        identity,
    );

    presence
}
