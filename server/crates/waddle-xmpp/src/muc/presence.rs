//! MUC Presence Types
//!
//! Types and utilities for handling MUC room join/leave presence stanzas
//! per XEP-0045.

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

use crate::types::{Affiliation, Role};
use crate::xep::xep0421::OccupantIdentity;

/// Serialize an `Affiliation` to its XEP-0045 wire token. The
/// `xmpp_parsers` `MucAffiliation` enum's `IntoAttributeValue` impl
/// omits the attribute entirely when it equals the default
/// (`MucAffiliation::None`), which violates XEP-0045 §9.1.1 / §10.2
/// where `affiliation` is `Required` on `<item>`. Building the `<item>`
/// via `minidom::Element` lets us write the literal token every time.
fn affiliation_token(aff: Affiliation) -> &'static str {
    match aff {
        Affiliation::Owner => "owner",
        Affiliation::Admin => "admin",
        Affiliation::Member => "member",
        Affiliation::Outcast => "outcast",
        Affiliation::None => "none",
    }
}

fn role_token(role: Role) -> &'static str {
    match role {
        Role::Moderator => "moderator",
        Role::Participant => "participant",
        Role::Visitor => "visitor",
        Role::None => "none",
    }
}

fn muc_status_codes(
    occupant_real_jid: Option<&FullJid>,
    status: MucPresenceStatus,
) -> Vec<&'static str> {
    let mut statuses = Vec::new();
    // XEP-0045 registrar: status 100 belongs to the "Entering a room"
    // context only — it warns the ENTERING user, with their initial
    // (self) presence, that the room discloses full JIDs. Stamping it
    // on kicks/bans/leaves/broadcasts was over-stamping (#1265 item 4).
    if occupant_real_jid.is_some() && status.is_self && status.warn_nonanonymous_join {
        statuses.push("100");
    }
    if status.is_self {
        statuses.push("110");
        if status.room_created {
            statuses.push("201");
        }
    }
    statuses
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MucPresenceStatus {
    pub is_self: bool,
    pub room_created: bool,
    /// True only for the joiner's initial self-presence: adds the
    /// XEP-0045 §7.2.3 status 100 non-anonymous warning.
    pub warn_nonanonymous_join: bool,
}

impl MucPresenceStatus {
    pub const fn new(is_self: bool, warn_nonanonymous_join: bool) -> Self {
        Self {
            is_self,
            room_created: false,
            warn_nonanonymous_join,
        }
    }

    pub const fn created_self(warn_nonanonymous_join: bool) -> Self {
        Self {
            is_self: true,
            room_created: true,
            warn_nonanonymous_join,
        }
    }
}

/// Build the `<item>` child of an `<x xmlns='muc#user'>` payload with
/// the `affiliation` and `role` attributes ALWAYS present on the wire,
/// regardless of value. `xmpp_parsers`' macro-generated serializer
/// elides default attribute values, so building the element directly
/// is the only way to satisfy XEP-0045 §9.1.1 / §10.2 / §7.14, where
/// the wire form requires `role='none'` on the kicked/banned/leaving
/// occupant's item.
fn build_muc_user_item(
    affiliation: Affiliation,
    role_token: &'static str,
    occupant_real_jid: Option<&FullJid>,
    reason: Option<&str>,
    actor: Option<&BareJid>,
) -> Element {
    let mut item = Element::builder("item", NS_MUC_USER)
        .attr(
            minidom::rxml::xml_ncname!("affiliation").to_owned(),
            affiliation_token(affiliation),
        )
        .attr(minidom::rxml::xml_ncname!("role").to_owned(), role_token);
    if let Some(jid) = occupant_real_jid {
        item = item.attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            jid.to_string(),
        );
    }
    if let Some(actor_bare) = actor {
        // XEP-0045 §9.1.2 / §10.2 example: `<actor jid='admin'/>`. The
        // schema accepts either a bare JID or a `nick` attribute; we
        // emit `jid` because the room knows the kicker's real bare JID
        // via the IQ `from`.
        item = item.append(
            Element::builder("actor", NS_MUC_USER)
                .attr(
                    minidom::rxml::xml_ncname!("jid").to_owned(),
                    actor_bare.to_string(),
                )
                .build(),
        );
    }
    if let Some(reason_text) = reason {
        item = item.append(
            Element::builder("reason", NS_MUC_USER)
                .append(reason_text)
                .build(),
        );
    }
    item.build()
}

