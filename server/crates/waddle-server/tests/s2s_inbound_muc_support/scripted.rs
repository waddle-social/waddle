//! `example.com` hosting rooms for two remote domains through the
//! production S2S router and connection manager, with a room host a case
//! scripts in place of the rooms of a WebSocket state.
//!
//! The router's own part of inbound MUC is what these cases look at: where
//! a request is placed, when a seat is opened, held and ended, and what
//! leaves for a remote JID. The scripted host seats and removes occupants
//! as it is told, records what the router asks of it, and can stand for a
//! room held by a replica that does not answer. `home-b.test` and
//! `home-c.test` are connection managers that keep what they receive. The
//! seat table is the deployment's in-memory SQLite database. No WebSocket
//! state, PostgreSQL, SpiceDB or second process is involved.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use async_trait::async_trait;
use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use waddle_server::db::Database;
use waddle_server::s2s::conn::ServiceOptions;
use waddle_server::s2s::route::{
    LocalRooms, RemotePrincipal, RoomOperation, RoomPlacement, RoomReplies, RoomRequest,
    RouterLimits, S2sRouter, Seat, SeatChange, SeatEnd, SqlSeatStore, HOLDER_ASK_LIMIT,
};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::MessageType;
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::s2s_inbound_muc_support::routing::transport::{self, listener, Deployment, Pki, Spec};
use crate::s2s_inbound_muc_support::routing::{
    correlation_database, send, start_routed, wait_for, CapturingSink, Routed, ARRIVES,
};
use crate::s2s_inbound_muc_support::{DOMAIN, MUC, NS_MUC, NS_MUC_USER};

/// The second remote domain of these cases.
pub const HOME_C: &str = "home-c.test";
/// Its occupant.
pub const CAROL: &str = "carol@home-c.test/laptop";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// One request the router handed the scripted host.
#[derive(Debug, Clone)]
pub struct Handled {
    pub from: Jid,
    pub id: Option<String>,
    pub operation: RoomOperation,
    pub joining: bool,
    /// The generation of the seat the request came with.
    pub session: Option<OccupancySessionGeneration>,
}

/// A room host that does what a case tells it.
#[derive(Default)]
pub struct ScriptedRooms {
    /// Whether requests are placed before they are decided, as by a host
    /// of a deployment of several replicas.
    carrying: AtomicBool,
    /// Rooms held by a replica that never answers: the host reports it as
    /// the cluster's carrier does, after its bound on the ask.
    silent: Mutex<Vec<BareJid>>,
    /// Rooms that refuse every join.
    closed: Mutex<Vec<BareJid>>,
    /// Occupants whose next request, other than a join, the host refuses.
    refused: Mutex<Vec<FullJid>>,
    /// The occupants each room seats, with nickname and generation.
    seated: Mutex<HashMap<BareJid, HashMap<FullJid, (String, OccupancySessionGeneration)>>>,
    handled: Mutex<Vec<Handled>>,
    unseated: Mutex<Vec<(FullJid, SeatEnd)>>,
    placed: AtomicUsize,
    room_asks: AtomicUsize,
    seat_asks: AtomicUsize,
    /// Whether the room sends an occupant a message while it answers the
    /// occupant's join.
    greeting: AtomicBool,
    /// Held while a case wants requests to wait in the host.
    gate: Arc<tokio::sync::RwLock<()>>,
    held: Mutex<Option<tokio::sync::OwnedRwLockWriteGuard<()>>>,
    router: OnceLock<S2sRouter>,
}

impl ScriptedRooms {
    /// Place requests before they are decided.
    pub fn carry(&self) {
        self.carrying.store(true, Ordering::Release);
    }

    /// Have every room send an occupant [`GREETING`] while its join is
    /// answered.
    pub fn greet(&self) {
        self.greeting.store(true, Ordering::Release);
    }

    /// `room` is held by a replica that never answers.
    pub fn silence(&self, room: &str) {
        lock(&self.silent).push(bare(room));
    }

    /// `room` refuses every join.
    pub fn close(&self, room: &str) {
        lock(&self.closed).push(bare(room));
    }

    /// Refuse the next request of `occupant` that is no join, as a host
    /// does for an occupant its room no longer admits.
    pub fn refuse_next(&self, occupant: &str) {
        lock(&self.refused).push(full(occupant));
    }

