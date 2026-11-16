//! Outbound MUC across S2S for the XEP-0045 and XEP-0410 suites:
//! `home-a.test` runs the S2S router (its WebSocket clients replaced by
//! the routing fixture's recording delivery), and `home-b.test` serves
//! `muc.home-b.test` with [`FakeMuc`], a minimal room service written for
//! these suites. Waddle's own MUC does not yet accept remote occupants, so
//! the room side is this hand-written responder rather than a deployment.
//! [`outbound_muc_replicas`] runs two replicas of `home-a.test` on one
//! shared database instead, with the room service dialling the second.

#[path = "../s2s_routing_support/mod.rs"]
pub mod routing;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use waddle_server::destination::{offer_stanza, StanzaOffer, REMOTE_ROUTE_OFFER_TIMEOUT};
use waddle_server::s2s::conn::{
    S2sHandle, ServiceOptions, SinkVerdict, StanzaSink, VerifiedPeerStream,
};
use waddle_server::s2s::route::RouterLimits;
use waddle_xmpp::disco::DISCO_INFO_NS;
use waddle_xmpp::mam::MAM_NS;
use waddle_xmpp::muc::presence::{NS_MUC, NS_MUC_USER};
use waddle_xmpp::s2s::into_client_content;
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::xep::xep0297::NS_FORWARD;
use waddle_xmpp::Stanza;
use xmpp_parsers::disco::{DiscoInfoResult, Feature, Identity};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Lang, Message, MessageType};
use xmpp_parsers::muc::user::{Affiliation, Item, MucUser, Role, Status};
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

pub use routing::transport::{listener, Deployment, Pki, Spec, HOME_A, HOME_B};
pub use routing::{
    correlation_database, error_condition, full, jid, start_routed, wait_for, wait_for_async,
    wait_for_rooms, Routed, ARRIVES, QUIET,
};

/// The room service domain `home-b.test` serves.
pub const MUC_B: &str = "muc.home-b.test";
/// A room on it.
pub const ROOM: &str = "room@muc.home-b.test";
/// A room on it that refuses every join with `conflict`.
pub const BUSY: &str = "busy@muc.home-b.test";
/// A JID on it that is not a room: its `disco#info` is answered with
/// `item-not-found`.
pub const NOT_A_ROOM: &str = "notaroom@muc.home-b.test";
/// A JID on it whose `disco#info` is answered with a temporary error
/// (`wait`, `resource-constraint`).
pub const OVERLOADED: &str = "overloaded@muc.home-b.test";
/// The id of the message a room sends a join it challenges.
pub const CHALLENGE: &str = "challenge";
/// The nickname of the occupant [`FakeMuc`] seats in every room.
pub const HOST: &str = "host";

pub const ALICE_PHONE: &str = "alice@home-a.test/phone";
pub const ALICE_LAPTOP: &str = "alice@home-a.test/laptop";
pub const BOB: &str = "bob@home-b.test";
pub const BOB_PHONE: &str = "bob@home-b.test/phone";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn occupant_jid(room: &BareJid, nick: &str) -> Jid {
    Jid::from(room.with_resource_str(nick).expect("an occupant JID"))
}

/// Occupant presence from `room` as `nick`, to `to`, with `statuses`.
fn room_presence(
    room: &BareJid,
    nick: &str,
    to: &FullJid,
    type_: PresenceType,
    item: Item,
    statuses: Vec<Status>,
) -> Presence {
    let mut presence = Presence::new(type_);
    presence.from = Some(occupant_jid(room, nick));
    presence.to = Some(Jid::from(to.clone()));
    presence.payloads.push(Element::from(
        MucUser::new()
            .with_items(vec![item])
            .with_statuses(statuses),
    ));
    presence
}

fn stanza_error(condition: DefinedCondition, text: &str) -> StanzaError {
    StanzaError::new(ErrorType::Cancel, condition, "en", text)
}

