//! What a remote room's traffic does to an occupancy, and the stanzas the
//! router sends for one (ADR-0020 D7's room exception; XEP-0045).
//!
//! D7 refuses account traffic across S2S in both directions, with one
//! exception each way for multi-user chat: a local account's presence and
//! messages for a remote room it occupies leave, and traffic from such a
//! room to the occupant's full JID arrives. A remote JID is a room for a
//! local resource only once two things hold: the resource sent it a join
//! (presence without a type, to an occupant JID, carrying the XEP-0045
//! §7.2 `<x xmlns='http://jabber.org/protocol/muc'/>` element), and the
//! JID's domain answered this server's XEP-0030 `disco#info` as a
//! multi-user chat service, with the `conference/text` identity or the MUC
//! feature (XEP-0045 §6.1). Every bare JID with a localpart at such a
//! service is a room or, when it does not exist yet, one a join creates
//! (§10.1). Until the domain has answered, the join is held and nothing
//! reaches the JID.
//!
//! The occupancies live in a table every replica shares
//! ([`super::occupancy`]); this module decides, for one occupancy and one
//! stanza, what is admitted and how the occupancy changes. The decisions
//! are recorded in `server/docs/planning/s2s-routing-2047.md` (outbound
//! MUC).

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use waddle_xmpp::disco::DISCO_INFO_NS;
use waddle_xmpp::muc::presence::{NS_MUC, NS_MUC_USER};
use waddle_xmpp::Stanza;
use xmpp_parsers::disco::{DiscoInfoQuery, DiscoInfoResult};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::muc::user::{Affiliation, Item, MucUser, Role, Status};
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::occupancy::{Occupancy, OccupancyPhase, RoomVerdict};

/// XEP-0045 status codes (§12) the occupancy reads.
const STATUS_SELF: &str = "110";
const STATUS_NEW_NICK: &str = "303";

/// The identity a multi-user chat service names in `disco#info` (XEP-0045
/// §6.1).
const ROOM_CATEGORY: &str = "conference";
const ROOM_TYPE: &str = "text";

/// What a local resource's message or presence for a remote JID asks of
/// the occupancy of its bare JID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RoomEgress {
    /// A join presence for `nick`, with the presence's id.
    Join { nick: String, id: Option<String> },
    /// Unavailable presence: leaving the room (XEP-0045 §7.14), also a
    /// second time.
    Leave,
    /// A presence update or a nickname change (XEP-0045 §7.6, §7.7): only
    /// while joining or joined.
    PresenceUpdate,
    /// An error presence the resource returns to the room: while the room
    /// seats it or is answering its leave.
    ErrorPresence,
    /// A message to the room or one of its occupants: only once joined.
    Message,
}

impl RoomEgress {
    /// Whether this stanza may leave for a room whose occupancy is in
    /// `phase`. Joins and leaves are decided by the router, not here.
    pub(super) fn permitted_in(&self, phase: OccupancyPhase) -> bool {
        match self {
            Self::Join { .. } | Self::Leave => true,
            Self::PresenceUpdate => phase.occupies(),
            Self::ErrorPresence => phase != OccupancyPhase::Verifying,
            Self::Message => phase == OccupancyPhase::Joined,
        }
    }
}

/// Whether `presence` is a XEP-0045 §7.2 join: no type and a `muc` child.
fn is_join(presence: &Presence) -> bool {
    presence.type_ == PresenceType::None
        && presence
            .payloads
            .iter()
            .any(|payload| payload.is("x", NS_MUC))
}

