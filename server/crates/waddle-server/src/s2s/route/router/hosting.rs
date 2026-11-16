//! Hosting local rooms for remote occupants: the router's part (ADR-0020
//! D10; XEP-0045).
//!
//! Inbound, a stanza for the MUC service waits for the room host on a
//! bounded queue, in its room's order, under each remote domain's request
//! rate and share. A join opens a seat within the seat bounds before the
//! host decides it, and the seat stays only if the room seated the
//! occupant.
//!
//! Outbound, a local room's stanza leaves for a remote JID only as the
//! answer to that JID's own request or for a seat this replica holds, one
//! stanza for each seat, on a bounded queue of the seat's domain pair. A
//! stanza for a seat that cannot be queued or handed to the stream ends the
//! seat, so an occupant is never left seated behind a gap.
//!
//! A seat also ends when the room's own presence says so, when a stream of
//! its domain pair is lost, when the occupant's server answers the room
//! with a delivery error, and when the room no longer seats the occupant.

use std::sync::Arc;
use std::time::{Duration, Instant};

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::muc::presence::NS_MUC_USER;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::message::MessageType;
use xmpp_parsers::muc::user::{Affiliation, Item, MucUser, Role, Status};
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::{
    duration_ms, lock, now_ms, Allowance, Hosting, Inner, Pair, Purpose, Queued, SWEEP_BATCH,
};
use crate::permissions::host_grant::StreamEvidence;
use crate::s2s::conn::{SinkVerdict, SinkWait, VerifiedPeerStream, Waited};
use crate::s2s::route::local_rooms::{RoomRequest, SeatChange, SeatEnd};
use crate::s2s::route::principal::RemotePrincipal;
use crate::s2s::route::reach::{Reachable, RoomOperation};
use crate::s2s::route::replies;
use crate::s2s::route::room_delivery::{LocalRoomRequest, Placed, RoomWork};
use crate::s2s::route::seats::{Deferred, Seat, SeatInsert, SeatLimits, SeatStoreError};
use crate::s2s::route::telemetry;

/// XEP-0045 status codes (§12) the seat reads in the room's own presence.
const STATUS_SELF: &str = "110";
const STATUS_NEW_NICK: &str = "303";

/// Remote domains whose request allowance is kept before idle ones are
/// forgotten.
const ALLOWANCES: usize = 4096;

/// Until shutdown: hand the work of one shard of local rooms to the room
/// host, one item at a time, so a room decides its requests in the order
/// they arrived.
pub(super) async fn serve_local_rooms(router: Arc<Inner>, shard: usize) {
    loop {
        let work = tokio::select! {
            biased;
            () = router.shutdown.cancelled() => return,
            work = router.local_rooms.next(shard) => work,
        };
        match work {
            RoomWork::Request(request) => router.decide_room_request(*request).await,
            RoomWork::Unseat(ended) => {
                let (seat, end) = *ended;
                router.unseat(seat, end).await;
            }
            // Work of the queues that serve remote rooms.
            RoomWork::Traffic(_) | RoomWork::Ended(_) => {}
        }
    }
}

/// The `muc#user` element of a presence, if any.
fn muc_user(stanza: &Stanza) -> Option<&Element> {
    let Stanza::Presence(presence) = stanza else {
        return None;
    };
    presence
        .payloads
        .iter()
        .find(|payload| payload.is("x", NS_MUC_USER))
}

fn has_status(user: &Element, code: &str) -> bool {
    user.children()
        .any(|child| child.is("status", NS_MUC_USER) && child.attr("code") == Some(code))
}

/// Whether `stanza` is the room's own presence telling the occupant that
/// its occupancy ended: unavailable presence with status 110 that is not a
/// nickname change (XEP-0045 §7.14, §8.2, §9.1, §9.4, §10.9).
fn ends_seat(stanza: &Stanza) -> bool {
    let Stanza::Presence(presence) = stanza else {
        return false;
    };
    presence.type_ == PresenceType::Unavailable
        && muc_user(stanza)
            .is_some_and(|user| has_status(user, STATUS_SELF) && !has_status(user, STATUS_NEW_NICK))
}