/// Build the `<x xmlns='muc#user'>` payload that wraps a single
/// occupant `<item>` plus the supplied status codes. Used by the
/// kick / ban / leave builders, all of which need
/// `affiliation` and `role` present on the wire.
fn build_muc_user_x_element(item: Element, status_codes: &[&str]) -> Element {
    let mut x = Element::builder("x", NS_MUC_USER).append(item);
    for code in status_codes {
        x = x.append(
            Element::builder("status", NS_MUC_USER)
                .attr(minidom::rxml::xml_ncname!("code").to_owned(), *code)
                .build(),
        );
    }
    x.build()
}

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
    status: MucPresenceStatus,
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    add_muc_user_payload(&mut presence, affiliation, role, status, identity.real_jid);
    add_presence_identity_payloads(&mut presence, from_room_jid, identity);

    presence
}

/// Build a rebroadcast in-room presence update with server-trusted MUC identity.
pub fn build_occupant_presence_update(
    incoming_presence: &Presence,
    from_room_jid: &FullJid,
    to_jid: &FullJid,
    affiliation: Affiliation,
    role: Role,
    status: MucPresenceStatus,
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = incoming_presence.clone();
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));
    strip_server_controlled_presence_payloads(&mut presence);
    add_muc_user_payload(&mut presence, affiliation, role, status, identity.real_jid);
    add_presence_identity_payloads(&mut presence, from_room_jid, identity);

    presence
}

fn add_muc_user_payload(
    presence: &mut Presence,
    affiliation: Affiliation,
    role: Role,
    status: MucPresenceStatus,
    occupant_real_jid: Option<&FullJid>,
) {
    let item = build_muc_user_item(affiliation, role_token(role), occupant_real_jid, None, None);
    presence.payloads.push(build_muc_user_x_element(
        item,
        &muc_status_codes(occupant_real_jid, status),
    ));
}

fn strip_server_controlled_presence_payloads(presence: &mut Presence) {
    presence
        .payloads
        .retain(|payload| !payload.is("x", NS_MUC_USER));
    crate::xep::xep0317::strip_hats(presence);
    crate::xep::xep0421::strip_occupant_id_from_presence(presence);
}

/// Build a MUC unavailable presence for when a user leaves.
///
/// Per XEP-0045 §7.14 the wire form is
/// `<presence type='unavailable' from='room/nick' to='occupant'>
///     <x xmlns='muc#user'>
///       <item affiliation='…' role='none'/>
///     </x>
///   </presence>`.
/// The `role='none'` attribute is required on the wire even though
/// it is the default for `xmpp_parsers::muc::user::Role`; we therefore
/// build the `<x>` payload via `minidom::Element` so the attribute
/// is always serialized.
pub fn build_leave_presence(
    from_room_jid: &FullJid, // room@domain/nick of the user leaving
    to_jid: &FullJid,        // recipient's real JID
    affiliation: Affiliation,
    status: MucPresenceStatus,
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut status_codes: Vec<&str> = Vec::new();
    if status.is_self {
        status_codes.push("110");
    }

    let item = build_muc_user_item(affiliation, "none", identity.real_jid, None, None);
    presence
        .payloads
        .push(build_muc_user_x_element(item, &status_codes));
    add_presence_identity_payloads(&mut presence, from_room_jid, identity);

    presence
}

fn add_presence_identity_payloads(
    presence: &mut Presence,
    from_room_jid: &FullJid,
    identity: &OccupantIdentity<'_>,
) {
    // XEP-0421 §"Business Rules": occupant-id MUST be on every emitted
    // MUC presence regardless of whether the real JID is disclosed.
    // The room service knows `bare_jid`; real-JID disclosure is governed
    // independently by `identity.real_jid`.
    //
    // No XEP-0317 hats are derived here: hats are descriptive social
    // metadata, not a duplicate of authority. MUC affiliation and role
    // are already carried by the `<x xmlns='muc#user'><item …/>`
    // payload the caller attaches above. Out-of-band descriptive hats
    // (today, only the extension-bot path) install themselves via
    // `crate::xep::xep0317::set_hats` after this helper returns.
    let occupant_id = crate::xep::xep0421::generate_occupant_id(
        identity.bare_jid,
        &from_room_jid.to_bare(),
        identity.secret,
    );
    crate::xep::xep0421::set_occupant_id_on_presence(presence, &occupant_id);
}