/// The room bare JID `stanza`, which a local resource addresses to the
/// remote JID `to`, is for, and what it asks of the occupancy; `None` for
/// a stanza no occupant sends a room, which is account traffic (D7).
pub(super) fn egress(to: &Jid, stanza: &Stanza) -> Option<(BareJid, RoomEgress)> {
    // A room has a localpart (XEP-0045 §4: room@service).
    to.node()?;
    let room = to.to_bare();
    let egress = match stanza {
        Stanza::Message(_) => RoomEgress::Message,
        Stanza::Presence(presence) => {
            // Presence goes to an occupant JID, never to the room's bare
            // JID (XEP-0045 §7.2.1).
            let nick = to.resource()?;
            match presence.type_ {
                PresenceType::None if is_join(presence) => RoomEgress::Join {
                    nick: nick.as_str().to_owned(),
                    id: presence.id.clone(),
                },
                PresenceType::None => RoomEgress::PresenceUpdate,
                PresenceType::Unavailable => RoomEgress::Leave,
                PresenceType::Error => RoomEgress::ErrorPresence,
                // Subscriptions and probes are account traffic.
                PresenceType::Subscribe
                | PresenceType::Subscribed
                | PresenceType::Unsubscribe
                | PresenceType::Unsubscribed
                | PresenceType::Probe => return None,
            }
        }
        Stanza::Iq(_) => return None,
    };
    Some((room, egress))
}

/// The `muc#user` element of a room's presence, if any.
fn muc_user(presence: &Presence) -> Option<&Element> {
    presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
}

/// Whether a `muc#user` element carries the status `code`.
fn has_status(user: Option<&Element>, code: &str) -> bool {
    user.is_some_and(|user| {
        user.children()
            .any(|child| child.is("status", NS_MUC_USER) && child.attr("code") == Some(code))
    })
}

/// The first `item` of a `muc#user` element.
fn item(user: Option<&Element>) -> Option<&Element> {
    user?.children().find(|child| child.is("item", NS_MUC_USER))
}

/// How a stanza from a room changes the occupancy it was admitted for.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Change {
    /// Store the occupancy as given.
    Update(Box<Occupancy>),
    /// The occupancy ended.
    Remove,
}