/// The nickname the room's own presence says the occupant now holds
/// (XEP-0045 §7.6: unavailable presence with statuses 303 and 110 whose
/// item names the new nickname).
fn new_nick(stanza: &Stanza) -> Option<String> {
    let user = muc_user(stanza)?;
    if !(has_status(user, STATUS_SELF) && has_status(user, STATUS_NEW_NICK)) {
        return None;
    }
    user.children()
        .find(|child| child.is("item", NS_MUC_USER))
        .and_then(|item| item.attr("nick"))
        .map(str::to_owned)
}

/// The identifier `room`'s archive gave a message (XEP-0359), which every
/// copy of it carries.
fn archive_id(stanza: &Stanza, room: &BareJid) -> Option<String> {
    let Stanza::Message(message) = stanza else {
        return None;
    };
    waddle_xmpp_core::xep0359::extract_stanza_ids(message)
        .into_iter()
        .find(|id| id.by == *room)
        .map(|id| id.id)
}

/// The nickname a presence asks for: available presence to an occupant
/// JID (XEP-0045 §7.2). From a sender without a seat it is a join.
fn presence_nick(stanza: &Stanza, to: &Jid) -> Option<String> {
    let Stanza::Presence(presence) = stanza else {
        return None;
    };
    if presence.type_ != PresenceType::None || to.node().is_none() {
        return None;
    }
    to.resource().map(|nick| nick.as_str().to_owned())
}

/// Whether `stanza` is an occupant's server telling the room that its
/// traffic does not reach the occupant: an error with one of the
/// delivery-related conditions of XEP-0045 §17.1.2.
fn is_delivery_error(stanza: &Stanza) -> bool {
    let payloads = match stanza {
        Stanza::Presence(presence) if presence.type_ == PresenceType::Error => &presence.payloads,
        Stanza::Message(message) if message.type_ == MessageType::Error => &message.payloads,
        Stanza::Presence(_) | Stanza::Message(_) | Stanza::Iq(_) => return false,
    };
    payloads
        .iter()
        .filter(|payload| payload.name() == "error")
        .filter_map(|payload| StanzaError::try_from(payload.clone()).ok())
        .any(|error| {
            matches!(
                error.defined_condition,
                DefinedCondition::Gone { .. }
                    | DefinedCondition::ItemNotFound
                    | DefinedCondition::RecipientUnavailable
                    | DefinedCondition::Redirect { .. }
                    | DefinedCondition::RemoteServerNotFound
                    | DefinedCondition::RemoteServerTimeout
            )
        })
}

fn stanza_from(stanza: &Stanza) -> Option<&Jid> {
    replies::addresses(stanza).0
}

fn set_to(stanza: &mut Stanza, to: Jid) {
    match stanza {
        Stanza::Iq(iq) => *iq.to_mut() = Some(to),
        Stanza::Message(message) => message.to = Some(to),
        Stanza::Presence(presence) => presence.to = Some(to),
    }
}

impl Inner {
    /// What this replica lets remote senders reach beyond the server
    /// domain: the MUC service, once a room host is attached.
    pub(super) fn reachable(&self) -> Reachable {
        Reachable {
            rooms: self.hosting.get().is_some(),
        }
    }

    /// Whether peer admission admits `remote` now (ADR-0020 D1).
    pub(super) fn admits(&self, remote: &CanonicalDomain) -> bool {
        self.config.admission().decide(remote).is_ok()
    }

    fn seat_limits(&self) -> SeatLimits {
        SeatLimits {
            per_room: self.limits.seats_per_room,
            per_peer: self.limits.seats_per_peer,
            per_account: self.limits.seats_per_account,
            total: self.limits.seats,
        }
    }

    // --- inbound requests ---------------------------------------------------