    /// Seat `occupant` in `room` without a join, as a room does when it
    /// seats its recorded occupants again.
    pub fn seat(&self, seat: &Seat) {
        lock(&self.seated)
            .entry(seat.room.clone())
            .or_default()
            .insert(seat.occupant.clone(), (seat.nick.clone(), seat.session));
    }

    /// Remove `occupant` from `room` without telling the router.
    pub fn forget(&self, room: &str, occupant: &str) {
        if let Some(seats) = lock(&self.seated).get_mut(&bare(room)) {
            seats.remove(&full(occupant));
        }
    }

    /// The generation `room` seats `occupant` under.
    pub fn seats_under(&self, room: &str, occupant: &str) -> Option<OccupancySessionGeneration> {
        lock(&self.seated)
            .get(&bare(room))
            .and_then(|seats| seats.get(&full(occupant)))
            .map(|(_, session)| *session)
    }

    /// Hold every request from now on in the host until [`Self::release`].
    pub async fn hold(&self) {
        let guard = Arc::clone(&self.gate).write_owned().await;
        *lock(&self.held) = Some(guard);
    }

    /// Let held requests through.
    pub fn release(&self) {
        drop(lock(&self.held).take());
    }

    pub fn handled(&self) -> Vec<Handled> {
        lock(&self.handled).clone()
    }

    /// The requests handled with `id`.
    pub fn handled_with(&self, id: &str) -> Vec<Handled> {
        self.handled()
            .into_iter()
            .filter(|handled| handled.id.as_deref() == Some(id))
            .collect()
    }

    /// The ends the router told the host of, for `occupant`.
    pub fn ends_of(&self, occupant: &str) -> Vec<SeatEnd> {
        let occupant = full(occupant);
        lock(&self.unseated)
            .iter()
            .filter(|(ended, _)| *ended == occupant)
            .map(|(_, end)| *end)
            .collect()
    }

    /// How many requests the router asked the host to place.
    pub fn placed(&self) -> usize {
        self.placed.load(Ordering::Acquire)
    }

    /// How many times the router asked for all the occupants of a room.
    pub fn room_asks(&self) -> usize {
        self.room_asks.load(Ordering::Acquire)
    }

    /// How many times the router asked whether one seat is seated.
    pub fn seat_asks(&self) -> usize {
        self.seat_asks.load(Ordering::Acquire)
    }

    fn own_presence(seat: &Seat, type_: PresenceType) -> Stanza {
        let mut presence = Presence::new(type_);
        presence.from = seat.room.with_resource_str(&seat.nick).ok().map(Jid::from);
        presence.to = Some(Jid::from(seat.occupant.clone()));
        presence.payloads.push(
            Element::builder("x", NS_MUC_USER)
                .append(
                    Element::builder("status", NS_MUC_USER)
                        .attr(minidom::rxml::xml_ncname!("code").to_owned(), "110")
                        .build(),
                )
                .build(),
        );
        Stanza::Presence(presence)
    }
}

/// The text of the message a greeting room sends.
pub const GREETING: &str = "welcome";

/// A room message for `seat`'s occupant from another occupant.
fn greeting(seat: &Seat) -> Stanza {
    let mut message = xmpp_parsers::message::Message::new_with_type(
        MessageType::Groupchat,
        Some(Jid::from(seat.occupant.clone())),
    );
    message.from = seat.room.with_resource_str("greeter").ok().map(Jid::from);
    message.bodies.insert(
        xmpp_parsers::message::Lang(String::new()),
        GREETING.to_owned(),
    );
    Stanza::Message(message)
}

/// The room's own unavailable presence for `seat`'s occupant, as a room
/// sends it when it removes the occupant.
pub fn own_unavailable(seat: &Seat) -> Stanza {
    ScriptedRooms::own_presence(seat, PresenceType::Unavailable)
}

fn to_of(stanza: &Stanza) -> Option<Jid> {
    match stanza {
        Stanza::Iq(iq) => iq.to().cloned(),
        Stanza::Message(message) => message.to.clone(),
        Stanza::Presence(presence) => presence.to.clone(),
    }
}

