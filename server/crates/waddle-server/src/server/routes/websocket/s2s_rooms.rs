//! The rooms of this deployment as occupants at other servers use them
//! (ADR-0020 D10; XEP-0045 with XEP-0030, XEP-0199, XEP-0410 and XEP-0313).
//!
//! The S2S router hands each stanza a remote principal addressed to the MUC
//! service to [`WebSocketLocalRooms`], which decides it for that principal
//! on the evidence of the stream that carried it and answers with typed
//! stanzas. Nothing here is a session: a remote principal holds no account,
//! no connection and no roster on this deployment, and every decision for
//! it names its stream's evidence ([`PrincipalEvidence::of`]), so that
//! peer admission (D1) and the trusted-home rule (D8) are evaluated where
//! the effect happens:
//!
//! - a room an enrolled host-grant scope manages decides from its canonical
//!   grants, as honoured for that evidence, behind the revocation fence;
//! - any other room keeps its own checks, on the standing those checks give
//!   the principal as the effect boundary honours it
//!   (`honoured_legacy_affiliation`): nothing for a domain that is not
//!   admitted, and no owner or admin standing unless a trusted home's
//!   `pkix` stream carried the request.
//!
//! What a remote principal may ask is narrower than what a session may:
//! joining, changing nickname and availability, leaving, sending to the
//! room, reading its archive, `disco#info`, `disco#items` and ping, and
//! administering the room (`muc#admin`, `muc#owner`, XEP-0425 moderation)
//! with a standing the effect boundary honours for its stream. Private
//! messages, invitations, voice requests and every extension of this
//! deployment (calls, pins, hats, commands, forum actions, mentions) are
//! refused or dropped. Presence and messages from a remote occupant
//! are rebuilt from an allowlist before the room sees them
//! ([`reduced_presence`], [`reduced_message`]): what is not named there
//! does not reach the room's pipeline, whose handlers act on payloads.
//!
//! A room is reached at all only while its `federated` setting is on; any
//! other room answers as one that does not exist ([`no_such_room`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use waddle_xmpp::ingress::{DigestContext, NormalizedTarget, TransportGeneration};
use waddle_xmpp::mam::{
    build_fin_iq, build_query_form_iq, build_result_messages, is_mam_query_form_request,
    parse_mam_query, MamArchiveKind, MamStorageError,
};
use waddle_xmpp::muc::presence::{NS_MUC, NS_MUC_USER};
use waddle_xmpp::muc::room_actor::{
    GetConfig, GetOccupantSessionGeneration, GetSeatedSessionGenerations, LeaveSessionSelector,
    PingSelfCheck,
};
use waddle_xmpp::muc::MucRemovalCause;
use waddle_xmpp::protocol::room::archive::is_archivable;
use waddle_xmpp::xep::xep0085::NS_CHATSTATES;
use waddle_xmpp::xep::xep0115::NS_CAPS;
use waddle_xmpp::xep::xep0308::NS_MESSAGE_CORRECT;
use waddle_xmpp::xep::xep0333::NS_CHAT_MARKERS;
use waddle_xmpp::xep::xep0424::NS_MESSAGE_RETRACT;
use waddle_xmpp::xep::xep0428::NS_FALLBACK;
use waddle_xmpp::xep::xep0444::NS_REACTIONS;
use waddle_xmpp::xep::xep0461::NS_REPLY;
use waddle_xmpp::Stanza;
use xmpp_parsers::disco::{DiscoInfoResult, DiscoItemsResult};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::cleanup::{get_room_actor_result, is_muc_room_jid, sweep_abandoned_room_occupancy};
use super::frame::{ResponseBatch, ResponseFrame};
use super::handlers::iq::{
    muc_admin_for_remote, muc_disco_info_for_remote, muc_owner_or_moderation_for_remote,
};
use super::handlers::presence::{
    get_managed_channel_for_room, handle_muc_join_with_ordered_relay, handle_muc_leave,
    host_grant_read_still_admitted_as, principal_witness, resolve_muc_room_archive_access_as,
    try_handle_muc_presence_update, MucJoinRequest, RoomArchiveAccess,
};
use super::interpret_loop::build_interpret_deps;
use super::WebSocketState;
use crate::auth::Session;
use crate::ingress::{
    ImmediateSink, IngressPrincipal, IngressStreamIdentity, IngressSubmission,
    RemoteOccupantPrincipal,
};
use crate::notification_activity::NotificationPresenceShow;
use crate::permissions::host_grant::{
    HostGrantAction, PrincipalEvidence, RoomBinding, RoomGate, StreamEvidence,
};
use crate::s2s::route::{
    LocalRooms, RemotePrincipal, RoomOperation, RoomPlacement, RoomReplies, RoomRequest, Seat,
    SeatChange, SeatEnd,
};
use crate::server::routes::interpret::effects::RoomExecutionPath;
use crate::server::routes::interpret::plan_remote_occupant_groupchat;

/// XEP-0045 status code 110: the presence is the occupant's own.
const STATUS_SELF: &str = "110";

/// Longest one ask of a room actor waits.
const ROOM_ASK_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest the effects of one accepted room message may take.
const MESSAGE_EFFECTS_TIMEOUT: Duration = Duration::from_secs(5);

/// Messages one remote account may have archived, on average each second.
const ARCHIVE_WRITE_RATE: f64 = 5.0;

/// Messages one remote account may have archived at once.
const ARCHIVE_WRITE_BURST: f64 = 30.0;

/// Remote accounts whose archive allowance is kept. An account that finds
/// the table full of accounts still spending theirs is refused, so the
/// table never grows with the accounts a domain mints.
const ARCHIVE_WRITERS: usize = 65_536;

/// What one remote account may still have archived.
struct ArchiveAllowance {
    tokens: f64,
    refilled: Instant,
}

/// The archive allowances of remote accounts on this replica, which is the
/// one that writes what they send to its rooms' archives.
#[derive(Default)]
struct ArchiveAllowances(Mutex<HashMap<BareJid, ArchiveAllowance>>);

