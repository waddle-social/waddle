//! Remote rooms that local resources occupy or are joining (XEP-0045,
//! ADR-0020 D7's room exception).
//!
//! D7 refuses account traffic across S2S in both directions, with one
//! exception each way for multi-user chat: a local account's presence and
//! messages for a remote room it occupies or is joining leave, and traffic
//! from such a room to the occupant's full JID arrives. A remote room is
//! known only because a local resource joined it: the join presence (a
//! presence without a type, to an occupant JID, carrying the XEP-0045 §7.2
//! `<x xmlns='http://jabber.org/protocol/muc'/>` element) is what makes the
//! room's bare JID a room for that resource. Nothing else does.
//!
//! [`RemoteRooms`] keeps, per local full JID, the remote rooms it joined or
//! is joining, with its nickname and the phase of its occupancy. It is
//! bounded, held in memory on the replica whose router routed the join, and
//! rebuilt by the clients themselves: a client that reconnects joins its
//! rooms again (XEP-0045 §7.2). The decisions are recorded in
//! `server/docs/planning/s2s-routing-2047.md` (outbound MUC).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use jid::{BareJid, DomainRef, FullJid, Jid};
use minidom::Element;
use waddle_xmpp::muc::presence::{NS_MUC, NS_MUC_USER};
use waddle_xmpp::Stanza;
use xmpp_parsers::muc::user::{Affiliation, Item, MucUser, Role, Status};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

/// XEP-0045 status codes (§12) the occupancy reads.
const STATUS_SELF: &str = "110";
const STATUS_NEW_NICK: &str = "303";

/// Bounds on the remote rooms local resources occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RoomBounds {
    /// Rooms one local full JID may occupy or be joining at once.
    pub per_resource: usize,
    /// Rooms every local full JID on this replica may occupy or be joining
    /// at once.
    pub total: usize,
    /// How long a room a resource left keeps admitting the room's answer
    /// to the leave and whatever it sent before it.
    pub leave_grace: Duration,
}

/// Where an occupancy stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The join presence was sent; the room has not yet sent the
    /// occupant's own presence.
    Joining,
    /// The room sent the occupant's own available presence.
    Joined,
    /// The occupant sent unavailable presence. The room's answer and the
    /// traffic it sent before reading the leave are still admitted until
    /// `until`.
    Leaving { until: Instant },
}

#[derive(Debug, Clone)]
struct Occupancy {
    /// The occupant's nickname: the one requested while joining, then the
    /// one the room's own presence names (XEP-0045 §7.2.2 lets it assign
    /// another).
    nick: String,
    phase: Phase,
    /// The affiliation the room last stated in the occupant's own presence.
    affiliation: Affiliation,
}

impl Occupancy {
    fn occupies(&self) -> bool {
        matches!(self.phase, Phase::Joining | Phase::Joined)
    }
}

/// What a local resource's stanza for a remote room does to its occupancy,
/// once the stanza is on its way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RoomEgress {
    /// A join presence: the resource is now joining `room` as `nick`,
    /// unless the room already seats it.
    Join { room: BareJid, nick: String },
    /// Unavailable presence: the resource is leaving `room`.
    Leave { room: BareJid },
    /// A presence update, a nickname change, a message, or an error the
    /// resource returns to the room: the occupancy does not change.
    Traffic,
}

/// Why a local resource's message or presence for a remote JID is
/// refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoomRefusal {
    /// Not a room the resource occupies or is joining, or a stanza no
    /// occupant sends a room: account traffic, refused by D7.
    NotOccupied,
    /// Joining it would exceed the bounds on occupied rooms.
    Bounds,
}

/// An occupancy the router ended without the room: what the occupant is
/// told.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct EndedOccupancy {
    pub resource: FullJid,
    pub room: BareJid,
    pub nick: String,
    pub affiliation: Affiliation,
}

/// The remote rooms each local full JID occupies or is joining.
#[derive(Debug, Default)]
pub(super) struct RemoteRooms {
    by_resource: HashMap<FullJid, HashMap<BareJid, Occupancy>>,
    total: usize,
}