/// Build a kick presence notification (role changed to none).
///
/// Per XEP-0045 §9.1.1 ("Kicking an Occupant"), normative:
///
/// > The service MUST then remove the kicked occupant by sending a
/// > presence stanza of type "unavailable" to each kicked occupant,
/// > including status code 307 in the extended presence information,
/// > optionally along with the reason (if provided) and the JID of
/// > the actor who initiated the kick.
/// >
/// > The service MUST then inform all of the remaining occupants that
/// > the kicked occupant is no longer in the room by sending presence
/// > stanzas of type "unavailable" from the individual's room-nick
/// > (i.e., `<room@service/nick>`) to all the remaining occupants.
///
/// The kicked occupant additionally receives `<status code='110'/>`
/// (XEP-0045 §6.6) so the client recognises the presence as its own.
///
/// We build the `<x xmlns='muc#user'>` payload via `minidom::Element`
/// because `xmpp_parsers`' macro-generated `Item` serializer omits
/// `role='none'` when role is the default — and XEP-0045 §9.1.1
/// example shows `role='none'` is required on the wire for kicks.
///
/// # Arguments
/// * `from_room_jid` - The room@domain/nick of the kicked user
/// * `to_jid` - The recipient's full JID
/// * `affiliation` - The kicked user's affiliation (unchanged by kick)
/// * `is_self` - True if this presence is going to the kicked user
/// * `reason` - Optional reason for the kick
/// * `actor` - Optional bare JID of who performed the kick
pub fn build_kick_presence(
    from_room_jid: &FullJid,
    to_jid: &FullJid,
    affiliation: Affiliation,
    status: MucPresenceStatus,
    reason: Option<&str>,
    actor: Option<&BareJid>,
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut status_codes: Vec<&str> = vec!["307"];
    if status.is_self {
        status_codes.push("110");
    }

    let item = build_muc_user_item(affiliation, "none", identity.real_jid, reason, actor);
    presence
        .payloads
        .push(build_muc_user_x_element(item, &status_codes));
    add_presence_identity_payloads(&mut presence, from_room_jid, identity);

    presence
}

/// Build a ban presence notification (affiliation changed to outcast).
///
/// Per XEP-0045 §10.2 ("Banning a User"), normative:
///
/// > The service MUST remove the banned user from the room and inform
/// > all remaining occupants by sending presence of type "unavailable"
/// > with status code 301 from the affected occupant's room-nick.
///
/// The wire form is `<item affiliation='outcast' role='none' …>` —
/// both attributes MUST appear on the wire. We build the `<x>` payload
/// via `minidom::Element` because `xmpp_parsers`' macro-generated
/// serializer drops attributes that match their default.
///
/// # Arguments
/// * `from_room_jid` - The room@domain/nick of the banned user
/// * `to_jid` - The recipient's full JID
/// * `is_self` - True if this presence is going to the banned user
/// * `reason` - Optional reason for the ban
/// * `actor` - Optional bare JID of who performed the ban
pub fn build_ban_presence(
    from_room_jid: &FullJid,
    to_jid: &FullJid,
    status: MucPresenceStatus,
    reason: Option<&str>,
    actor: Option<&BareJid>,
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut status_codes: Vec<&str> = vec!["301"];
    if status.is_self {
        status_codes.push("110");
    }

    let item = build_muc_user_item(
        Affiliation::Outcast,
        "none",
        identity.real_jid,
        reason,
        actor,
    );
    presence
        .payloads
        .push(build_muc_user_x_element(item, &status_codes));
    add_presence_identity_payloads(&mut presence, from_room_jid, identity);

    presence
}