/// A XEP-0030 `disco#info` result naming one identity and `features`.
fn disco_info(category: &str, type_: &str, features: &[&str]) -> Element {
    Element::from(DiscoInfoResult {
        node: None,
        identities: vec![Identity {
            category: category.to_owned(),
            type_: type_.to_owned(),
            lang: None,
            name: None,
        }],
        features: features.iter().copied().map(Feature::new).collect(),
        extensions: Vec::new(),
    })
}

/// Whether `iq` asks for `disco#info`.
fn is_disco_info_request(iq: &Iq) -> bool {
    matches!(iq, Iq::Get { payload, .. } if payload.is("query", DISCO_INFO_NS))
}

/// A reflected self-ping the room waits on: who asked, with which id, and
/// the occupant JID it asked.
#[derive(Debug, Clone)]
struct Reflected {
    requester: FullJid,
    id: String,
    occupant: Jid,
}

/// A minimal XEP-0045 room service on `muc.home-b.test`: it seats a
/// [`HOST`] occupant in every room, answers a join with the host's presence
/// and the joiner's own presence (status 110), leaves and nickname changes
/// with the occupant's own unavailable presence (status 110, and 303 for a
/// change), reflects groupchat messages to every occupant, relays private
/// messages, answers a XEP-0410 self-ping itself (or reflects it to the
/// occupant when [`FakeMuc::reflect_pings`] is set), and answers a XEP-0313
/// query with one result and its `fin`. It keeps every stanza it receives,
/// in the `jabber:client` namespace.
///
/// It also answers XEP-0030 `disco#info` for bare JIDs (XEP-0045 §6.4):
/// a JID on `muc.home-b.test` names the `conference`/`text` identity and
/// the MUC feature, except [`NOT_A_ROOM`] (`item-not-found`) and
/// [`OVERLOADED`] (a temporary error); an account at `home-b.test` names
/// the `account`/`registered` identity, as its server would.
pub struct FakeMuc {
    handle: OnceLock<S2sHandle>,
    outbox: mpsc::UnboundedSender<Element>,
    pending_outbox: Mutex<Option<mpsc::UnboundedReceiver<Element>>>,
    received: Mutex<Vec<Element>>,
    occupants: Mutex<HashMap<BareJid, HashMap<FullJid, String>>>,
    reflected: Mutex<HashMap<String, Reflected>>,
    reflect_pings: AtomicBool,
    silent_joins: AtomicBool,
    silent_disco: AtomicBool,
    challenge_joins: AtomicBool,
    /// Joins answered with a challenge, waiting for
    /// [`FakeMuc::pass_challenge`]: the nickname and the join's id.
    challenged: Mutex<HashMap<(BareJid, FullJid), (String, Option<String>)>>,
    defer_leaves: AtomicBool,
    /// Answers to leaves withheld until the occupant joins the room again.
    deferred_leaves: Mutex<Vec<(BareJid, FullJid, Presence)>>,
}

impl FakeMuc {
    fn new() -> Arc<Self> {
        let (outbox, receiver) = mpsc::unbounded_channel();
        Arc::new(Self {
            handle: OnceLock::new(),
            outbox,
            pending_outbox: Mutex::new(Some(receiver)),
            received: Mutex::new(Vec::new()),
            occupants: Mutex::new(HashMap::new()),
            reflected: Mutex::new(HashMap::new()),
            reflect_pings: AtomicBool::new(false),
            silent_joins: AtomicBool::new(false),
            silent_disco: AtomicBool::new(false),
            challenge_joins: AtomicBool::new(false),
            challenged: Mutex::new(HashMap::new()),
            defer_leaves: AtomicBool::new(false),
            deferred_leaves: Mutex::new(Vec::new()),
        })
    }