fn id_of(stanza: &Stanza) -> Option<String> {
    match stanza {
        Stanza::Iq(iq) => Some(iq.id().to_owned()),
        Stanza::Message(message) => message.id.as_ref().map(|id| id.0.clone()),
        Stanza::Presence(presence) => presence.id.clone(),
    }
}

fn refusal(stanza: &Stanza) -> Vec<Stanza> {
    let error = StanzaError::new(
        ErrorType::Auth,
        DefinedCondition::Forbidden,
        "en",
        "Refused by the scripted host.",
    );
    match stanza {
        Stanza::Iq(iq) => vec![Stanza::Iq(Box::new(Iq::Error {
            from: iq.to().cloned(),
            to: iq.from().cloned(),
            id: iq.id().to_owned(),
            payload: None,
            error,
        }))],
        Stanza::Presence(presence) => {
            let mut reply = Presence::new(PresenceType::Error);
            reply.from = presence.to.clone();
            reply.to = presence.from.clone();
            reply.id = presence.id.clone();
            reply.payloads.push(Element::from(error));
            vec![Stanza::Presence(reply)]
        }
        Stanza::Message(_) => Vec::new(),
    }
}

#[async_trait]
impl LocalRooms for ScriptedRooms {
    async fn handle(&self, request: RoomRequest) -> RoomReplies {
        lock(&self.handled).push(Handled {
            from: request.principal.address().clone(),
            id: id_of(&request.stanza),
            operation: request.operation,
            joining: request.joining,
            session: request.seat.as_ref().map(|seat| seat.session),
        });
        let _open = self.gate.read().await;
        let Some(to) = to_of(&request.stanza) else {
            return RoomReplies::none();
        };
        let room = to.to_bare();
        if let Some(seat) = request.seat.as_ref().filter(|_| !request.joining) {
            let mut refused = lock(&self.refused);
            if let Some(at) = refused.iter().position(|who| *who == seat.occupant) {
                refused.remove(at);
                return RoomReplies {
                    stanzas: refusal(&request.stanza),
                    seat: SeatChange::Refused,
                };
            }
        }
        match &request.stanza {
            Stanza::Presence(presence) if presence.type_ == PresenceType::None => {
                let Some(seat) = &request.seat else {
                    return RoomReplies::none();
                };
                if !request.joining {
                    let seated = lock(&self.seated)
                        .get(&room)
                        .and_then(|seats| seats.get(&seat.occupant))
                        .is_some_and(|(_, session)| *session == seat.session);
                    let enters = presence
                        .payloads
                        .iter()
                        .any(|payload| payload.is("x", NS_MUC));
                    if seated && !enters {
                        return RoomReplies::none();
                    }
                    return RoomReplies {
                        stanzas: Vec::new(),
                        seat: SeatChange::Rejoin,
                    };
                }
                if lock(&self.closed).contains(&room) {
                    return RoomReplies {
                        stanzas: refusal(&request.stanza),
                        seat: SeatChange::Refused,
                    };
                }
                self.seat(seat);
                // What the room sends the new occupant while its join is
                // answered: it must reach the occupant after the answer.
                if self.greeting.load(Ordering::Acquire) {
                    if let Some(router) = self.router.get() {
                        let _ = router.offer_to_seat(&seat.occupant, greeting(seat));
                    }
                }
                RoomReplies {
                    stanzas: vec![Self::own_presence(seat, PresenceType::None)],
                    seat: SeatChange::Seated,
                }
            }
            Stanza::Presence(presence) if presence.type_ == PresenceType::Unavailable => {
                let Some(seat) = &request.seat else {
                    return RoomReplies::none();
                };
                if let Some(seats) = lock(&self.seated).get_mut(&room) {
                    seats.remove(&seat.occupant);
                }
                RoomReplies::of(vec![Self::own_presence(seat, PresenceType::Unavailable)])
            }
            Stanza::Message(message) if message.type_ == MessageType::Groupchat => {
                let Some(seat) = &request.seat else {
                    return RoomReplies::none();
                };
                // The room's copy for the sender: stamped by the room's
                // archive, handed to the sender's seat where the room fans
                // the message out, and given again as the answer.
                let mut copy = message.clone();
                copy.from = seat.room.with_resource_str(&seat.nick).ok().map(Jid::from);
                copy.to = Some(Jid::from(seat.occupant.clone()));
                let archived = ["archived-", &id_of(&request.stanza).unwrap_or_default()].concat();
                copy.payloads.push(
                    Element::builder("stanza-id", "urn:xmpp:sid:0")
                        .attr(minidom::rxml::xml_ncname!("id").to_owned(), archived)
                        .attr(minidom::rxml::xml_ncname!("by").to_owned(), room.as_str())
                        .build(),
                );
                if let Some(router) = self.router.get() {
                    let _ = router.offer_to_seat(&seat.occupant, Stanza::Message(copy.clone()));
                }
                RoomReplies::of(vec![Stanza::Message(copy)])
            }
            Stanza::Iq(iq) => match iq.as_ref() {
                Iq::Get { .. } | Iq::Set { .. } => {
                    RoomReplies::of(vec![Stanza::Iq(Box::new(Iq::Result {
                        from: iq.to().cloned(),
                        to: iq.from().cloned(),
                        id: iq.id().to_owned(),
                        payload: None,
                    }))])
                }
                Iq::Result { .. } | Iq::Error { .. } => RoomReplies::none(),
            },
            _ => RoomReplies::none(),
        }
    }