impl ArchiveAllowances {
    /// Take one archived message of `account`'s allowance: `false` when it
    /// is spent, or when the table holds no place for the account.
    fn take(&self, account: &BareJid) -> bool {
        let now = Instant::now();
        let mut allowances = self
            .0
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if allowances.len() >= ARCHIVE_WRITERS && !allowances.contains_key(account) {
            // An account whose allowance has refilled needs no entry.
            allowances.retain(|_, allowance| {
                now.duration_since(allowance.refilled).as_secs_f64() * ARCHIVE_WRITE_RATE
                    < ARCHIVE_WRITE_BURST - allowance.tokens
            });
            if allowances.len() >= ARCHIVE_WRITERS {
                return false;
            }
        }
        let allowance = allowances
            .entry(account.clone())
            .or_insert(ArchiveAllowance {
                tokens: ARCHIVE_WRITE_BURST,
                refilled: now,
            });
        let refill = now.duration_since(allowance.refilled).as_secs_f64() * ARCHIVE_WRITE_RATE;
        allowance.tokens = (allowance.tokens + refill).min(ARCHIVE_WRITE_BURST);
        allowance.refilled = now;
        if allowance.tokens >= 1.0 {
            allowance.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// A remote occupant's message was refused because its account had spent
/// its archive allowance.
fn archive_write_limited() {
    waddle_xmpp::counter_add!(
        "waddle.s2s.rooms.archive_writes.limited",
        "{stanza}",
        "Messages of remote occupants refused with resource-constraint because their account had spent its allowance of archived room messages.",
        1,
    );
}

/// Finds the replica that holds a room and carries a remote principal's
/// request there when it is another one.
///
/// A deployment of several replicas has one (its cluster's room claims and
/// relay); a single replica has none and decides every request itself.
#[async_trait]
pub trait RoomCarrier: Send + Sync {
    /// Where `stanza`, which `principal` addressed to `room`, is decided:
    /// here, or on the replica that holds `room`, which then took it with
    /// the facts of `principal`'s stream.
    async fn place(
        &self,
        room: &BareJid,
        principal: &RemotePrincipal,
        stanza: &Stanza,
    ) -> RoomPlacement;
}

/// [`LocalRooms`] over the WebSocket state's rooms, grants and archive.
///
/// It holds the state weakly: the state holds the router, which holds this.
pub struct WebSocketLocalRooms {
    state: Weak<WebSocketState>,
    archive_writes: ArchiveAllowances,
    carrier: Option<Arc<dyn RoomCarrier>>,
}

impl WebSocketLocalRooms {
    /// Host the rooms of `state` for remote occupants, carrying a request
    /// for a room another replica holds through `carrier`.
    pub fn new(state: &Arc<WebSocketState>, carrier: Option<Arc<dyn RoomCarrier>>) -> Self {
        Self {
            state: Arc::downgrade(state),
            archive_writes: ArchiveAllowances::default(),
            carrier,
        }
    }
}

/// The carrier of a replica of `state`'s deployment: its cluster's, when
/// the build clusters, and otherwise none.
fn cluster_carrier(state: &Arc<WebSocketState>) -> Option<Arc<dyn RoomCarrier>> {
    #[cfg(feature = "clustering")]
    {
        Some(Arc::new(super::s2s_room_relay::ClusterRoomCarrier::new(
            state,
        )))
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = state;
        None
    }
}

/// Have `router` host the rooms of `state` for remote occupants, with their
/// seats in `global`, the database every replica shares:
///
/// - the router hands what remote principals ask of the MUC service to
///   [`WebSocketLocalRooms`];
/// - a room's stanza for an occupant that holds no connection here is
///   offered to the router, which takes it only for a seat it holds;
/// - decisions that name a seated remote occupant without a request of its
///   own read the evidence its seat was admitted under.
///
/// A request for a room another replica holds is carried there by the
/// cluster's relay.
pub(crate) fn host_rooms(
    state: &Arc<WebSocketState>,
    router: &crate::s2s::route::S2sRouter,
    global: crate::db::Database,
) {
    host_rooms_carrying(state, router, global, cluster_carrier(state));
}

/// [`host_rooms`] with `carrier` in place of the cluster's.
pub(crate) fn host_rooms_carrying(
    state: &Arc<WebSocketState>,
    router: &crate::s2s::route::S2sRouter,
    global: crate::db::Database,
    carrier: Option<Arc<dyn RoomCarrier>>,
) {
    router.attach_rooms(
        Arc::new(WebSocketLocalRooms::new(state, carrier)),
        Arc::new(crate::s2s::route::SqlSeatStore::new(global)),
    );
    state
        .deps
        .protocol
        .connection_registry
        .set_foreign_delivery(Arc::new(SeatDelivery(router.clone())));
    state
        .deps
        .protocol
        .ingress
        .host_grant_gate()
        .attach_seat_evidence(router.seats());
}

/// Delivery of a room's stanzas to the seats of remote occupants.
struct SeatDelivery(crate::s2s::route::S2sRouter);

impl waddle_xmpp::registry::ForeignDelivery for SeatDelivery {
    fn takes(&self, recipient: &FullJid, stanza: &Stanza) -> bool {
        self.0.seats_recipient(recipient, stanza)
    }

    fn offer(&self, recipient: &FullJid, stanza: Stanza) -> Result<(), Box<Stanza>> {
        self.0.offer_to_seat(recipient, stanza)
    }

    fn close_room(&self, room: &BareJid) {
        self.0.close_room_seats(room);
    }
}

/// One request, as its handlers read it.
struct Asked<'a> {
    state: &'a Arc<WebSocketState>,
    archive_writes: &'a ArchiveAllowances,
    request: &'a RoomRequest,
    /// The address the stanza was sent to.
    to: &'a Jid,
    /// The sender's bare JID: the subject of every decision.
    account: BareJid,
    /// What the stream that carried the stanza proved.
    evidence: PrincipalEvidence,
}

fn error(type_: ErrorType, condition: DefinedCondition, text: &str) -> StanzaError {
    StanzaError::new(type_, condition, "en", text)
}

fn forbidden(text: &str) -> StanzaError {
    error(ErrorType::Auth, DefinedCondition::Forbidden, text)
}

fn unavailable() -> StanzaError {
    error(
        ErrorType::Cancel,
        DefinedCondition::ServiceUnavailable,
        "The request is not available to occupants from other servers.",
    )
}

/// The answer for a room a remote asker may not learn about: one that does
/// not exist, a group conversation of this deployment's accounts, and one
/// whose `federated` setting is off. They are answered alike, in
/// condition, type and text, so the answer says nothing about which.
fn no_such_room() -> StanzaError {
    error(
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        "Requested item not found.",
    )
}

fn internal() -> StanzaError {
    error(
        ErrorType::Wait,
        DefinedCondition::InternalServerError,
        "The room could not answer now; try again later.",
    )
}

/// The error answering `stanza`, from the address it was sent to; `None`
/// for a stanza no error may answer (RFC 6120 §8.3.1) and for presence
/// other than an attempt to enter.
fn refuse(stanza: &Stanza, error: StanzaError) -> Vec<Stanza> {
    let reply = match stanza {
        Stanza::Iq(iq) => match iq.as_ref() {
            Iq::Get { .. } | Iq::Set { .. } => Some(Stanza::Iq(Box::new(Iq::Error {
                from: iq.to().cloned(),
                to: iq.from().cloned(),
                id: iq.id().to_owned(),
                payload: None,
                error,
            }))),
            Iq::Result { .. } | Iq::Error { .. } => None,
        },
        Stanza::Message(message) if message.type_ != MessageType::Error => {
            let mut reply = Message::error(message.from.clone());
            reply.from = message.to.clone();
            reply.id = message.id.clone();
            reply.payloads.push(Element::from(error));
            Some(Stanza::Message(reply))
        }
        Stanza::Presence(presence) if presence.type_ == PresenceType::None => {
            let mut reply = Presence::new(PresenceType::Error);
            reply.from = presence.to.clone();
            reply.to = presence.from.clone();
            reply.id = presence.id.clone();
            reply.payloads.push(Element::builder("x", NS_MUC).build());
            reply.payloads.push(Element::from(error));
            Some(Stanza::Presence(reply))
        }
        Stanza::Message(_) | Stanza::Presence(_) => None,
    };
    reply.into_iter().collect()
}

/// The result answering the request `iq`, from the address it was sent to.
fn result(iq: &Iq, payload: Option<Element>) -> Stanza {
    Stanza::Iq(Box::new(Iq::Result {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_owned(),
        payload,
    }))
}

/// The frames a room handler written for WebSocket sessions returns are
/// that transport's wire form; they are read back into typed stanzas once,
/// here, before anything else sees them. `None` when one of them is not a
/// stanza: an answer read only in part must not be taken for the whole.
fn stanzas_of(frames: Vec<String>) -> Option<Vec<Stanza>> {
    frames
        .into_iter()
        .map(|frame| {
            let element = frame.parse::<Element>().ok()?;
            match element.name() {
                "message" => Message::try_from(element).ok().map(Stanza::Message),
                "presence" => Presence::try_from(element).ok().map(Stanza::Presence),
                "iq" => Iq::try_from(element)
                    .ok()
                    .map(|iq| Stanza::Iq(Box::new(iq))),
                _ => None,
            }
        })
        .collect()
}

/// The stanza a response frame of a session handler carries; `None` for
/// a frame that is no stanza, which no handler reached here produces.
fn stanza_of(frame: ResponseFrame) -> Option<Stanza> {
    match frame {
        ResponseFrame::Stanza(stanza) => Some(*stanza),
        ResponseFrame::Provenanced { frame, .. } | ResponseFrame::Replay { frame, .. } => {
            stanza_of(*frame)
        }
        ResponseFrame::Element(element) => match element.name() {
            "message" => Message::try_from(element).ok().map(Stanza::Message),
            "presence" => Presence::try_from(element).ok().map(Stanza::Presence),
            "iq" => Iq::try_from(element)
                .ok()
                .map(|iq| Stanza::Iq(Box::new(iq))),
            _ => None,
        },
        ResponseFrame::SmEnabled(_) | ResponseFrame::StreamError(_) => None,
    }
}

/// A remote occupant's presence as the room sees it: its availability
/// (`show`, `status`, `priority`), its entity capabilities (XEP-0115) and,
/// for an attempt to enter, the bare `muc` element. Every other payload is
/// dropped, as XEP-0045 §17.1.1 lets a service do: the presence extensions
/// of this deployment (calls, hats) carry meaning only for its own
/// sessions, and a history request is not honoured.
///
/// A `muc#user` element is not carried inbound. XEP-0045 has the room, not
/// the occupant, author it (§7.2.2), the room strips one from a session's
/// presence before it adds its own, and the router reads the status codes
/// of the room's presence to follow a seat.
fn reduced_presence(presence: &Presence) -> Presence {
    let mut reduced = Presence::new(presence.type_.clone());
    reduced.from = presence.from.clone();
    reduced.to = presence.to.clone();
    reduced.id = presence.id.clone();
    reduced.show = presence.show.clone();
    reduced.statuses = presence.statuses.clone();
    reduced.priority = presence.priority.clone();
    if presence
        .payloads
        .iter()
        .any(|payload| payload.is("x", NS_MUC))
    {
        reduced.payloads.push(Element::builder("x", NS_MUC).build());
    }
    if let Some(caps) = presence
        .payloads
        .iter()
        .find(|payload| payload.is("c", NS_CAPS))
    {
        // The element as XEP-0115 §4 defines it: its attributes, no child.
        let mut carried = Element::builder("c", NS_CAPS);
        for (name, value) in [
            (minidom::rxml::xml_ncname!("hash"), caps.attr("hash")),
            (minidom::rxml::xml_ncname!("node"), caps.attr("node")),
            (minidom::rxml::xml_ncname!("ver"), caps.attr("ver")),
            (minidom::rxml::xml_ncname!("ext"), caps.attr("ext")),
        ] {
            if let Some(value) = value {
                carried = carried.attr(name.to_owned(), value);
            }
        }
        reduced.payloads.push(carried.build());
    }
    reduced
}

/// Give the error presences among `replies` the id of the presence they
/// answer (RFC 6120 §8.1.3). The room's handlers build a refusal without
/// one: a session matches it by its addresses, while a server relaying it
/// to a client has only the id to go by.
fn with_request_id(mut replies: Vec<Stanza>, id: Option<&str>) -> Vec<Stanza> {
    let Some(id) = id else {
        return replies;
    };
    for reply in &mut replies {
        if let Stanza::Presence(presence) = reply {
            if presence.type_ == PresenceType::Error && presence.id.is_none() {
                presence.id = Some(id.to_owned());
            }
        }
    }
    replies
}

/// Whether `payload` of a remote occupant's message reaches the room: the
/// payloads of the standard extensions a room carries between occupants.
///
/// - XEP-0359 `origin-id`, the identifier the sender's client gave the
///   message. A `stanza-id` is the room's to assign and is not carried.
/// - XEP-0308 `replace` and XEP-0424 `retract`: the room lets an occupant
///   correct or retract only its own archived message.
/// - XEP-0444 `reactions`, XEP-0461 `reply` and the XEP-0428 `fallback`
///   that marks the part of a body standing in for either.
/// - XEP-0333 markers and XEP-0085 chat states.
///
/// Everything else is dropped before the room plans the message: this
/// deployment's own payloads (pins, forum actions, framework envelopes),
/// mentions and references, which notify accounts, processing hints, and
/// any payload the room's pipeline has not been reviewed to act on for a
/// remote principal.
fn carried_in_message(payload: &Element) -> bool {
    payload.is("origin-id", waddle_xmpp_core::xep0359::NS_SID)
        || payload.is("replace", NS_MESSAGE_CORRECT)
        || payload.is("retract", NS_MESSAGE_RETRACT)
        || payload.is("reactions", NS_REACTIONS)
        || payload.is("reply", NS_REPLY)
        || payload.is("fallback", NS_FALLBACK)
        || (payload.ns() == NS_CHAT_MARKERS
            && matches!(
                payload.name(),
                "markable" | "received" | "displayed" | "acknowledged"
            ))
        || (payload.ns() == NS_CHATSTATES
            && matches!(
                payload.name(),
                "active" | "composing" | "paused" | "inactive" | "gone"
            ))
}

/// A remote occupant's message as the room sees it: its text, its subject,
/// its thread and the payloads [`carried_in_message`] names, each kind once
/// except `fallback`, which a message may carry for several extensions.
/// `None` when nothing of the message is carried.
fn reduced_message(message: &Message) -> Option<Message> {
    let mut reduced = Message::new_with_type(MessageType::Groupchat, message.to.clone());
    reduced.from = message.from.clone();
    reduced.id = message.id.clone();
    reduced.bodies = message.bodies.clone();
    reduced.subjects = message.subjects.clone();
    reduced.thread = message.thread.clone();
    for payload in message.payloads.iter().filter(|p| carried_in_message(p)) {
        let repeated = !payload.is("fallback", NS_FALLBACK)
            && reduced
                .payloads
                .iter()
                .any(|kept| kept.name() == payload.name() && kept.ns() == payload.ns());
        if !repeated {
            reduced.payloads.push(payload.clone());
        }
    }
    let carried =
        !reduced.bodies.is_empty() || !reduced.subjects.is_empty() || !reduced.payloads.is_empty();
    carried.then_some(reduced)
}

/// Whether `replies` hold the room's own available presence for the
/// occupant (status 110): the room seated it.
fn seated(replies: &[Stanza]) -> bool {
    replies.iter().any(|reply| {
        let Stanza::Presence(presence) = reply else {
            return false;
        };
        presence.type_ == PresenceType::None
            && presence
                .payloads
                .iter()
                .filter(|payload| payload.is("x", NS_MUC_USER))
                .flat_map(Element::children)
                .any(|child| {
                    child.is("status", NS_MUC_USER) && child.attr("code") == Some(STATUS_SELF)
                })
    })
}

/// The features of a room or of the MUC service remote askers are told:
/// service discovery, the MUC protocol and the room's XEP-0045 type flags.
/// Anything else stays unadvertised until its use across servers has been
/// demonstrated (ADR-0020 gate G21).
fn told_to_remote(feature: &str) -> bool {
    feature == DISCO_INFO_NS
        || feature == DISCO_ITEMS_NS
        || feature == NS_MUC
        || feature.starts_with("muc_")
}

impl Asked<'_> {
    fn refuse(&self, error: StanzaError) -> RoomReplies {
        RoomReplies::of(refuse(&self.request.stanza, error))
    }

    /// The room the stanza addresses: an address at the MUC service with a
    /// localpart.
    fn room(&self) -> Option<BareJid> {
        self.to.node().map(|_| self.to.to_bare())
    }

    /// Whether accounts at other servers may reach `room` at all: it
    /// exists, it is not a group conversation of this deployment's
    /// accounts, and its `federated` setting is on. A room an enrolled
    /// scope manages needs the setting like any other; its grants decide
    /// who enters once it is on. `None` when that cannot be told now.
    ///
    /// The setting is read from the room's actor when this replica holds
    /// one, and otherwise from the channel catalog, which a room's
    /// configuration keeps in step. A room with neither is not found: no
    /// room is created for a remote principal.
    ///
    /// A room whose actor cannot be reached now (it is being restored, or
    /// its claim is moving) is "cannot be told" only when it may be open:
    /// its catalog row says so, or it has no row while this deployment
    /// opens its new rooms to other servers. Any other address is not
    /// found, as before the actor was unreachable, so the retryable answer
    /// never tells a closed room from one that does not exist.
    async fn open_to_other_servers(&self, room: &BareJid) -> Option<bool> {
        if !is_muc_room_jid(self.state, room).await {
            return Some(false);
        }
        let read_catalog = || async {
            let channel = get_managed_channel_for_room(self.state, room).await.ok()?;
            // `None` for an address without a row.
            Some(channel.map(|channel| {
                channel.federated
                    && channel.channel_type != waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM
            }))
        };
        match get_room_actor_result(self.state, room).await {
            Ok(Some(actor)) => {
                let config = actor
                    .ask(GetConfig)
                    .reply_timeout(ROOM_ASK_TIMEOUT)
                    .await
                    .ok()?;
                Some(config.federated && !config.group_dm)
            }
            Ok(None) => Some(read_catalog().await?.unwrap_or(false)),
            Err(_) => {
                let may_be_open = read_catalog()
                    .await?
                    .unwrap_or_else(|| self.state.deps.app_state.federates_new_rooms());
                if may_be_open {
                    None
                } else {
                    Some(false)
                }
            }
        }
    }

    /// The refusal of a request for a room accounts at other servers may
    /// not reach, or `None` for a room they may. It is decided before
    /// anything else about the room, the sender's grants and standing
    /// included, so no later answer tells such a room from one that does
    /// not exist. An occupant the room still seats is removed: its seat
    /// was opened while the room was open. While the setting cannot be
    /// read the request is answered with a retryable error and a seat is
    /// left as it is.
    async fn closed_room(&self, room: &BareJid) -> Option<RoomReplies> {
        match self.open_to_other_servers(room).await {
            Some(true) => return None,
            Some(false) => {}
            None => {
                return Some(RoomReplies {
                    stanzas: refuse(&self.request.stanza, internal()),
                    // A seat opened for this request has no room to hold
                    // it; one the occupant already held stays.
                    seat: if self.request.joining {
                        SeatChange::Refused
                    } else {
                        SeatChange::Unchanged
                    },
                });
            }
        }
        let refused = no_such_room();
        if let (Some(seat), false) = (&self.request.seat, self.request.joining) {
            sweep_abandoned_room_occupancy(
                self.state,
                &seat.room,
                &seat.occupant,
                LeaveSessionSelector::Generation(seat.session),
                MucRemovalCause::TechnicalProblem,
            )
            .await;
        }
        Some(RoomReplies {
            stanzas: refuse(&self.request.stanza, refused),
            seat: SeatChange::Refused,
        })
    }

    // --- service discovery and ping ------------------------------------------

    async fn disco_info(&self, iq: &Iq) -> RoomReplies {
        let Iq::Get { payload, .. } = iq else {
            return self.refuse(unavailable());
        };
        // No node of the service or of a room is described to remote
        // askers.
        if payload.attr("node").is_some() {
            return self.refuse(no_such_room());
        }
        let requester = self.request.principal.full();
        let answered =
            muc_disco_info_for_remote(self.state, iq, requester.as_ref(), self.evidence).await;
        let described = match answered {
            Some(Ok(Iq::Result {
                payload: Some(payload),
                ..
            })) => DiscoInfoResult::try_from(payload).ok(),
            Some(Err(error)) => return self.refuse(error),
            Some(Ok(_)) | None => None,
        };
        // An answer this server could not build is no statement about the
        // address: a peer caches what a service or a room says it is.
        let Some(mut described) = described else {
            return self.refuse(internal());
        };
        described
            .features
            .retain(|feature| told_to_remote(&feature.var));
        described.extensions.clear();
        RoomReplies::of(vec![result(iq, Some(Element::from(described)))])
    }

    /// Neither the service's rooms nor a room's occupants are listed to
    /// remote askers: an empty list is the answer XEP-0045 §6.3 and §6.5
    /// allow.
    fn disco_items(&self, iq: &Iq) -> RoomReplies {
        let Iq::Get { payload, .. } = iq else {
            return self.refuse(unavailable());
        };
        if payload.attr("node").is_some() || self.to.resource().is_some() {
            return self.refuse(no_such_room());
        }
        let listed = DiscoItemsResult {
            node: None,
            items: Vec::new(),
            rsm: None,
        };
        RoomReplies::of(vec![result(iq, Some(Element::from(listed)))])
    }

    /// XEP-0199 ping of the service or a room, and the XEP-0410 self-ping
    /// of the sender's own occupant JID, answered from the room's grants
    /// and then from its occupancy: `not-acceptable` is "not joined".
    async fn ping(&self, iq: &Iq) -> RoomReplies {
        let Some(nick) = self.to.resource() else {
            return RoomReplies::of(vec![result(iq, None)]);
        };
        let not_joined = || {
            self.refuse(error(
                ErrorType::Cancel,
                DefinedCondition::NotAcceptable,
                "You are not joined to this room.",
            ))
        };
        let (Some(seat), Some(occupant)) = (&self.request.seat, self.request.principal.full())
        else {
            return not_joined();
        };
        // An IQ for another occupant would be relayed to an account, which
        // remote senders do not reach (ADR-0020 D7).
        if seat.nick != nick.as_str() {
            return self.refuse(unavailable());
        }
        // What cannot be told now is answered with the one error XEP-0410
        // §4 has a client take no decision from, so that it neither leaves
        // nor enters again on it: any other error means "not joined".
        let undecided = |text: &str| {
            self.refuse(error(
                ErrorType::Wait,
                DefinedCondition::RemoteServerTimeout,
                text,
            ))
        };
        match self
            .state
            .deps
            .app_state
            .host_grants
            .allows_as(
                RoomBinding::Room(&seat.room),
                &self.account,
                &self.evidence,
                HostGrantAction::Read,
            )
            .await
        {
            RoomGate::Unmanaged | RoomGate::Managed(true) => {}
            RoomGate::Managed(false) => return not_joined(),
            RoomGate::Unavailable => {
                return undecided("The room's grants could not be read; try again.");
            }
        }
        // A room whose actor cannot be reached now (it is being restored,
        // or its claim is moving) may still seat the occupant; only a room
        // without an actor here seats nobody.
        let actor = match get_room_actor_result(self.state, &seat.room).await {
            Ok(Some(actor)) => actor,
            Ok(None) => return not_joined(),
            Err(_) => return undecided("The room cannot be reached now; try again."),
        };
        let asked = actor
            .ask(PingSelfCheck {
                nick: seat.nick.clone(),
                sender_jid: occupant,
            })
            .reply_timeout(ROOM_ASK_TIMEOUT)
            .await;
        match asked {
            Ok(()) => RoomReplies::of(vec![result(iq, None)]),
            Err(kameo::error::SendError::HandlerError(_)) => not_joined(),
            Err(_) => undecided("The room did not answer; try again."),
        }
    }

    // --- archive --------------------------------------------------------------

    /// A XEP-0313 query of a room's archive, admitted as entry to the room
    /// is (XEP-0313 §5.1.1) for the asker and its stream's evidence, before
    /// the archive is read and again after it.
    async fn archive(&self, iq: &Iq) -> RoomReplies {
        let Some(room) = self.room().filter(|_| self.to.resource().is_none()) else {
            return self.refuse(unavailable());
        };
        let not_found = || self.refuse(no_such_room());
        let Ok(channel) = get_managed_channel_for_room(self.state, &room).await else {
            return self.refuse(internal());
        };
        match resolve_muc_room_archive_access_as(
            self.state,
            &room,
            Some(&self.account),
            &self.evidence,
            channel.as_ref(),
        )
        .await
        {
            RoomArchiveAccess::Allowed => {}
            RoomArchiveAccess::Denied => return self.refuse(forbidden("Operation not permitted.")),
            RoomArchiveAccess::Error => return self.refuse(internal()),
        }
        if is_mam_query_form_request(iq) {
            return RoomReplies::of(vec![Stanza::Iq(Box::new(build_query_form_iq(iq)))]);
        }
        let Ok((query_id, query)) = parse_mam_query(iq) else {
            return self.refuse(error(
                ErrorType::Modify,
                DefinedCondition::BadRequest,
                "Malformed archive query.",
            ));
        };
        let storage = &self.state.deps.protocol.mam_storage;
        let mut page = match storage
            .query_messages(&room, MamArchiveKind::Room, &query)
            .await
        {
            Ok(page) => page,
            Err(MamStorageError::NotFound(_)) => return not_found(),
            Err(_) => return self.refuse(internal()),
        };
        if page.count.is_none() {
            page.count = storage.count_messages(&room).await.ok();
        }
        // A page of a managed room is sent only on a decision taken after
        // it was read, so a revoke that committed before the read keeps it
        // from being sent.
        if host_grant_read_still_admitted_as(self.state, &room, Some(&self.account), &self.evidence)
            .await
            == Some(false)
        {
            return self.refuse(forbidden("Operation not permitted."));
        }
        let asker = self.request.principal.address().clone();
        let mut replies: Vec<Stanza> =
            build_result_messages(&query_id, &Jid::from(room), &asker, &page.messages)
                .into_iter()
                .map(Stanza::Message)
                .collect();
        replies.push(Stanza::Iq(Box::new(build_fin_iq(iq, &page))));
        RoomReplies::of(replies)
    }

    // --- presence -------------------------------------------------------------

    async fn presence(&self, presence: &Presence) -> RoomReplies {
        let mut replies = self.decide_presence(presence).await;
        replies.stanzas = with_request_id(replies.stanzas, presence.id.as_deref());
        replies
    }

    async fn decide_presence(&self, presence: &Presence) -> RoomReplies {
        let (Some(room), Some(seat), Some(occupant)) = (
            self.room(),
            self.request.seat.as_ref(),
            self.request.principal.full(),
        ) else {
            // Presence to the service or a room's bare JID, or from an
            // entity that holds no seat and asks for none, is ignored
            // (XEP-0045 §17.3).
            return RoomReplies::none();
        };
        let reduced = reduced_presence(presence);
        match presence.type_ {
            PresenceType::None => self.enter_or_update(&room, seat, &occupant, &reduced).await,
            PresenceType::Unavailable if !self.request.joining => {
                let frames =
                    handle_muc_leave(self.state, &room, &occupant, &seat.nick, seat.session, None)
                        .await;
                // The room no longer seats the occupant, whatever of its
                // answer could be read: the seat is given up.
                match stanzas_of(frames) {
                    Some(stanzas) => RoomReplies::of(stanzas),
                    None => RoomReplies {
                        stanzas: Vec::new(),
                        seat: SeatChange::Refused,
                    },
                }
            }
            _ => RoomReplies::none(),
        }
    }

    /// Available presence to an occupant JID: a change of availability or
    /// nickname by an occupant the room seats, and otherwise a join
    /// (XEP-0045 §7.2), seated under the seat's occupancy generation.
    async fn enter_or_update(
        &self,
        room: &BareJid,
        seat: &Seat,
        occupant: &FullJid,
        presence: &Presence,
    ) -> RoomReplies {
        let Some(nick) = self.to.resource().map(|nick| nick.as_str().to_owned()) else {
            return RoomReplies::none();
        };
        if !self.request.joining {
            let updated = try_handle_muc_presence_update(
                self.state,
                room,
                occupant,
                &nick,
                seat.session,
                presence,
            )
            .await;
            if let Some(frames) = updated {
                return RoomReplies::of(stanzas_of(frames).unwrap_or_default());
            }
            // The presence enters the room: the room does not seat the
            // occupant any more, or the occupant asks to enter again. It
            // is decided as a join, on a seat opened for it, so that the
            // room's answer precedes its traffic for the occupant
            // (XEP-0045 §7.1); the router asks again with that seat.
            return RoomReplies {
                stanzas: Vec::new(),
                seat: SeatChange::Rejoin,
            };
        }
        // The join handler reads the joiner's address from a session. This
        // one names the remote account and nothing else: it is no
        // credential, is stored nowhere, and every decision the join takes
        // for the joiner names the stream's evidence.
        let localpart = self
            .account
            .node()
            .map_or_else(|| self.account.to_string(), |node| node.to_string());
        let address = Session::new(self.account.as_str(), &localpart, &localpart);
        let frames = handle_muc_join_with_ordered_relay(
            self.state,
            MucJoinRequest {
                domain: self.state.deps.auth_state.xmpp_domain.as_str(),
                room_jid: room,
                sender_jid: occupant,
                nick: &nick,
                presence_show: presence
                    .show
                    .clone()
                    .map(NotificationPresenceShow::from_xep0045),
                occupancy_session: seat.session,
                authenticated_session: Some(&address),
                registry_owner: None,
                // A join for a room another replica holds was carried
                // there with its stream's evidence before it reached this
                // host; the session relay, which names its requester
                // without one, is never taken for a remote principal.
                ordered_relay_origin: None,
                evidence: self.evidence,
            },
        )
        .await;
        let Some(stanzas) = stanzas_of(frames) else {
            // The room may have seated the occupant, and its answer cannot
            // be given: the occupant is removed again rather than left
            // seated without having been told.
            tracing::warn!("a room's answer to a remote occupant's join could not be read");
            sweep_abandoned_room_occupancy(
                self.state,
                room,
                occupant,
                LeaveSessionSelector::Generation(seat.session),
                MucRemovalCause::TechnicalProblem,
            )
            .await;
            return RoomReplies {
                stanzas: refuse(&self.request.stanza, internal()),
                seat: SeatChange::Refused,
            };
        };
        let seat = if seated(&stanzas) {
            SeatChange::Seated
        } else {
            SeatChange::Refused
        };
        RoomReplies { stanzas, seat }
    }

    /// Whether a subject change of the sender may be put to `room`: the
    /// room lets every occupant change its subject, or the sender's
    /// privileged standing is honoured on the stream that carried the
    /// request (ADR-0020 D8). `None` when that cannot be told now: the
    /// room's actor cannot be reached or its configuration read. A room
    /// that cannot say is asked nothing.
    async fn may_ask_subject_change(&self, room: &BareJid) -> Option<bool> {
        if principal_witness(self.state, &self.account, &self.evidence)
            .is_some_and(|witness| witness.privilege_honoured())
        {
            return Some(true);
        }
        let actor = match get_room_actor_result(self.state, room).await {
            Ok(Some(actor)) => actor,
            Ok(None) => return Some(false),
            Err(_) => return None,
        };
        actor
            .ask(GetConfig)
            .reply_timeout(ROOM_ASK_TIMEOUT)
            .await
            .ok()
            .map(|config| config.occupants_may_change_subject)
    }

    // --- administration -------------------------------------------------------

    /// XEP-0045 `muc#admin`: the affiliation and role lists of a room and
    /// changes to them. The requester is decided on its stream's evidence
    /// by the handler a session's request reaches: in a room an enrolled
    /// scope manages from its canonical grants, with an affiliation change
    /// recorded through the store on that evidence and behind the
    /// revocation fence; in any other room from the standing the room
    /// holds for it, honoured as D8 says.
    async fn admin(&self, iq: &Iq) -> RoomReplies {
        let Some(requester) = self.request.principal.full() else {
            return self.refuse(forbidden("Operation not permitted."));
        };
        let batch = muc_admin_for_remote(self.state, iq, &requester, &self.evidence).await;
        self.answered(batch).await
    }

    /// XEP-0045 `muc#owner` (configuration and destruction) and XEP-0425
    /// moderation, decided as [`Self::admin`] is.
    async fn owner_or_moderation(&self, iq: &Iq) -> RoomReplies {
        let Some(requester) = self.request.principal.full() else {
            return self.refuse(forbidden("Operation not permitted."));
        };
        let batch =
            muc_owner_or_moderation_for_remote(self.state, iq, &requester, &self.evidence).await;
        // The handler answers nothing to a moderation element that is no
        // request to retract a message; an IQ request is always answered
        // (RFC 6120 §8.2.3). An owner's destruction may answer nothing
        // here: its completion is then delivered by the room's outbox.
        if self.request.operation == RoomOperation::Moderation
            && batch.frames.is_empty()
            && batch.completions.is_empty()
        {
            return self.refuse(error(
                ErrorType::Modify,
                DefinedCondition::BadRequest,
                "Malformed moderation request.",
            ));
        }
        self.answered(batch).await
    }

    /// The answer of a handler written for sessions, for the remote
    /// requester. A session's connection settles what the batch carries
    /// beside its frames once its writer has taken them; here the answer is
    /// handed to the router's queue for the sender's domain, which is all
    /// the custody a reply to a remote sender has, so they are settled as
    /// the answer is returned. A room effect's completion can wait for
    /// other occupants' connections to take their copies, so it is settled
    /// off the room's request order.
    async fn answered(&self, batch: ResponseBatch) -> RoomReplies {
        let ResponseBatch {
            frames,
            completions,
            mut ingress_reports,
            ..
        } = batch;
        let stanzas: Option<Vec<Stanza>> = frames.into_iter().map(stanza_of).collect();
        for report in &mut ingress_reports {
            let completed = self
                .state
                .deps
                .protocol
                .ingress
                .complete_frame_obligations(report)
                .await;
            if completed.is_err() {
                tracing::warn!("recording a remote principal's answered room request failed");
            }
        }
        for completion in completions {
            let state = Arc::clone(self.state);
            tokio::spawn(async move {
                // An effect whose completion is not recorded stays leased
                // and is delivered again by the room's outbox.
                let recorded =
                    crate::room_effect_outbox::drain::complete_after_write(&state, &completion)
                        .await;
                if recorded.is_err() {
                    tracing::warn!("recording a remote principal's delivered room effect failed");
                }
            });
        }
        match stanzas {
            Some(stanzas) => RoomReplies::of(stanzas),
            // The request may have been applied, and its answer cannot be
            // given in full.
            None => self.refuse(internal()),
        }
    }

    // --- messages -------------------------------------------------------------

    async fn message(&self, message: &Message) -> RoomReplies {
        if message.type_ == MessageType::Error {
            return RoomReplies::none();
        }
        let room = match (message.type_.clone(), self.room(), self.to.resource()) {
            (MessageType::Groupchat, Some(room), None) => room,
            // Private messages, invitations, voice requests and declines.
            _ => {
                return self.refuse(error(
                    ErrorType::Cancel,
                    DefinedCondition::FeatureNotImplemented,
                    "Only messages to the room are accepted from occupants of other servers.",
                ));
            }
        };
        let Some(occupant) = self
            .request
            .principal
            .full()
            .filter(|_| self.request.seat.is_some())
        else {
            // XEP-0045 §7.4: only an occupant sends to the room.
            return self.refuse(error(
                ErrorType::Modify,
                DefinedCondition::NotAcceptable,
                "You are not an occupant of this room.",
            ));
        };
        let Some(mut message) = reduced_message(message) else {
            // Nothing a room carries between occupants: not relayed and
            // not answered.
            return RoomReplies::none();
        };
        // XEP-0045 §8.1: a room lets its moderators change the subject,
        // and its other occupants when its configuration says so. A
        // moderator's standing counts for a remote occupant only as D8
        // honours it for the stream that carried this request; the room
        // then applies its own check to the occupant's role.
        if waddle_xmpp::muc::is_groupchat_subject_change_message(&message) {
            match self.may_ask_subject_change(&room).await {
                Some(true) => {}
                Some(false) => {
                    return self.refuse(forbidden(
                        "Occupants from other servers may change the subject only where the \
                         room lets every occupant, or as a moderator over a \
                         certificate-authenticated stream from a trusted server.",
                    ));
                }
                // Whether the room lets it cannot be told now: the sender
                // may try again, and is not told that it may not.
                None => return self.refuse(internal()),
            }
        }
        // What an admitted domain can make this deployment store is
        // bounded for each of its accounts as well as for the domain. A
        // message of a kind the room archives is counted when it is put to
        // the room, whether or not the room then accepts it.
        if is_archivable(&message) && !self.archive_writes.take(&self.account) {
            archive_write_limited();
            return self.refuse(error(
                ErrorType::Wait,
                DefinedCondition::ResourceConstraint,
                "Too many messages; try again later.",
            ));
        }
        message.from = Some(Jid::from(occupant.clone()));
        message.to = Some(Jid::from(room.clone()));
        let target = NormalizedTarget::Bare(room.clone());
        let Ok(digest_input) = crate::ingress::submission::digest_input(
            &message,
            &DigestContext {
                target: target.clone(),
                // The room is the only authority of this deployment that
                // stamps the message; the sender's own archive is its
                // server's.
                server_authorities: vec![room.clone()],
                stanza_lang: None,
            },
        ) else {
            return self.refuse(error(
                ErrorType::Modify,
                DefinedCondition::BadRequest,
                "The message cannot be accepted as sent.",
            ));
        };
        let deps = build_interpret_deps(self.state, None);
        #[cfg(feature = "clustering")]
        let deps = deps.with_ordered_relay_origin(Some(
            crate::server::routes::interpret::OrderedRelayRouteOrigin::room(&room),
        ));
        let plan = plan_remote_occupant_groupchat(&deps, room.clone(), message).await;
        // A request for a room another replica holds was carried there
        // before it reached this host; the room's claim moved since.
        if matches!(plan.room_execution, RoomExecutionPath::Remote { .. }) {
            return self.refuse(error(
                ErrorType::Wait,
                DefinedCondition::ResourceConstraint,
                "This room is moving between server nodes; try again later.",
            ));
        }
        let submission = IngressSubmission {
            identity: IngressStreamIdentity::Remote {
                account: self.account.clone(),
            },
            principal: IngressPrincipal::Remote(RemoteOccupantPrincipal {
                account: self.account.clone(),
                evidence: StreamEvidence::of(&self.request.principal),
            }),
            sender: occupant,
            target,
            plan,
            digest_input,
            connection_generation: TransportGeneration::Host,
        };
        let authority = &self.state.deps.protocol.ingress;
        let decision = authority.commit(&submission).await;
        if !decision.class.advances() {
            return self.refuse(internal());
        }
        // The acceptance is committed: its effects run within their own
        // budget, and an effect that does not finish is recovered by the
        // ingress authority, not answered here.
        let executed = authority.execute(&decision, &ImmediateSink, &deps);
        let Ok(mut report) = tokio::time::timeout(MESSAGE_EFFECTS_TIMEOUT, executed).await else {
            return RoomReplies::none();
        };
        let stanzas = report
            .frame_obligations
            .iter()
            .flat_map(|obligation| obligation.frames.iter())
            .map(|frame| frame.stanza.clone())
            .collect();
        // The answer is handed to the router's queue for the sender's
        // domain, which is all the custody a reply to a remote sender has.
        if authority
            .complete_frame_obligations(&mut report)
            .await
            .is_err()
        {
            tracing::warn!("recording a remote occupant's answered room message failed");
        }
        RoomReplies::of(stanzas)
    }
}

#[async_trait]
impl LocalRooms for WebSocketLocalRooms {
    async fn handle(&self, request: RoomRequest) -> RoomReplies {
        let Some(state) = self.state.upgrade() else {
            return RoomReplies::none();
        };
        let to = match &request.stanza {
            Stanza::Iq(iq) => iq.to().cloned(),
            Stanza::Message(message) => message.to.clone(),
            Stanza::Presence(presence) => presence.to.clone(),
        };
        let Some(to) = to else {
            return RoomReplies::none();
        };
        let asked = Asked {
            state: &state,
            archive_writes: &self.archive_writes,
            request: &request,
            to: &to,
            account: request.principal.account().as_bare_jid().clone(),
            evidence: PrincipalEvidence::of(&request.principal),
        };
        // The D1 decision where the effect happens: a domain the snapshot
        // this replica runs does not admit, or a stream below host policy's
        // strength, is refused whatever the room holds for the account.
        let admitted = principal_witness(&state, &asked.account, &asked.evidence)
            .is_some_and(|witness| witness.grants_honoured());
        if !admitted {
            return RoomReplies {
                stanzas: refuse(
                    &request.stanza,
                    forbidden("This service does not admit your server."),
                ),
                seat: SeatChange::Refused,
            };
        }
        // A room that is not open to other servers answers every request
        // that could tell it from a room that does not exist the same way.
        // A ping of a room's bare JID and a `disco#items` request are
        // answered alike for every address, presence from a sender without
        // a seat that asks for none is ignored, and a message from such a
        // sender is refused before its room is looked at, so those say
        // nothing about the room either.
        let names_room = match request.operation {
            RoomOperation::DiscoInfo
            | RoomOperation::Admin
            | RoomOperation::Owner
            | RoomOperation::Moderation => true,
            // A query addressed to an occupant JID is refused the same way
            // whatever its room is.
            RoomOperation::Archive => to.resource().is_none(),
            RoomOperation::Presence | RoomOperation::Message => request.seat.is_some(),
            RoomOperation::DiscoItems | RoomOperation::Ping => false,
        };
        if let Some(room) = asked.room().filter(|_| names_room) {
            if let Some(refused) = asked.closed_room(&room).await {
                return refused;
            }
        }
        match (request.operation, &request.stanza) {
            (RoomOperation::DiscoInfo, Stanza::Iq(iq)) => asked.disco_info(iq).await,
            (RoomOperation::DiscoItems, Stanza::Iq(iq)) => asked.disco_items(iq),
            (RoomOperation::Ping, Stanza::Iq(iq)) => asked.ping(iq).await,
            (RoomOperation::Archive, Stanza::Iq(iq)) => asked.archive(iq).await,
            (RoomOperation::Presence, Stanza::Presence(presence)) => asked.presence(presence).await,
            (RoomOperation::Message, Stanza::Message(message)) => asked.message(message).await,
            (RoomOperation::Admin, Stanza::Iq(iq)) => asked.admin(iq).await,
            (RoomOperation::Owner | RoomOperation::Moderation, Stanza::Iq(iq)) => {
                asked.owner_or_moderation(iq).await
            }
            _ => asked.refuse(unavailable()),
        }
    }