    /// Start sending through `handle`, in the order stanzas were queued,
    /// each from the domain of its `from`: `home-b.test` for its accounts,
    /// `muc.home-b.test` otherwise.
    fn attach(&self, handle: S2sHandle) {
        assert!(self.handle.set(handle.clone()).is_ok());
        let Some(mut receiver) = lock(&self.pending_outbox).take() else {
            return;
        };
        tokio::spawn(async move {
            while let Some(stanza) = receiver.recv().await {
                let from_accounts = stanza
                    .attr("from")
                    .and_then(|from| from.parse::<Jid>().ok())
                    .is_some_and(|from| from.domain().as_str() == HOME_B);
                let local = if from_accounts { HOME_B } else { MUC_B };
                let stream = handle
                    .connect(
                        &routing::transport::domain(local),
                        &routing::transport::domain(HOME_A),
                    )
                    .await
                    .expect("a stream from home-b.test");
                handle
                    .transmit(&stream, stanza)
                    .await
                    .expect("the room's stanza is written");
            }
        });
    }

    /// Send `stanza` from the room service to `home-a.test`.
    pub fn send(&self, stanza: impl Into<Element>) {
        let _ = self.outbox.send(stanza.into());
    }

    /// Reflect self-pings to the occupant instead of answering them.
    pub fn reflect_pings(&self) {
        self.reflect_pings.store(true, Ordering::SeqCst);
    }

    /// Leave further joins unanswered, as a room does whose answer is
    /// still on its way.
    pub fn silence_joins(&self) {
        self.silent_joins.store(true, Ordering::SeqCst);
    }

    /// Leave further `disco#info` requests unanswered, as a JID does that
    /// never says whether it is a room.
    pub fn silence_disco(&self) {
        self.silent_disco.store(true, Ordering::SeqCst);
    }

    /// Answer further joins with a challenge message from the room's bare
    /// JID (id [`CHALLENGE`]) and seat the occupant only once
    /// [`FakeMuc::pass_challenge`] is called, as a room with a CAPTCHA
    /// does (XEP-0045 §7.2, XEP-0158).
    pub fn challenge_joins(&self) {
        self.challenge_joins.store(true, Ordering::SeqCst);
    }

    /// Seat `occupant`, whose join to `room` was challenged: the host's
    /// presence and the occupant's own presence, with the join's id.
    pub fn pass_challenge(&self, room: &str, occupant: &str) {
        let room = room.parse::<BareJid>().expect("a room JID");
        let occupant = full(occupant);
        let challenged = lock(&self.challenged).remove(&(room.clone(), occupant.clone()));
        let (nick, id) = challenged.expect("a challenged join");
        self.seat(&room, &occupant, &nick, id);
    }

    /// Withhold the answer to further leaves until the occupant joins the
    /// same room again, then answer the leave before the join, as a room
    /// does that processes a leave and a rejoin in the order they came.
    pub fn defer_leaves(&self) {
        self.defer_leaves.store(true, Ordering::SeqCst);
    }

    /// The `disco#info` requests received, in order.
    pub fn disco_requests(&self) -> Vec<Element> {
        self.received()
            .into_iter()
            .filter(|stanza| {
                stanza.name() == "iq"
                    && stanza.attr("type") == Some("get")
                    && stanza
                        .children()
                        .any(|child| child.is("query", DISCO_INFO_NS))
            })
            .collect()
    }

    /// The `disco#info` requests received for the bare JID `to`.
    pub fn disco_requests_to(&self, to: &str) -> Vec<Element> {
        self.disco_requests()
            .into_iter()
            .filter(|request| request.attr("to") == Some(to))
            .collect()
    }

    /// The join presences received from `from` for an occupant JID of
    /// `room`.
    pub fn joins(&self, from: &str, room: &str) -> Vec<Element> {
        self.received()
            .into_iter()
            .filter(|stanza| {
                stanza.name() == "presence"
                    && stanza.attr("type").is_none()
                    && stanza.attr("from") == Some(from)
                    && stanza
                        .attr("to")
                        .and_then(|to| to.parse::<Jid>().ok())
                        .is_some_and(|to| to.to_bare().to_string() == room)
                    && stanza.children().any(|child| child.is("x", NS_MUC))
            })
            .collect()
    }

    /// Everything the room service received.
    pub fn received(&self) -> Vec<Element> {
        lock(&self.received).clone()
    }

    /// The received stanzas named `name` whose `id` is `id`.
    pub fn received_with_id(&self, name: &str, id: &str) -> Vec<Element> {
        self.received()
            .into_iter()
            .filter(|stanza| stanza.name() == name && stanza.attr("id") == Some(id))
            .collect()
    }