/// Whether `stanza`, from `from` to the occupancy's local resource, is
/// traffic from a room the resource occupies, is joining or has just left,
/// and so is admitted (ADR-0020 D7), and how it changes the occupancy.
///
/// - Nothing is admitted while the join is held for verification: the JID
///   has been sent nothing.
/// - A message from the room's bare JID, such as a challenge the room
///   sends a joining occupant, is admitted once the join left. A message
///   from one of its occupants only once the room has sent the occupant's
///   own presence (XEP-0045 §7.1 sends it before history and subject).
/// - Presence carrying `muc#user`, or an error, is admitted. The room's own
///   presence for the occupant (status 110, or the occupant JID with the
///   current nickname) moves the occupancy on: available presence
///   completes the join and records the nickname and affiliation the room
///   states (§7.2.2); unavailable presence with status 303 records the new
///   nickname (§7.6); any other own unavailable presence answers a leave
///   still pending from before a rejoin, or else ends the occupancy (left,
///   kicked, banned, removed or destroyed: §7.14, §8.2, §9.1, §10.9); an
///   error ends a join in progress (§7.2.5 to §7.2.11).
/// - IQs are never room traffic here: responses are correlated, requests
///   refused.
pub(super) fn inbound(
    occupancy: &Occupancy,
    from: &Jid,
    stanza: &Stanza,
    now_ms: i64,
) -> (bool, Option<Change>) {
    if occupancy.phase == OccupancyPhase::Leaving
        && occupancy
            .deadline_ms
            .is_some_and(|deadline| deadline <= now_ms)
    {
        return (false, Some(Change::Remove));
    }
    if occupancy.phase == OccupancyPhase::Verifying {
        return (false, None);
    }
    let presence = match stanza {
        Stanza::Message(_) if from.resource().is_none() => return (true, None),
        Stanza::Message(_) => {
            return (
                matches!(
                    occupancy.phase,
                    OccupancyPhase::Joined | OccupancyPhase::Leaving
                ),
                None,
            )
        }
        Stanza::Iq(_) => return (false, None),
        Stanza::Presence(presence) => presence,
    };
    let user = muc_user(presence);
    // Occupant presence from a room carries `muc#user` (XEP-0045 §7.2.2);
    // an error answers a join or a nickname change.
    if user.is_none() && presence.type_ != PresenceType::Error {
        return (false, None);
    }
    // XEP-0045 §7.2.2: the occupant's own presence carries status 110; a
    // service that omits it still sends it from the occupant JID.
    let own = has_status(user, STATUS_SELF)
        || from
            .resource()
            .is_some_and(|nick| nick.as_str() == occupancy.nick);
    let mut next = occupancy.clone();
    next.updated_at_ms = now_ms;
    let change = match presence.type_ {
        PresenceType::Error if occupancy.phase == OccupancyPhase::Joining => Some(Change::Remove),
        PresenceType::Error => None,
        PresenceType::None if own => {
            if let Some(nick) = from.resource() {
                next.nick = nick.as_str().to_owned();
            }
            if let Some(affiliation) = item(user)
                .and_then(|item| item.attr("affiliation"))
                .and_then(|affiliation| affiliation.parse::<Affiliation>().ok())
            {
                next.affiliation = affiliation;
            }
            if next.phase == OccupancyPhase::Joining {
                next.phase = OccupancyPhase::Joined;
                next.deadline_ms = None;
            }
            Some(Change::Update(Box::new(next)))
        }
        PresenceType::None => None,
        PresenceType::Unavailable if own => {
            let new_nick = has_status(user, STATUS_NEW_NICK)
                .then(|| item(user).and_then(|item| item.attr("nick")))
                .flatten();
            let answers_earlier_leave = occupancy.leaves_pending > 0
                && occupancy.leave_until_ms.is_some_and(|until| until > now_ms);
            match new_nick {
                Some(nick) if occupancy.phase.occupies() => {
                    next.nick = nick.to_owned();
                    Some(Change::Update(Box::new(next)))
                }
                // The answer to a leave sent before the resource joined
                // again: the room processes the join after it.
                _ if answers_earlier_leave => {
                    next.leaves_pending -= 1;
                    if next.leaves_pending == 0 {
                        next.leave_until_ms = None;
                    }
                    Some(Change::Update(Box::new(next)))
                }
                _ => Some(Change::Remove),
            }
        }
        PresenceType::Unavailable => None,
        // Subscriptions and probes are account traffic, not room traffic.
        PresenceType::Subscribe
        | PresenceType::Subscribed
        | PresenceType::Unsubscribe
        | PresenceType::Unsubscribed
        | PresenceType::Probe => return (false, None),
    };
    (true, change)
}

/// XEP-0045 §7.14: the unavailable presence that leaves `room`, from
/// `resource`, to its occupant JID.
pub(super) fn leave_presence(resource: &FullJid, room: &BareJid, nick: &str) -> Option<Stanza> {
    let occupant = room.with_resource_str(nick).ok()?;
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(resource.clone()));
    presence.to = Some(Jid::from(occupant));
    Some(Stanza::Presence(presence))
}

/// XEP-0045 §11.1 (status 333): the occupant's own unavailable presence,
/// from the room, after the stream to the room's domain failed or the
/// replica that routed its stanzas stopped. The occupant may no longer be
/// in the room, and the room may no longer reach it; the client joins
/// again when it wants the room back.
pub(super) fn removed_by_error(occupancy: &Occupancy) -> Option<Stanza> {
    let occupant = occupancy.room.with_resource_str(&occupancy.nick).ok()?;
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(occupant));
    presence.to = Some(Jid::from(occupancy.occupant.clone()));
    let user = MucUser::new()
        .with_items(vec![Item::new(occupancy.affiliation.clone(), Role::None)])
        .with_statuses(vec![Status::SelfPresence, Status::ServiceErrorKick]);
    presence.payloads.push(Element::from(user));
    Some(Stanza::Presence(presence))
}