    async fn place(&self, principal: &RemotePrincipal, stanza: &Stanza) -> RoomPlacement {
        let Some(carrier) = &self.carrier else {
            return RoomPlacement::Here;
        };
        let to = match stanza {
            Stanza::Iq(iq) => iq.to(),
            Stanza::Message(message) => message.to.as_ref(),
            Stanza::Presence(presence) => presence.to.as_ref(),
        };
        // The service itself is every replica's.
        match to.filter(|to| to.node().is_some()) {
            Some(to) => carrier.place(&to.to_bare(), principal, stanza).await,
            None => RoomPlacement::Here,
        }
    }

    async fn unseat(&self, seat: &Seat, end: SeatEnd) -> bool {
        tracing::debug!(?end, "removing a remote occupant whose seat ended");
        // Whether the room seats this occupancy now: only then is it this
        // call's to remove. A room that cannot say is asked to remove it
        // all the same, without that being taken for a removal.
        let seated = self.seats(seat).await;
        if seated == Some(false) {
            return false;
        }
        self.remove_unseated(&seat.room, &seat.occupant, seat.session)
            .await;
        seated == Some(true)
    }

    async fn remove_unseated(
        &self,
        room: &BareJid,
        occupant: &FullJid,
        session: waddle_xmpp_core::OccupancySessionGeneration,
    ) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        // The removal is pinned to the occupancy generation, so a later
        // join of the same full JID keeps its seat. Those who stay are
        // told with status 333 (XEP-0045 §11.1); the removed occupant's
        // own copy finds no seat and is not sent.
        sweep_abandoned_room_occupancy(
            &state,
            room,
            occupant,
            LeaveSessionSelector::Generation(session),
            MucRemovalCause::TechnicalProblem,
        )
        .await;
    }