    /// The nickname `occupant` holds in `room`, if it is seated.
    pub fn nick_of(&self, room: &str, occupant: &str) -> Option<String> {
        lock(&self.occupants)
            .get(&room.parse::<BareJid>().ok()?)?
            .get(&full(occupant))
            .cloned()
    }

    /// Drop `occupant` from `room` without telling it, as a room does that
    /// lost track of an occupant.
    pub fn forget_occupant(&self, room: &str, occupant: &str) {
        if let Ok(room) = room.parse::<BareJid>() {
            if let Some(occupants) = lock(&self.occupants).get_mut(&room) {
                occupants.remove(&full(occupant));
            }
        }
    }

    fn respond(&self, element: Element) {
        // The room service answers what is for it; for `home-b.test`
        // itself, only `disco#info` is answered, and everything else is
        // kept and left unanswered.
        let for_rooms = element
            .attr("to")
            .and_then(|to| to.parse::<Jid>().ok())
            .is_some_and(|to| to.domain().as_str() == MUC_B);
        match element.name() {
            "presence" if for_rooms => {
                if let Ok(presence) = Presence::try_from(element) {
                    self.presence(presence);
                }
            }
            "message" if for_rooms => {
                if let Ok(message) = Message::try_from(element) {
                    self.message(message);
                }
            }
            "iq" => {
                if let Ok(iq) = Iq::try_from(element) {
                    if is_disco_info_request(&iq) {
                        self.disco(iq);
                    } else if for_rooms {
                        self.iq(iq);
                    }
                }
            }
            _ => {}
        }
    }

    /// XEP-0030 `disco#info` of a bare JID with a localpart, answered from
    /// that JID. A `disco#info` of anything else is left unanswered.
    fn disco(&self, iq: Iq) {
        let Iq::Get {
            from: Some(from),
            to: Some(to),
            id,
            ..
        } = iq
        else {
            return;
        };
        if self.silent_disco.load(Ordering::SeqCst)
            || to.node().is_none()
            || to.resource().is_some()
        {
            return;
        }
        let bare = to.to_bare().to_string();
        let answer = if to.domain().as_str() == HOME_B {
            Ok(disco_info("account", "registered", &[DISCO_INFO_NS]))
        } else if to.domain().as_str() != MUC_B {
            return;
        } else if bare == NOT_A_ROOM {
            Err(stanza_error(
                DefinedCondition::ItemNotFound,
                "No such room.",
            ))
        } else if bare == OVERLOADED {
            Err(StanzaError::new(
                ErrorType::Wait,
                DefinedCondition::ResourceConstraint,
                "en",
                "Try again later.",
            ))
        } else {
            Ok(disco_info("conference", "text", &[DISCO_INFO_NS, NS_MUC]))
        };
        match answer {
            Ok(payload) => self.send(Iq::Result {
                from: Some(to),
                to: Some(from),
                id,
                payload: Some(payload),
            }),
            Err(error) => self.send(Iq::Error {
                from: Some(to),
                to: Some(from),
                id,
                payload: None,
                error,
            }),
        }
    }