    async fn place(&self, _principal: &RemotePrincipal, stanza: &Stanza) -> RoomPlacement {
        self.placed.fetch_add(1, Ordering::AcqRel);
        let silent = to_of(stanza)
            .map(|to| to.to_bare())
            .is_some_and(|room| lock(&self.silent).contains(&room));
        if silent {
            // What the cluster's carrier answers for a holder that does not
            // answer, once its own bound on the ask has passed: before the
            // router's placement timeout does.
            tokio::time::sleep(HOLDER_ASK_LIMIT).await;
            return RoomPlacement::Silent;
        }
        RoomPlacement::Here
    }

    async fn unseat(&self, seat: &Seat, end: SeatEnd) {
        lock(&self.unseated).push((seat.occupant.clone(), end));
        if let Some(seats) = lock(&self.seated).get_mut(&seat.room) {
            if seats
                .get(&seat.occupant)
                .is_some_and(|(_, session)| *session == seat.session)
            {
                seats.remove(&seat.occupant);
            }
        }
    }

    async fn seats(&self, seat: &Seat) -> Option<bool> {
        self.seat_asks.fetch_add(1, Ordering::AcqRel);
        Some(
            lock(&self.seated)
                .get(&seat.room)
                .and_then(|seats| seats.get(&seat.occupant))
                .is_some_and(|(_, session)| *session == seat.session),
        )
    }

    fn carries(&self) -> bool {
        self.carrying.load(Ordering::Acquire)
    }

    async fn occupancies(
        &self,
        room: &BareJid,
    ) -> Option<Vec<(FullJid, OccupancySessionGeneration)>> {
        self.room_asks.fetch_add(1, Ordering::AcqRel);
        Some(
            lock(&self.seated)
                .get(room)
                .map(|seats| {
                    seats
                        .iter()
                        .map(|(occupant, (_, session))| (occupant.clone(), *session))
                        .collect()
                })
                .unwrap_or_default(),
        )
    }
}

pub fn bare(raw: &str) -> BareJid {
    raw.parse().expect("a bare JID")
}

pub fn full(raw: &str) -> FullJid {
    raw.parse().expect("a full JID")
}

/// One remote domain beside the hosting deployment.
pub struct Peer {
    pub domain: &'static str,
    deployment: Option<Deployment>,
    pub captured: Arc<CapturingSink>,
}

impl Peer {
    /// Send `stanza` from this domain to the MUC service.
    pub async fn send(&self, stanza: impl Into<Element>) {
        let deployment = self.deployment.as_ref().expect("the peer is running");
        send(deployment, self.domain, MUC, stanza).await;
    }

    /// The first stanza this domain received that `found` accepts.
    pub async fn received(&self, found: impl Fn(&Element) -> bool) -> Option<Element> {
        wait_for(ARRIVES, || {
            self.captured
                .stanzas()
                .into_iter()
                .find(|stanza| found(stanza))
        })
        .await
    }