/// Whether `presence` is a XEP-0045 §7.2 join: no type and a `muc` child.
fn is_join(presence: &Presence) -> bool {
    presence.type_ == PresenceType::None
        && presence
            .payloads
            .iter()
            .any(|payload| payload.is("x", NS_MUC))
}

/// The `muc#user` element of a room's presence, if any.
fn muc_user(presence: &Presence) -> Option<&Element> {
    presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
}

/// The status codes of a `muc#user` element.
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

impl RemoteRooms {
    /// Rooms occupied or being joined, by every resource.
    pub(super) fn len(&self) -> usize {
        self.total
    }

    fn occupancy(&self, resource: &FullJid, room: &BareJid) -> Option<&Occupancy> {
        self.by_resource.get(resource)?.get(room)
    }

    /// Decide whether `stanza`, which the local resource `sender` addresses
    /// to the remote JID `to`, is room traffic D7 lets leave, and what it
    /// does to the occupancy. Nothing changes until [`Self::record`].
    pub(super) fn outbound(
        &self,
        bounds: &RoomBounds,
        sender: &FullJid,
        to: &Jid,
        stanza: &Stanza,
    ) -> Result<RoomEgress, RoomRefusal> {
        // A room has a localpart (XEP-0045 §4: room@service).
        if to.node().is_none() {
            return Err(RoomRefusal::NotOccupied);
        }
        let room = to.to_bare();
        let current = self.occupancy(sender, &room);
        match stanza {
            // A message to the room or to one of its occupants, while the
            // resource occupies or is joining the room (ADR-0020 D7).
            Stanza::Message(_) => match current {
                Some(occupancy) if occupancy.occupies() => Ok(RoomEgress::Traffic),
                _ => Err(RoomRefusal::NotOccupied),
            },
            Stanza::Presence(presence) => {
                // Presence goes to an occupant JID, never to the room's bare
                // JID (XEP-0045 §7.2.1).
                let Some(nick) = to.resource() else {
                    return Err(RoomRefusal::NotOccupied);
                };
                match presence.type_ {
                    PresenceType::None if is_join(presence) => {
                        if current.is_none() {
                            let held = self.by_resource.get(sender).map_or(0, HashMap::len);
                            if held >= bounds.per_resource || self.total >= bounds.total {
                                return Err(RoomRefusal::Bounds);
                            }
                        }
                        Ok(RoomEgress::Join {
                            room,
                            nick: nick.as_str().to_owned(),
                        })
                    }
                    // A presence update or a nickname change (XEP-0045
                    // §7.6, §7.7) from an occupant.
                    PresenceType::None => match current {
                        Some(occupancy) if occupancy.occupies() => Ok(RoomEgress::Traffic),
                        _ => Err(RoomRefusal::NotOccupied),
                    },
                    // Leaving (XEP-0045 §7.14), also a second time.
                    PresenceType::Unavailable => match current {
                        Some(_) => Ok(RoomEgress::Leave { room }),
                        None => Err(RoomRefusal::NotOccupied),
                    },
                    PresenceType::Error => match current {
                        Some(_) => Ok(RoomEgress::Traffic),
                        None => Err(RoomRefusal::NotOccupied),
                    },
                    // Subscriptions and probes are account traffic.
                    PresenceType::Subscribe
                    | PresenceType::Subscribed
                    | PresenceType::Unsubscribe
                    | PresenceType::Unsubscribed
                    | PresenceType::Probe => Err(RoomRefusal::NotOccupied),
                }
            }
            Stanza::Iq(_) => Err(RoomRefusal::NotOccupied),
        }
    }