    /// Answer a join of `from` to `room` as `nick`, sent to the occupant
    /// JID `to`.
    fn join(&self, room: BareJid, from: FullJid, nick: String, to: Jid, id: Option<String>) {
        // A leave answered late goes out before the join's answer.
        let deferred: Vec<Presence> = {
            let mut deferred = lock(&self.deferred_leaves);
            let (now, later): (Vec<_>, Vec<_>) = deferred
                .drain(..)
                .partition(|(left, leaver, _)| *left == room && *leaver == from);
            *deferred = later;
            now.into_iter().map(|(_, _, answer)| answer).collect()
        };
        for answer in deferred {
            self.send(answer);
        }
        if self.silent_joins.load(Ordering::SeqCst) {
            return;
        }
        if room.to_string() == BUSY {
            let mut refusal = Presence::new(PresenceType::Error);
            refusal.from = Some(to);
            refusal.to = Some(Jid::from(from));
            refusal.id = id;
            refusal.payloads.push(Element::builder("x", NS_MUC).build());
            refusal.payloads.push(Element::from(stanza_error(
                DefinedCondition::Conflict,
                "That nickname is in use.",
            )));
            self.send(refusal);
            return;
        }
        if self.challenge_joins.load(Ordering::SeqCst) {
            lock(&self.challenged).insert((room.clone(), from.clone()), (nick, id));
            let mut challenge = Message::new(Some(Jid::from(from)));
            challenge.from = Some(Jid::from(room));
            challenge.id = Some(xmpp_parsers::message::Id(CHALLENGE.to_owned()));
            challenge.bodies.insert(
                Lang(String::new()),
                "Answer the challenge to join.".to_owned(),
            );
            self.send(challenge);
            return;
        }
        self.seat(&room, &from, &nick, id);
    }

    /// Seat `from` in `room` as `nick`: the host's presence, then the
    /// occupant's own presence with the join's id.
    fn seat(&self, room: &BareJid, from: &FullJid, nick: &str, id: Option<String>) {
        lock(&self.occupants)
            .entry(room.clone())
            .or_default()
            .insert(from.clone(), nick.to_owned());
        self.send(room_presence(
            room,
            HOST,
            from,
            PresenceType::None,
            Item::new(Affiliation::Owner, Role::Moderator),
            Vec::new(),
        ));
        let mut own = room_presence(
            room,
            nick,
            from,
            PresenceType::None,
            Item::new(Affiliation::Member, Role::Participant),
            vec![Status::SelfPresence],
        );
        own.id = id;
        self.send(own);
    }

    fn presence(&self, presence: Presence) {
        let (Some(Ok(from)), Some(to)) = (
            presence.from.clone().map(Jid::try_into_full),
            presence.to.clone(),
        ) else {
            return;
        };
        let room = to.to_bare();
        let Some(nick) = to.resource().map(|nick| nick.as_str().to_owned()) else {
            return;
        };
        let joining = presence
            .payloads
            .iter()
            .any(|payload| payload.is("x", NS_MUC));
        match presence.type_ {
            PresenceType::None if joining => {
                self.join(room, from, nick, to, presence.id.clone());
            }
            PresenceType::None => {
                let previous = lock(&self.occupants)
                    .get_mut(&room)
                    .and_then(|occupants| occupants.insert(from.clone(), nick.clone()));
                let Some(previous) = previous else {
                    return;
                };
                if previous != nick {
                    // XEP-0045 §7.6.
                    self.send(room_presence(
                        &room,
                        &previous,
                        &from,
                        PresenceType::Unavailable,
                        Item::new(Affiliation::Member, Role::Participant).with_nick(nick.clone()),
                        vec![Status::NewNick, Status::SelfPresence],
                    ));
                }
                self.send(room_presence(
                    &room,
                    &nick,
                    &from,
                    PresenceType::None,
                    Item::new(Affiliation::Member, Role::Participant),
                    vec![Status::SelfPresence],
                ));
            }
            PresenceType::Unavailable => {
                let seated = lock(&self.occupants)
                    .get_mut(&room)
                    .and_then(|occupants| occupants.remove(&from));
                if let Some(seated) = seated {
                    let mut own = room_presence(
                        &room,
                        &seated,
                        &from,
                        PresenceType::Unavailable,
                        Item::new(Affiliation::Member, Role::None),
                        vec![Status::SelfPresence],
                    );
                    own.id = presence.id.clone();
                    if self.defer_leaves.load(Ordering::SeqCst) {
                        lock(&self.deferred_leaves).push((room, from, own));
                    } else {
                        self.send(own);
                    }
                }
            }
            _ => {}
        }
    }