/// Build an unavailable presence for XEP-0045 membership removals.
///
/// Used for:
/// - status 321: a user was removed because their affiliation changed.
/// - status 322: a user was removed because the room became members-only.
pub fn build_membership_removal_presence(
    from_room_jid: &FullJid,
    to_jid: &FullJid,
    status_code: &'static str,
    status: MucPresenceStatus,
    actor: Option<&BareJid>,
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let mut status_codes: Vec<&str> = vec![status_code];
    if status.is_self {
        status_codes.push("110");
    }

    let item = build_muc_user_item(Affiliation::None, "none", identity.real_jid, None, actor);
    presence
        .payloads
        .push(build_muc_user_x_element(item, &status_codes));
    add_presence_identity_payloads(&mut presence, from_room_jid, identity);

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
/// * `identity` - Real JID and occupant-id source for the affected occupant
pub fn build_affiliation_change_presence(
    from_room_jid: &FullJid,
    to_jid: &FullJid,
    new_affiliation: Affiliation,
    role: Role,
    status: MucPresenceStatus,
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let item = build_muc_user_item(
        new_affiliation,
        role_token(role),
        identity.real_jid,
        None,
        None,
    );
    presence.payloads.push(build_muc_user_x_element(
        item,
        &muc_status_codes(identity.real_jid, status),
    ));
    add_presence_identity_payloads(&mut presence, from_room_jid, identity);

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
/// * `identity` - Real JID and occupant-id source for the affected occupant
pub fn build_role_change_presence(
    from_room_jid: &FullJid,
    to_jid: &FullJid,
    affiliation: Affiliation,
    new_role: Role,
    status: MucPresenceStatus,
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(to_jid.clone()));

    let item = build_muc_user_item(
        affiliation,
        role_token(new_role),
        identity.real_jid,
        None,
        None,
    );
    presence.payloads.push(build_muc_user_x_element(
        item,
        &muc_status_codes(identity.real_jid, status),
    ));
    add_presence_identity_payloads(&mut presence, from_room_jid, identity);

    presence
}

/// XEP-0045 §10.9 destroy request payload — the `<destroy/>` element
/// that an owner sends inside `<query xmlns='muc#owner'>`.
///
/// All fields are optional per the XEP. The reason is shown to
/// remaining occupants; alternate_venue lets clients automatically
/// follow the room elsewhere.
#[derive(Debug, Default, Clone)]
pub struct DestroyRequest {
    /// Optional reason for destruction.
    pub reason: Option<String>,
    /// Optional alternate venue JID an occupant should redirect to.
    pub alternate_venue: Option<BareJid>,
    /// Optional password for the alternate venue.
    pub password: Option<String>,
}

/// Build a XEP-0045 §10.9 destroy notification — an unavailable
/// presence with `<x xmlns='muc#user'>` carrying `<item
/// affiliation='none' role='none'/>` plus a `<destroy/>` child.
///
/// `xmpp-parsers`' `MucUser` doesn't model the `<destroy/>` child,
/// so we serialize the typed `MucUser` first (items + statuses) and
/// then append the destroy element via `minidom::Element` — XML is
/// still produced via typed builders, never via `format!` or string
/// concat.
///
/// `is_self` adds XEP-0045 status code 110 so the recipient
/// recognizes the presence as their own.
///
/// `identity` stamps the XEP-0421 `<occupant-id/>` — the Business
/// Rules require it on *every* presence sent by a MUC, and the destroy
/// notification is the occupant's final unavailable presence (#1268).
pub fn build_destroy_notification(
    room_jid: &BareJid,
    occupant_nick: &str,
    occupant_jid: &FullJid,
    destroy_request: &DestroyRequest,
    is_self: bool,
    identity: &OccupantIdentity<'_>,
) -> Presence {
    let from_room_jid = room_jid
        .with_resource_str(occupant_nick)
        .unwrap_or_else(|_| {
            room_jid
                .with_resource_str("unknown")
                .expect("literal 'unknown' is always a valid resource")
        });

    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(from_room_jid.clone()));
    presence.to = Some(Jid::from(occupant_jid.clone()));

    // The typed MucUser serializer omits affiliation='none' / role='none'
    // attributes when they're the default; XEP-0045 §10.9 shows them
    // explicitly. Build the `<x>` payload via minidom so the wire shape
    // matches the XEP example precisely.
    let mut x_elem = Element::builder("x", NS_MUC_USER).append(
        Element::builder("item", NS_MUC_USER)
            .attr(minidom::rxml::xml_ncname!("affiliation").to_owned(), "none")
            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "none")
            .build(),
    );

    // <destroy jid='alternate'><reason>…</reason><password>…</password></destroy>
    let mut destroy = Element::builder("destroy", NS_MUC_USER);
    if let Some(ref venue) = destroy_request.alternate_venue {
        destroy = destroy.attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            venue.to_string(),
        );
    }
    if let Some(ref reason) = destroy_request.reason {
        destroy = destroy.append(
            Element::builder("reason", NS_MUC_USER)
                .append(reason.as_str())
                .build(),
        );
    }
    if let Some(ref password) = destroy_request.password {
        destroy = destroy.append(
            Element::builder("password", NS_MUC_USER)
                .append(password.as_str())
                .build(),
        );
    }
    x_elem = x_elem.append(destroy.build());

    if is_self {
        x_elem = x_elem.append(
            Element::builder("status", NS_MUC_USER)
                .attr(minidom::rxml::xml_ncname!("code").to_owned(), "110")
                .build(),
        );
    }

    presence.payloads.push(x_elem.build());
    add_presence_identity_payloads(&mut presence, &from_room_jid, identity);
    presence
}