    /// Apply `egress`, decided by [`Self::outbound`] for `sender`, once its
    /// stanza is queued.
    pub(super) fn record(
        &mut self,
        sender: &FullJid,
        egress: RoomEgress,
        now: Instant,
        grace: Duration,
    ) {
        match egress {
            RoomEgress::Join { room, nick } => {
                let rooms = self.by_resource.entry(sender.clone()).or_default();
                match rooms.get_mut(&room) {
                    // A client joins again to resynchronise, such as after
                    // reconnecting under the same full JID. The room still
                    // seats it, so the occupancy stays joined: its traffic
                    // keeps arriving, and the room's own presence updates
                    // the nickname.
                    Some(occupancy) if occupancy.phase == Phase::Joined => {}
                    Some(occupancy) => {
                        occupancy.nick = nick;
                        occupancy.phase = Phase::Joining;
                    }
                    None => {
                        rooms.insert(
                            room,
                            Occupancy {
                                nick,
                                phase: Phase::Joining,
                                affiliation: Affiliation::None,
                            },
                        );
                        self.total += 1;
                    }
                }
            }
            RoomEgress::Leave { room } => {
                if let Some(occupancy) = self
                    .by_resource
                    .get_mut(sender)
                    .and_then(|rooms| rooms.get_mut(&room))
                {
                    occupancy.phase = Phase::Leaving { until: now + grace };
                }
            }
            RoomEgress::Traffic => {}
        }
    }

    /// Forget `room` for `resource`.
    pub(super) fn forget(&mut self, resource: &FullJid, room: &BareJid) {
        let Some(rooms) = self.by_resource.get_mut(resource) else {
            return;
        };
        if rooms.remove(room).is_some() {
            self.total = self.total.saturating_sub(1);
        }
        if rooms.is_empty() {
            self.by_resource.remove(resource);
        }
    }

    /// Forget `room` for `resource` if the resource is still joining it as
    /// `nick`: the join never reached the room.
    pub(super) fn forget_join(&mut self, resource: &FullJid, room: &BareJid, nick: &str) {
        let unanswered = self
            .occupancy(resource, room)
            .is_some_and(|occupancy| occupancy.phase == Phase::Joining && occupancy.nick == nick);
        if unanswered {
            self.forget(resource, room);
        }
    }

    /// Whether `stanza`, from `from` to the local resource `to`, is traffic
    /// from a room the resource occupies, is joining or has just left, and
    /// so is admitted (ADR-0020 D7): presence carrying `muc#user` or an
    /// error, and, once the room has sent the occupant's own presence,
    /// messages of every type. IQs are never room traffic here: responses
    /// are correlated, requests refused. The room's presence moves the
    /// occupancy on: its own presence for the occupant completes a join or
    /// a nickname change, its own unavailable presence or an error for a
    /// join ends it.
    pub(super) fn inbound(
        &mut self,
        to: &FullJid,
        from: &Jid,
        stanza: &Stanza,
        now: Instant,
    ) -> bool {
        let room = from.to_bare();
        let Some(occupancy) = self
            .by_resource
            .get_mut(to)
            .and_then(|rooms| rooms.get_mut(&room))
        else {
            return false;
        };
        if let Phase::Leaving { until } = occupancy.phase {
            if until <= now {
                self.forget(to, &room);
                return false;
            }
        }
        let presence = match stanza {
            // XEP-0045 §7.1: a room sends the occupant's own presence
            // before history and subject. Until it has, nothing but room
            // presence shows that the joined JID is a room at all, so a
            // client cannot open a conversation with an account by
            // "joining" it.
            Stanza::Message(_) => return occupancy.phase != Phase::Joining,
            Stanza::Iq(_) => return false,
            Stanza::Presence(presence) => presence,
        };
        let user = muc_user(presence);
        // Occupant presence from a room carries `muc#user` (XEP-0045
        // §7.2.2); an error answers a join or a nickname change.
        if user.is_none() && presence.type_ != PresenceType::Error {
            return false;
        }
        // XEP-0045 §7.2.2: the occupant's own presence carries status 110;
        // a service that omits it still sends it from the occupant JID.
        let own = has_status(user, STATUS_SELF)
            || from
                .resource()
                .is_some_and(|nick| nick.as_str() == occupancy.nick);
        match presence.type_ {
            // XEP-0045 §7.2.5–§7.2.11: the room refused the join.
            PresenceType::Error => {
                if occupancy.phase == Phase::Joining {
                    self.forget(to, &room);
                }
                true
            }
            PresenceType::None => {
                if own {
                    if let Some(nick) = from.resource() {
                        occupancy.nick = nick.as_str().to_owned();
                    }
                    if let Some(affiliation) = item(user)
                        .and_then(|item| item.attr("affiliation"))
                        .and_then(|affiliation| affiliation.parse::<Affiliation>().ok())
                    {
                        occupancy.affiliation = affiliation;
                    }
                    if occupancy.phase == Phase::Joining {
                        occupancy.phase = Phase::Joined;
                    }
                }
                true
            }
            PresenceType::Unavailable => {
                if own {
                    // XEP-0045 §7.6: a nickname change is the old
                    // nickname's unavailable presence with status 303 and
                    // the new nickname.
                    let new_nick = has_status(user, STATUS_NEW_NICK)
                        .then(|| item(user).and_then(|item| item.attr("nick")))
                        .flatten();
                    match new_nick {
                        Some(nick) if occupancy.occupies() => occupancy.nick = nick.to_owned(),
                        // Left, kicked, banned, removed, or the room was
                        // destroyed (XEP-0045 §7.14, §8.2, §9.1, §10.9).
                        _ => self.forget(to, &room),
                    }
                }
                true
            }
            // Subscriptions and probes are account traffic, not room traffic.
            PresenceType::Subscribe
            | PresenceType::Subscribed
            | PresenceType::Unsubscribe
            | PresenceType::Unsubscribed
            | PresenceType::Probe => false,
        }
    }