    fn message(&self, message: Message) {
        let (Some(Ok(from)), Some(to)) = (
            message.from.clone().map(Jid::try_into_full),
            message.to.clone(),
        ) else {
            return;
        };
        let room = to.to_bare();
        let occupants = lock(&self.occupants)
            .get(&room)
            .cloned()
            .unwrap_or_default();
        let Some(nick) = occupants.get(&from).cloned() else {
            return;
        };
        match (message.type_.clone(), to.resource()) {
            (MessageType::Groupchat, None) => {
                for occupant in occupants.keys() {
                    let mut echo = message.clone();
                    echo.from = Some(occupant_jid(&room, &nick));
                    echo.to = Some(Jid::from(occupant.clone()));
                    self.send(echo);
                }
            }
            (_, Some(target)) => {
                let recipient = occupants
                    .iter()
                    .find(|(_, seated)| seated.as_str() == target.as_str())
                    .map(|(occupant, _)| occupant.clone());
                if let Some(recipient) = recipient {
                    let mut private = message.clone();
                    private.from = Some(occupant_jid(&room, &nick));
                    private.to = Some(Jid::from(recipient));
                    private.payloads.push(Element::from(MucUser::new()));
                    self.send(private);
                }
            }
            _ => {}
        }
    }

    fn iq(&self, iq: Iq) {
        let (Some(Ok(from)), Some(to)) =
            (iq.from().cloned().map(Jid::try_into_full), iq.to().cloned())
        else {
            return;
        };
        let room = to.to_bare();
        let seated = lock(&self.occupants)
            .get(&room)
            .and_then(|occupants| occupants.get(&from).cloned());
        match &iq {
            Iq::Get { payload, id, .. } if payload.is("ping", NS_PING) => {
                let own = to
                    .resource()
                    .is_some_and(|nick| seated.as_deref() == Some(nick.as_str()));
                if !own {
                    // XEP-0410 §3.2: not an occupant.
                    self.send(Iq::Error {
                        from: Some(to.clone()),
                        to: Some(Jid::from(from)),
                        id: id.clone(),
                        payload: None,
                        error: stanza_error(DefinedCondition::NotAcceptable, "Not an occupant."),
                    });
                } else if self.reflect_pings.load(Ordering::SeqCst) {
                    let reflected_id = ["reflected-", id].concat();
                    lock(&self.reflected).insert(
                        reflected_id.clone(),
                        Reflected {
                            requester: from.clone(),
                            id: id.clone(),
                            occupant: to.clone(),
                        },
                    );
                    self.send(Iq::Get {
                        from: Some(to.clone()),
                        to: Some(Jid::from(from)),
                        id: reflected_id,
                        payload: Element::builder("ping", NS_PING).build(),
                    });
                } else {
                    self.send(Iq::Result {
                        from: Some(to.clone()),
                        to: Some(Jid::from(from)),
                        id: id.clone(),
                        payload: None,
                    });
                }
            }
            Iq::Set { payload, id, .. } if payload.is("query", MAM_NS) && seated.is_some() => {
                let query_id = payload.attr("queryid").unwrap_or_default().to_owned();
                let mut archived = Message::new_with_type(MessageType::Groupchat, None);
                archived.from = Some(occupant_jid(&room, HOST));
                archived
                    .bodies
                    .insert(Lang(String::new()), "archived".to_owned());
                let forwarded = Element::builder("forwarded", NS_FORWARD)
                    .append(Element::from(archived))
                    .build();
                let result = Element::builder("result", MAM_NS)
                    .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), query_id)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "archive-1")
                    .append(forwarded)
                    .build();
                let mut carrier = Message::new(Some(Jid::from(from.clone())));
                carrier.from = Some(Jid::from(room.clone()));
                carrier.payloads.push(result);
                self.send(carrier);
                self.send(Iq::Result {
                    from: Some(Jid::from(room.clone())),
                    to: Some(Jid::from(from)),
                    id: id.clone(),
                    payload: Some(
                        Element::builder("fin", MAM_NS)
                            .attr(minidom::rxml::xml_ncname!("complete").to_owned(), "true")
                            .build(),
                    ),
                });
            }
            Iq::Result { id, .. } | Iq::Error { id, .. } => {
                let Some(reflected) = lock(&self.reflected).remove(id) else {
                    return;
                };
                // XEP-0410 §3.2: the occupant's answer, whatever it is,
                // answers the self-ping.
                let answer = match iq {
                    Iq::Error { error, .. } => Iq::Error {
                        from: Some(reflected.occupant),
                        to: Some(Jid::from(reflected.requester)),
                        id: reflected.id,
                        payload: None,
                        error,
                    },
                    _ => Iq::Result {
                        from: Some(reflected.occupant),
                        to: Some(Jid::from(reflected.requester)),
                        id: reflected.id,
                        payload: None,
                    },
                };
                self.send(answer);
            }
            _ => {}
        }
    }
}