    /// Queue a stanza for the MUC service for the room host. A stanza that
    /// finds its domain's rate spent or the queues full holds its stream
    /// until it fits.
    pub(super) fn room_request(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        principal: RemotePrincipal,
        operation: RoomOperation,
        stanza: Stanza,
    ) -> SinkVerdict {
        let Some(room) = replies::addresses(&stanza).1.map(Jid::to_bare) else {
            return SinkVerdict::Accepted;
        };
        let peer = stream.remote().as_domain_ref().as_str().to_owned();
        let mut work = RoomWork::Request(Box::new(LocalRoomRequest {
            principal,
            operation,
            stanza,
            local: stream.local().clone(),
            remote: stream.remote().clone(),
        }));
        let mut pause = self.take_allowance(&peer);
        if pause.is_none() {
            match self.local_rooms.try_push(room.clone(), work, &peer) {
                Ok(()) => return SinkVerdict::Accepted,
                Err(refused) => work = refused,
            }
        }
        telemetry::room_request_waited();
        let router = Arc::clone(self);
        let deadline = tokio::time::Instant::now() + self.limits.room_request_wait;
        SinkVerdict::Wait(SinkWait::new(async move {
            while let Some(wait) = pause {
                if tokio::time::Instant::now() + wait > deadline {
                    telemetry::room_request_overloaded();
                    return Waited::Overloaded;
                }
                tokio::select! {
                    biased;
                    () = router.shutdown.cancelled() => return Waited::Stopping,
                    () = tokio::time::sleep(wait) => {}
                }
                pause = router.take_allowance(&peer);
            }
            let placed = router
                .local_rooms
                .push_or_wait(room, work, &peer, deadline, &router.shutdown)
                .await;
            match placed {
                Placed::Queued => Waited::Taken,
                Placed::TimedOut => {
                    telemetry::room_request_overloaded();
                    Waited::Overloaded
                }
                Placed::Stopped => Waited::Stopping,
            }
        }))
    }

    /// Take one stanza of `peer`'s allowance for the MUC service, or say
    /// how long until its rate allows the next.
    fn take_allowance(&self, peer: &str) -> Option<Duration> {
        let rate = f64::from(self.limits.room_request_rate.max(1));
        let burst = f64::from(self.limits.room_request_burst.max(1));
        let now = Instant::now();
        let mut allowances = lock(&self.allowances);
        if allowances.len() >= ALLOWANCES && !allowances.contains_key(peer) {
            // A domain whose allowance has refilled needs no entry.
            allowances.retain(|_, allowance| {
                now.duration_since(allowance.refilled).as_secs_f64() * rate < burst
            });
        }
        let allowance = allowances.entry(peer.to_owned()).or_insert(Allowance {
            tokens: burst,
            refilled: now,
        });
        let refill = now.duration_since(allowance.refilled).as_secs_f64() * rate;
        allowance.tokens = (allowance.tokens + refill).min(burst);
        allowance.refilled = now;
        if allowance.tokens >= 1.0 {
            allowance.tokens -= 1.0;
            None
        } else {
            Some(Duration::from_secs_f64((1.0 - allowance.tokens) / rate))
        }
    }