    /// End every occupancy of `resource` and return the rooms it occupies
    /// or is joining, with its nickname in each, to tell them it left.
    pub(super) fn take_resource(&mut self, resource: &FullJid) -> Vec<(BareJid, String)> {
        let Some(rooms) = self.by_resource.remove(resource) else {
            return Vec::new();
        };
        self.total = self.total.saturating_sub(rooms.len());
        rooms
            .into_iter()
            .filter(|(_, occupancy)| occupancy.occupies())
            .map(|(room, occupancy)| (room, occupancy.nick))
            .collect()
    }

    /// End every occupancy of a local resource at `local` in a room at
    /// `remote`, and return those the occupant must be told about.
    pub(super) fn take_lost(
        &mut self,
        local: &DomainRef,
        remote: &DomainRef,
    ) -> Vec<EndedOccupancy> {
        let mut ended = Vec::new();
        let total = &mut self.total;
        self.by_resource.retain(|resource, rooms| {
            if resource.domain() != local {
                return true;
            }
            rooms.retain(|room, occupancy| {
                if room.domain() != remote {
                    return true;
                }
                *total = total.saturating_sub(1);
                if occupancy.occupies() {
                    ended.push(EndedOccupancy {
                        resource: resource.clone(),
                        room: room.clone(),
                        nick: occupancy.nick.clone(),
                        affiliation: occupancy.affiliation.clone(),
                    });
                }
                false
            });
            !rooms.is_empty()
        });
        ended
    }

    /// Forget rooms whose leave grace has passed.
    pub(super) fn sweep(&mut self, now: Instant) {
        let total = &mut self.total;
        self.by_resource.retain(|_, rooms| {
            rooms.retain(|_, occupancy| {
                let expired = matches!(occupancy.phase, Phase::Leaving { until } if until <= now);
                if expired {
                    *total = total.saturating_sub(1);
                }
                !expired
            });
            !rooms.is_empty()
        });
    }
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
/// from the room, after the stream to the room's domain failed. The
/// occupant may no longer be in the room, and the room can no longer reach
/// it; the client joins again when it wants the room back.
pub(super) fn removed_by_error(ended: &EndedOccupancy) -> Option<Stanza> {
    let occupant = ended.room.with_resource_str(&ended.nick).ok()?;
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(Jid::from(occupant));
    presence.to = Some(Jid::from(ended.resource.clone()));
    let user = MucUser::new()
        .with_items(vec![Item::new(ended.affiliation.clone(), Role::None)])
        .with_statuses(vec![Status::SelfPresence, Status::ServiceErrorKick]);
    presence.payloads.push(Element::from(user));
    Some(Stanza::Presence(presence))
}