impl StanzaSink for FakeMuc {
    fn offer(&self, _stream: &VerifiedPeerStream, stanza: Element) -> SinkVerdict {
        let stanza = into_client_content(stanza);
        lock(&self.received).push(stanza.clone());
        self.respond(stanza);
        SinkVerdict::Accepted
    }
}

/// The specification of `home-a.test`, dialling `home-b.test` and
/// `muc.home-b.test` at `address_b`.
fn spec_a(address_b: SocketAddr) -> Spec {
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.allow = vec![HOME_B, MUC_B];
    spec_a.private = vec![HOME_B, MUC_B];
    spec_a.endpoints = vec![(HOME_B, address_b), (MUC_B, address_b)];
    spec_a
}

/// Start `home-b.test` serving `muc.home-b.test` with a [`FakeMuc`] on
/// `listener`, dialling `home-a.test` at `address_a`.
async fn start_muc(
    pki: &Pki,
    listener: TcpListener,
    address_a: SocketAddr,
) -> (Deployment, Arc<FakeMuc>) {
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.local = vec![HOME_B, MUC_B];
    spec_b.endpoints = vec![(HOME_A, address_a)];
    let muc = FakeMuc::new();
    let b = routing::transport::start_with(
        pki,
        &spec_b,
        listener,
        ServiceOptions {
            sink: muc.clone(),
            ..Default::default()
        },
    )
    .await;
    muc.attach(b.handle.clone());
    (b, muc)
}

/// `home-a.test`, routed, and `home-b.test` serving `muc.home-b.test`
/// with a [`FakeMuc`]; each admits and dials the other's domains.
pub async fn outbound_muc(pki: &Pki, limits: RouterLimits) -> (Routed, Deployment, Arc<FakeMuc>) {
    let (listener_a, address_a) = listener().await;
    let (listener_b, address_b) = listener().await;
    let a = start_routed(
        pki,
        &spec_a(address_b),
        listener_a,
        correlation_database("s2s-remote-muc-home-a").await,
        limits,
    )
    .await;
    let (b, muc) = start_muc(pki, listener_b, address_a).await;
    (a, b, muc)
}

/// Two replicas of `home-a.test` sharing one database (its correlation
/// table and remote room occupancies), and `home-b.test` serving
/// `muc.home-b.test` with a [`FakeMuc`]. Both replicas dial
/// `home-b.test`; `home-b.test` dials only the second, so every answer
/// from the room arrives there.
pub async fn outbound_muc_replicas(
    pki: &Pki,
    limits: RouterLimits,
) -> (Routed, Routed, Deployment, Arc<FakeMuc>) {
    let database = correlation_database("s2s-remote-muc-replicas").await;
    let (listener_a1, _) = listener().await;
    let (listener_a2, address_a2) = listener().await;
    let (listener_b, address_b) = listener().await;
    let spec = spec_a(address_b);
    let a1 = start_routed(pki, &spec, listener_a1, database.clone(), limits).await;
    let a2 = start_routed(pki, &spec, listener_a2, database, limits).await;
    let (b, muc) = start_muc(pki, listener_b, address_a2).await;
    (a1, a2, b, muc)
}