    /// The stanza with `id` this domain received.
    pub async fn answered(&self, id: &str) -> Element {
        wait_for(ARRIVES, || self.captured.with_id(id).into_iter().next())
            .await
            .unwrap_or_else(|| panic!("no stanza with id {id} reached {}", self.domain))
    }

    /// How many stanzas this domain received that `found` accepts.
    pub fn count(&self, found: impl Fn(&Element) -> bool) -> usize {
        self.captured
            .stanzas()
            .iter()
            .filter(|stanza| found(stanza))
            .count()
    }
}

/// `example.com`, routed, hosting rooms through a scripted host for
/// `home-b.test` and `home-c.test`.
pub struct Scripted {
    pub routed: Routed,
    pub router: S2sRouter,
    pub host: Arc<ScriptedRooms>,
    pub store: Arc<SqlSeatStore>,
    pub database: Database,
    pub home_b: Peer,
    pub home_c: Peer,
    _pki: Pki,
}

impl Scripted {
    /// Start the three deployments, the hosting one with `limits`.
    pub async fn start(name: &str, limits: RouterLimits) -> Self {
        Self::start_with(name, limits, |_| {}).await
    }

    /// [`Self::start`], with `script` applied to the host before any
    /// stream exists.
    pub async fn start_with(
        name: &str,
        limits: RouterLimits,
        script: impl FnOnce(&ScriptedRooms),
    ) -> Self {
        let pki = Pki::new();
        let (local_listener, address) = listener().await;
        let (listener_b, address_b) = listener().await;
        let (listener_c, address_c) = listener().await;
        let mut spec = Spec::new(DOMAIN, transport::HOME_B);
        spec.local.push(MUC);
        spec.allow.push(HOME_C);
        spec.private.push(HOME_C);
        spec.endpoints = vec![(transport::HOME_B, address_b), (HOME_C, address_c)];
        let database = correlation_database(name).await;
        let routed = start_routed(&pki, &spec, local_listener, database.clone(), limits).await;
        let host = Arc::new(ScriptedRooms::default());
        script(&host);
        let store = Arc::new(SqlSeatStore::new(database.clone()));
        let _ = host.router.set(routed.router.clone());
        assert!(routed.router.attach_rooms(host.clone(), store.clone()));
        let peer = |domain: &'static str| {
            let mut peer = Spec::new(domain, DOMAIN);
            peer.allow = spec.local.clone();
            peer.private = spec.local.clone();
            peer.endpoints = spec.local.iter().map(|local| (*local, address)).collect();
            peer
        };
        let captured_b = Arc::new(CapturingSink::default());
        let home_b = transport::start_with(
            &pki,
            &peer(transport::HOME_B),
            listener_b,
            ServiceOptions {
                sink: captured_b.clone(),
                ..Default::default()
            },
        )
        .await;
        let captured_c = Arc::new(CapturingSink::default());
        let home_c = transport::start_with(
            &pki,
            &peer(HOME_C),
            listener_c,
            ServiceOptions {
                sink: captured_c.clone(),
                ..Default::default()
            },
        )
        .await;
        Self {
            router: routed.router.clone(),
            routed,
            host,
            store,
            database,
            home_b: Peer {
                domain: transport::HOME_B,
                deployment: Some(home_b),
                captured: captured_b,
            },
            home_c: Peer {
                domain: HOME_C,
                deployment: Some(home_c),
                captured: captured_c,
            },
            _pki: pki,
        }
    }

    /// The seat `occupant` holds in `room` on this replica.
    pub fn seat(&self, room: &str, occupant: &str) -> Option<Seat> {
        self.router.seats().get(&bare(room), &full(occupant))
    }

    /// Wait until `occupant` holds a seat in `room`, and return it.
    pub async fn seated(&self, room: &str, occupant: &str) -> Seat {
        wait_for(ARRIVES, || self.seat(room, occupant))
            .await
            .unwrap_or_else(|| panic!("{occupant} holds no seat in {room}"))
    }

    /// Wait until `occupant` holds no seat in `room`.
    pub async fn unseated(&self, room: &str, occupant: &str) -> bool {
        wait_for(ARRIVES, || {
            self.seat(room, occupant).is_none().then_some(())
        })
        .await
        .is_some()
    }
}