    /// Have the room host decide one stanza and queue its answer for the
    /// sender.
    async fn decide_room_request(self: &Arc<Self>, request: LocalRoomRequest) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let LocalRoomRequest {
            principal,
            operation,
            stanza,
            local,
            remote,
        } = request;
        let Some(to) = replies::addresses(&stanza).1.cloned() else {
            return;
        };
        let room = to.to_bare();
        let pair = Pair { local, remote };
        let occupant = principal.full();
        let mut seat = occupant
            .as_ref()
            .and_then(|occupant| self.seats.get(&room, occupant));
        if let Some(held) = &seat {
            if is_delivery_error(&stanza) {
                self.unseat(held.clone(), SeatEnd::OccupantError).await;
                return;
            }
            // A seat stands for what the stream that carried its join
            // proved. A stanza a stream of other strength or trust profile
            // carries is not the seat's: the seat ends, and the stanza is
            // decided as one from an entity without a seat, so presence
            // enters again on the evidence of the stream that carries it
            // now.
            if held.evidence != StreamEvidence::of(&principal) {
                self.unseat(held.clone(), SeatEnd::EvidenceChanged).await;
                seat = None;
            }
        }
        let mut joining = false;
        if seat.is_none() {
            if let (Some(occupant), Some(nick)) = (occupant.as_ref(), presence_nick(&stanza, &to)) {
                match self
                    .open_seat(hosting, &principal, &room, occupant, nick)
                    .await
                {
                    Ok(opened) => {
                        seat = Some(opened);
                        joining = true;
                    }
                    Err(error) => {
                        if let Some(reply) = replies::error_reply(&stanza, *error) {
                            self.queue_room_reply(&pair, reply).await;
                        }
                        return;
                    }
                }
            }
        }
        let sender = principal.address().clone();
        let handled = hosting.host.handle(RoomRequest {
            principal,
            operation,
            stanza,
            seat: seat.clone(),
            joining,
        });
        let Ok(answer) = tokio::time::timeout(self.limits.room_request_timeout, handled).await
        else {
            telemetry::room_request_timed_out();
            // The room may have seated the occupant before the host was
            // abandoned.
            if let Some(opened) = seat.filter(|_| joining) {
                self.unseat(opened, SeatEnd::Undeliverable).await;
            }
            return;
        };
        // A seat stays only if the room seats the occupant: one opened
        // for a join the room did not accept is dropped, and so is one the
        // room says it does not hold any more.
        let dropped = match answer.seat {
            SeatChange::Seated => false,
            SeatChange::Refused => true,
            SeatChange::Unchanged => joining,
        };
        if dropped {
            if let Some(opened) = seat.take() {
                self.close_seat(hosting, &opened).await;
            }
            joining = false;
        }
        for mut reply in answer.stanzas {
            // Only what the MUC service sends the entity it answers leaves.
            let from_service = stanza_from(&reply).is_some_and(|from| {
                CanonicalDomain::from_domain(from.domain()).is_ok_and(|domain| domain == pair.local)
            });
            if !from_service {
                telemetry::seat_traffic_refused();
                continue;
            }
            set_to(&mut reply, sender.clone());
            let last = seat.is_some() && ends_seat(&reply);
            if let (Some(held), Some(nick)) = (&seat, new_nick(&reply)) {
                self.rename_seat(hosting, held, &nick).await;
            }
            if !self.queue_room_reply(&pair, reply).await {
                // The sender missed part of its answer: a seated occupant
                // cannot be kept in step and joins again.
                if let Some(held) = seat.take() {
                    self.unseat(held, SeatEnd::Undeliverable).await;
                }
                return;
            }
            if last {
                if let Some(held) = seat.take() {
                    self.close_seat(hosting, &held).await;
                }
            }
        }
        // The join's answer is queued: what the room sent the occupant
        // meanwhile follows it, in order.
        if let Some(held) = seat.filter(|_| joining) {
            while let Some(kept) = self
                .seats
                .settle(&held.room, &held.occupant, held.session)
                .filter(|kept| !kept.is_empty())
            {
                for stanza in kept {
                    self.queue_seat(&held, stanza);
                }
            }
        }
    }

    /// Open the seat a join asks for, within the seat bounds, and hold it
    /// while the join is decided. The error is the join's answer.
    async fn open_seat(
        &self,
        hosting: &Hosting,
        principal: &RemotePrincipal,
        room: &BareJid,
        occupant: &FullJid,
        nick: String,
    ) -> Result<Seat, Box<StanzaError>> {
        let home = self.home();
        let busy = |text: &str| {
            Box::new(replies::error(
                home,
                ErrorType::Wait,
                DefinedCondition::ResourceConstraint,
                text,
            ))
        };
        let seat = Seat {
            room: room.clone(),
            occupant: occupant.clone(),
            nick,
            session: OccupancySessionGeneration::mint(),
            evidence: StreamEvidence::of(principal),
            snapshot: principal.generation(),
            owner: self.owner.as_str().to_owned(),
            lost_at_ms: None,
            updated_at_ms: now_ms(),
        };
        let limits = self.seat_limits();
        // A row for the same room and occupant is one this replica left
        // behind or one of a replica that stopped, which is replaced, or
        // one a live replica holds, which is that replica's.
        for _ in 0..2 {
            match hosting.store.insert(&seat, &limits).await {
                Ok(SeatInsert::Inserted) => {
                    self.seats.hold_pending(seat.clone());
                    return Ok(seat);
                }
                Ok(SeatInsert::Bounds) => {
                    return Err(busy(
                        "Too many occupants from other servers; try again later.",
                    ));
                }
                Ok(SeatInsert::Exists) => {
                    let existing = hosting.store.get(room, occupant).await;
                    let stale = match existing {
                        Ok(None) => continue,
                        Ok(Some(row)) if row.owner == seat.owner => Some(row),
                        Ok(Some(_)) => {
                            let now = now_ms();
                            let before =
                                now.saturating_sub(duration_ms(self.limits.seat_owner_grace));
                            hosting
                                .store
                                .adopt(room, occupant, &seat.owner, before, now)
                                .await
                                .ok()
                                .flatten()
                        }
                        Err(_) => None,
                    };
                    let Some(stale) = stale else {
                        return Err(busy(
                            "This occupant is seated through another server node; try again later.",
                        ));
                    };
                    if hosting.store.remove(&stale).await.is_err() {
                        break;
                    }
                }
                Err(SeatStoreError::TooLong) => {
                    return Err(Box::new(replies::error(
                        home,
                        ErrorType::Modify,
                        DefinedCondition::NotAcceptable,
                        "The address or nickname is too long.",
                    )));
                }
                Err(SeatStoreError::Storage(_)) => break,
            }
        }
        tracing::warn!("opening a remote occupant's seat failed");
        Err(Box::new(replies::error(
            home,
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "The room could not be joined now; try again later.",
        )))
    }

    /// Stop holding `seat` and remove its row: the room does not seat the
    /// occupant, or has just told it that its occupancy ended.
    async fn close_seat(&self, hosting: &Hosting, seat: &Seat) {
        self.seats.release(&seat.room, &seat.occupant, seat.session);
        if hosting.store.remove(seat).await.is_err() {
            tracing::warn!("removing a remote occupant's seat failed");
        }
    }

    async fn rename_seat(&self, hosting: &Hosting, seat: &Seat, nick: &str) {
        self.seats.rename(&seat.room, &seat.occupant, nick);
        if hosting.store.rename(seat, nick, now_ms()).await.is_err() {
            tracing::debug!("recording a remote occupant's nickname failed");
        }
    }

    /// End `seat` without the room deciding it: the seat is given up and
    /// the room removes the occupant, telling those who stay (XEP-0045
    /// §11.1).
    async fn unseat(self: &Arc<Self>, seat: Seat, end: SeatEnd) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        self.seats.release(&seat.room, &seat.occupant, seat.session);
        // Where the occupant's server may still be reached and was not told
        // by a failed stream, the occupant is told that the service removed
        // it (XEP-0045 §11.1), ahead of anything a later join is answered
        // with. It is not waited for: nothing depends on its arrival.
        if matches!(
            end,
            SeatEnd::Undeliverable | SeatEnd::EvidenceChanged | SeatEnd::RoomClosed
        ) {
            self.tell_removed(&seat);
        }
        if hosting.store.remove(&seat).await.is_err() {
            tracing::warn!("removing an ended remote occupant's seat failed");
        }
        let removal = hosting.host.unseat(&seat, end);
        if tokio::time::timeout(self.limits.room_request_timeout, removal)
            .await
            .is_err()
        {
            tracing::warn!("removing an ended remote occupant from its room timed out");
        }
    }

    /// Queue the occupant's own unavailable presence with statuses 110 and
    /// 333 for a seat the service ended, if a place is free for it.
    fn tell_removed(self: &Arc<Self>, seat: &Seat) {
        let (Ok(from), Ok(local), Ok(remote)) = (
            seat.room.with_resource_str(&seat.nick),
            CanonicalDomain::from_domain(seat.room.domain()),
            CanonicalDomain::from_domain(seat.occupant.domain()),
        ) else {
            return;
        };
        let mut presence = Presence::new(PresenceType::Unavailable);
        presence.from = Some(Jid::from(from));
        presence.to = Some(Jid::from(seat.occupant.clone()));
        let user = MucUser::new()
            .with_items(vec![Item::new(Affiliation::None, Role::None)])
            .with_statuses(vec![Status::SelfPresence, Status::ServiceErrorKick]);
        presence.payloads.push(Element::from(user));
        let queued = Queued {
            stanza: Stanza::Presence(presence),
            purpose: Purpose::RoomReply,
        };
        let placed = self.reserve_on(Pair { local, remote }, self.limits.reply_queues, |queue| {
            &queue.seats
        });
        if let Some(permit) = placed {
            permit.send(queued);
        }
    }

    // --- egress -----------------------------------------------------------

    /// The seat `occupant` holds here in the room `stanza` comes from, for
    /// a stanza a room sends its occupants: presence and groupchat
    /// messages. Private messages, invitations and IQs relayed between
    /// occupants are not a room's traffic to a seat: their answers would be
    /// account traffic, which remote senders do not reach (ADR-0020 D7).
    pub(super) fn seat_of(&self, occupant: &FullJid, stanza: &Stanza) -> Option<Seat> {
        self.hosting.get()?;
        match stanza {
            Stanza::Presence(_) => {}
            Stanza::Message(message) if message.type_ == MessageType::Groupchat => {}
            Stanza::Message(_) | Stanza::Iq(_) => return None,
        }
        let room = stanza_from(stanza)?.to_bare();
        self.seats.get(&room, occupant)
    }

    pub(super) fn offer_to_seat(
        self: &Arc<Self>,
        occupant: &FullJid,
        stanza: Stanza,
    ) -> Result<(), Box<Stanza>> {
        let Some(seat) = self.seat_of(occupant, &stanza) else {
            return Err(Box::new(stanza));
        };
        match self.seats.defer(&seat.room, occupant, stanza) {
            Deferred::Ready(stanza) => self.queue_seat(&seat, stanza),
            Deferred::Kept => {}
            Deferred::Overflow => {
                self.seat_undeliverable(&seat.room, occupant, seat.session);
            }
            Deferred::NotHeld(stanza) => return Err(Box::new(stanza)),
        }
        Ok(())
    }

    /// Queue one stanza of a room for `seat`, without waiting. A copy of a
    /// room message already handed to the seat is not queued again. The
    /// room's own presence that ends the occupancy ends the seat once it
    /// is queued; a stanza that finds no place ends it too.
    fn queue_seat(self: &Arc<Self>, seat: &Seat, mut stanza: Stanza) {
        if let Some(id) = archive_id(&stanza, &seat.room) {
            if !self.seats.note_copy(&seat.room, &seat.occupant, &id) {
                return;
            }
        }
        let (Ok(local), Ok(remote)) = (
            CanonicalDomain::from_domain(seat.room.domain()),
            CanonicalDomain::from_domain(seat.occupant.domain()),
        ) else {
            return;
        };
        set_to(&mut stanza, Jid::from(seat.occupant.clone()));
        let last = ends_seat(&stanza);
        let renamed = new_nick(&stanza);
        let queued = Queued {
            stanza,
            purpose: Purpose::Seat {
                room: seat.room.clone(),
                occupant: seat.occupant.clone(),
                session: seat.session,
                last,
            },
        };
        let placed = self.reserve_on(Pair { local, remote }, self.limits.peer_queues, |queue| {
            &queue.seats
        });
        match placed {
            Some(permit) => {
                permit.send(queued);
            }
            None if last => {}
            None => {
                self.seat_undeliverable(&seat.room, &seat.occupant, seat.session);
                return;
            }
        }
        if last {
            // No later stanza of the room is for this seat.
            self.seats.release(&seat.room, &seat.occupant, seat.session);
        }
        if last || renamed.is_some() {
            let router = Arc::clone(self);
            let seat = seat.clone();
            self.tasks.spawn(async move {
                let Some(hosting) = router.hosting.get() else {
                    return;
                };
                match renamed {
                    Some(nick) if !last => router.rename_seat(hosting, &seat, &nick).await,
                    _ => router.close_seat(hosting, &seat).await,
                }
            });
        }
    }

    /// Queue the MUC service's answer to a remote entity's request for the
    /// entity's domain, waiting for a place at most the reply wait: `false`
    /// when it found none and was dropped.
    async fn queue_room_reply(self: &Arc<Self>, pair: &Pair, stanza: Stanza) -> bool {
        let deadline = tokio::time::Instant::now() + self.limits.room_reply_wait;
        let mut queued = Queued {
            stanza,
            purpose: Purpose::RoomReply,
        };
        loop {
            let placed =
                self.reserve_on(pair.clone(), self.limits.reply_queues, |queue| &queue.seats);
            if let Some(permit) = placed {
                permit.send(queued);
                return true;
            }
            // The pair has a queue that is full, or none may be opened for
            // a reply now.
            let Some(sender) = lock(&self.queues)
                .get(pair)
                .map(|queue| queue.seats.clone())
            else {
                return false;
            };
            let waited = tokio::select! {
                biased;
                () = self.shutdown.cancelled() => return false,
                waited = tokio::time::timeout_at(deadline, sender.send(queued)) => waited,
            };
            match waited {
                Ok(Ok(())) => return true,
                // The pair's worker ended meanwhile: open another.
                Ok(Err(returned)) => queued = returned.0,
                Err(_) => return false,
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// A stanza of `room` did not reach the seat `occupant` holds under
    /// `session`: give the seat up now, so that nothing more is queued for
    /// it, and have the room remove the occupant behind what it already
    /// decides.
    pub(super) fn seat_undeliverable(
        &self,
        room: &BareJid,
        occupant: &FullJid,
        session: OccupancySessionGeneration,
    ) {
        let Some(seat) = self.seats.release(room, occupant, session) else {
            return;
        };
        telemetry::seat_ended_undeliverable();
        self.local_rooms.push_exempt(
            room.clone(),
            RoomWork::Unseat(Box::new((seat, SeatEnd::Undeliverable))),
        );
    }

    // --- ends ---------------------------------------------------------------

    /// A stream between a local domain and a remote one failed: every seat
    /// of an occupant at that remote domain in a room at that local domain
    /// ends, since stanzas either way may be lost (XEP-0045 §11.1). Seats
    /// this replica holds end here; those another replica holds are marked
    /// for it.
    pub(super) fn lose_seats(self: &Arc<Self>, stream: &VerifiedPeerStream) {
        if self.hosting.get().is_none() {
            return;
        }
        let room_domain = stream.local().as_domain_ref().as_str().to_owned();
        let occupant_domain = stream.remote().as_domain_ref().as_str().to_owned();
        for seat in self.seats.of_pair(&room_domain, &occupant_domain) {
            self.seats.release(&seat.room, &seat.occupant, seat.session);
            telemetry::seat_ended_stream_lost();
            self.local_rooms.push_exempt(
                seat.room.clone(),
                RoomWork::Unseat(Box::new((seat, SeatEnd::StreamLost))),
            );
        }
        let router = Arc::clone(self);
        self.tasks.spawn(async move {
            let Some(hosting) = router.hosting.get() else {
                return;
            };
            let marked = hosting.store.mark_lost(
                &room_domain,
                &occupant_domain,
                router.owner.as_str(),
                now_ms(),
            );
            if marked.await.is_err() {
                tracing::warn!("marking the seats behind a lost stream failed");
            }
        });
    }

    /// The room stopped being open to accounts at other servers: every
    /// seat held here in it ends, behind what the room already decides.
    pub(super) fn close_room_seats(&self, room: &BareJid) {
        if self.hosting.get().is_none() {
            return;
        }
        for seat in self.seats.of_room(room) {
            self.seats.release(&seat.room, &seat.occupant, seat.session);
            telemetry::seat_ended_room_closed();
            self.local_rooms.push_exempt(
                seat.room.clone(),
                RoomWork::Unseat(Box::new((seat, SeatEnd::RoomClosed))),
            );
        }
    }

    /// End the seats of this replica whose stream another replica lost.
    pub(super) async fn sweep_lost_seats(&self) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let Ok(lost) = hosting.store.lost(self.owner.as_str(), SWEEP_BATCH).await else {
            tracing::warn!("reading the seats whose stream another replica lost failed");
            return;
        };
        for seat in lost {
            telemetry::seat_ended_stream_lost();
            self.local_rooms.push_exempt(
                seat.room.clone(),
                RoomWork::Unseat(Box::new((seat, SeatEnd::StreamLost))),
            );
        }
    }

    /// Remove the seats a stopped replica left: its rooms ended with it.
    pub(super) async fn sweep_orphaned_seats(&self, before_ms: i64) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        let Ok(orphaned) = hosting.store.orphaned(before_ms, SWEEP_BATCH).await else {
            tracing::warn!("reading the seats stopped replicas left failed");
            return;
        };
        for seat in orphaned {
            if hosting.store.remove(&seat).await.is_err() {
                tracing::warn!("removing a seat a stopped replica left failed");
            }
        }
    }

    /// Give up every seat held here whose room no longer seats the
    /// occupant: the room removed it and its own presence did not pass
    /// through this router, or the room's actor ended.
    pub(super) async fn reconcile_seats(&self) {
        let Some(hosting) = self.hosting.get() else {
            return;
        };
        for seat in self.seats.settled() {
            let asked = hosting.host.seats(&seat);
            let seated = tokio::time::timeout(self.limits.room_delivery_timeout, asked).await;
            if matches!(seated, Ok(Some(false))) {
                self.close_seat(hosting, &seat).await;
            }
        }
    }

    pub(super) async fn adopt_seat(&self, room: &BareJid, occupant: &FullJid) -> bool {
        if self.seats.holds(room, occupant) {
            return true;
        }
        let Some(hosting) = self.hosting.get() else {
            return false;
        };
        let now = now_ms();
        let before = now.saturating_sub(duration_ms(self.limits.seat_owner_grace));
        match hosting
            .store
            .adopt(room, occupant, self.owner.as_str(), before, now)
            .await
        {
            Ok(Some(seat)) => {
                self.seats.hold(seat);
                true
            }
            Ok(None) => false,
            Err(_) => {
                tracing::warn!("taking over a remote occupant's seat failed");
                false
            }
        }
    }
}