/// Offer a client's message or presence to `home-a.test`'s router, as the
/// WebSocket path does for a destination it does not serve.
pub async fn offer(a: &Routed, stanza: Stanza) -> StanzaOffer {
    let to = match &stanza {
        Stanza::Message(message) => message.to.clone(),
        Stanza::Presence(presence) => presence.to.clone(),
        Stanza::Iq(iq) => iq.to().cloned(),
    }
    .expect("a stanza with a recipient");
    offer_stanza(
        &a.router,
        routing::remote(&a.served, to.as_str()),
        stanza,
        a.served.local(),
        REMOTE_ROUTE_OFFER_TIMEOUT,
    )
    .await
}

/// A XEP-0045 §7.2 join presence from `from` to the occupant JID
/// `occupant`.
pub fn join_presence(from: &str, occupant: &str, id: &str) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(jid(from));
    presence.to = Some(jid(occupant));
    presence.id = Some(id.to_owned());
    presence
        .payloads
        .push(Element::builder("x", NS_MUC).build());
    presence
}

/// Presence of `type_` from `from` to `to`, without payload.
pub fn presence(from: &str, to: &str, type_: PresenceType, id: &str) -> Presence {
    let mut presence = Presence::new(type_);
    presence.from = Some(jid(from));
    presence.to = Some(jid(to));
    presence.id = Some(id.to_owned());
    presence
}

/// A message of `type_` with a body from `from` to `to`.
pub fn message(from: &str, to: &str, type_: MessageType, id: &str, body: &str) -> Message {
    let mut message = Message::new_with_type(type_, Some(jid(to)));
    message.from = Some(jid(from));
    message.id = Some(xmpp_parsers::message::Id(id.to_owned()));
    message.bodies.insert(Lang(String::new()), body.to_owned());
    message
}

/// The presence delivered to `to` from `from`, in delivery order.
pub fn delivered_presence(a: &Routed, to: &str, from: &str) -> Vec<Presence> {
    let to = full(to);
    let from = jid(from);
    a.delivery
        .delivered()
        .into_iter()
        .filter_map(|(recipient, stanza)| match stanza {
            Stanza::Presence(presence)
                if recipient == to && presence.from.as_ref() == Some(&from) =>
            {
                Some(presence)
            }
            _ => None,
        })
        .collect()
}

/// The messages delivered to `to` with `id`.
pub fn delivered_messages(a: &Routed, to: &str, id: &str) -> Vec<Message> {
    let to = full(to);
    a.delivery
        .delivered()
        .into_iter()
        .filter_map(|(recipient, stanza)| match stanza {
            Stanza::Message(message)
                if recipient == to && message.id.as_ref().map(|id| id.0.as_str()) == Some(id) =>
            {
                Some(message)
            }
            _ => None,
        })
        .collect()
}

/// The XEP-0045 status codes of a room presence.
pub fn statuses(presence: &Presence) -> Vec<String> {
    presence
        .payloads
        .iter()
        .filter(|payload| payload.is("x", NS_MUC_USER))
        .flat_map(|user| user.children())
        .filter(|child| child.is("status", NS_MUC_USER))
        .filter_map(|status| status.attr("code").map(str::to_owned))
        .collect()
}

/// Join `occupant` (a room JID with a nickname) as `from` and wait for the
/// room's own presence for it.
pub async fn join(a: &Routed, from: &str, occupant: &str) {
    join_through(a, a, from, occupant).await;
}

/// Join `occupant` as `from` through the replica `route` and wait for the
/// room's own presence for it to be delivered by the replica `delivers`,
/// the one the room's server dials.
pub async fn join_through(route: &Routed, delivers: &Routed, from: &str, occupant: &str) {
    let offered = offer(
        route,
        Stanza::Presence(join_presence(from, occupant, "join")),
    )
    .await;
    assert!(
        matches!(offered, StanzaOffer::Accepted),
        "the join is routed: {offered:?}"
    );
    let own = wait_for(ARRIVES, || {
        delivered_presence(delivers, from, occupant)
            .into_iter()
            .find(|presence| statuses(presence).contains(&"110".to_owned()))
    })
    .await;
    assert!(own.is_some(), "the room's own presence for {from} arrives");
}

/// How long a suite waits for a stream to notice its peer went away.
pub const STREAM_LOSS: Duration = Duration::from_secs(15);