    async fn seats(&self, seat: &Seat) -> Option<bool> {
        let state = self.state.upgrade()?;
        // A room whose actor cannot be reached now (it is being restored,
        // or its claim is moving) may seat the occupant: only a room this
        // replica holds no actor for seats nobody.
        let Some(actor) = get_room_actor_result(&state, &seat.room).await.ok()? else {
            return Some(false);
        };
        let seated = actor
            .ask(GetOccupantSessionGeneration {
                jid: seat.occupant.clone(),
            })
            .reply_timeout(ROOM_ASK_TIMEOUT)
            .await
            .ok()?;
        Some(seated == Some(seat.session))
    }

    fn carries(&self) -> bool {
        self.carrier.is_some()
    }

    async fn occupancies(
        &self,
        room: &BareJid,
    ) -> Option<Vec<(FullJid, waddle_xmpp_core::OccupancySessionGeneration)>> {
        let state = self.state.upgrade()?;
        // A room without an actor here seats nobody here; one whose actor
        // cannot be reached now is not answered for.
        let Some(actor) = get_room_actor_result(&state, room).await.ok()? else {
            return Some(Vec::new());
        };
        actor
            .ask(GetSeatedSessionGenerations)
            .reply_timeout(ROOM_ASK_TIMEOUT)
            .await
            .ok()
    }
}