/// The error presence answering a join that never reached the room, or
/// that the room never answered, from the occupant JID it was sent to,
/// with the join's id and the `muc` child, as a room answers a join it
/// refuses (XEP-0045 §7.2.5 onwards).
pub(super) fn join_error(occupancy: &Occupancy, error: StanzaError) -> Option<Stanza> {
    let occupant = occupancy.room.with_resource_str(&occupancy.nick).ok()?;
    let mut presence = Presence::new(PresenceType::Error);
    presence.from = Some(Jid::from(occupant));
    presence.to = Some(Jid::from(occupancy.occupant.clone()));
    presence.id = occupancy.join_id.clone();
    presence
        .payloads
        .push(Element::builder("x", NS_MUC).build());
    presence.payloads.push(Element::from(error));
    Some(Stanza::Presence(presence))
}

/// The `item-not-found` a join receives when the JID's domain is not a
/// multi-user chat service.
pub(super) fn not_a_room(home: &jid::DomainRef) -> StanzaError {
    super::replies::error(
        home,
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        "The address is not a multi-user chat room.",
    )
}

/// The `remote-server-timeout` a join receives when the JID's domain did
/// not say whether it is a multi-user chat service, or the room did not
/// answer the join, in time.
pub(super) fn unanswered(home: &jid::DomainRef) -> StanzaError {
    super::replies::error(
        home,
        ErrorType::Wait,
        DefinedCondition::RemoteServerTimeout,
        "The remote room did not answer in time.",
    )
}

/// The XEP-0030 `disco#info` request this server sends from its own
/// domain `from` to the domain JID `service`, to learn whether it is a
/// multi-user chat service (XEP-0045 §6.1). It goes to the domain, which
/// no client can answer, never to a room or an occupant JID. Asking the
/// service rather than the room lets a join create a room that does not
/// exist yet, whose own `disco#info` would answer `item-not-found`.
pub(super) fn disco_request(from: Jid, service: &BareJid, id: String) -> Stanza {
    Stanza::Iq(Box::new(Iq::Get {
        from: Some(from),
        to: Some(Jid::from(service.clone())),
        id,
        payload: Element::from(DiscoInfoQuery { node: None }),
    }))
}

/// What a remote domain's answer to [`disco_request`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VerificationAnswer {
    /// A verdict to act on and to cache.
    Verdict(RoomVerdict),
    /// No verdict: the answer said the JID could not be reached for now,
    /// or this server could not use it. Not cached.
    Unavailable,
}

/// Read a response that completed a [`disco_request`]. `usable` is
/// `false` for an error this server generated because the response
/// carried an address outside the identity profile.
pub(super) fn verification_answer(stanza: &Stanza, usable: bool) -> VerificationAnswer {
    if !usable {
        return VerificationAnswer::Unavailable;
    }
    let Stanza::Iq(iq) = stanza else {
        return VerificationAnswer::Unavailable;
    };
    match iq.as_ref() {
        Iq::Result {
            payload: Some(payload),
            ..
        } if payload.is("query", DISCO_INFO_NS) => {
            let room = DiscoInfoResult::try_from(payload.clone()).is_ok_and(|info| {
                info.identities.iter().any(|identity| {
                    identity.category == ROOM_CATEGORY && identity.type_ == ROOM_TYPE
                }) || info.features.iter().any(|feature| feature.var == NS_MUC)
            });
            VerificationAnswer::Verdict(if room {
                RoomVerdict::Room
            } else {
                RoomVerdict::NotRoom
            })
        }
        Iq::Result { .. } => VerificationAnswer::Verdict(RoomVerdict::NotRoom),
        Iq::Error { error, .. } => {
            // RFC 6120 §8.3.2: `wait` is temporary; the remote-server
            // conditions are about the route, not the JID.
            let transient = error.type_ == ErrorType::Wait
                || matches!(
                    error.defined_condition,
                    DefinedCondition::RemoteServerNotFound | DefinedCondition::RemoteServerTimeout
                );
            if transient {
                VerificationAnswer::Unavailable
            } else {
                VerificationAnswer::Verdict(RoomVerdict::NotRoom)
            }
        }
        Iq::Get { .. } | Iq::Set { .. } => VerificationAnswer::Unavailable,
    }
}
