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
//! joining, changing nickname and availability, leaving, sending text to
//! the room, reading its archive, `disco#info`, `disco#items` and ping.
//! Private messages, invitations, voice requests, room administration and
//! configuration, and every extension of this deployment (calls, pins,
//! hats, commands) are refused or dropped: presence from a remote occupant
//! is reduced to its availability, and a message to its text, before the
//! room sees it.

use std::sync::{Arc, Weak};
use std::time::Duration;

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
    GetOccupantSessionGeneration, LeaveSessionSelector, PingSelfCheck,
};
use waddle_xmpp::muc::MucRemovalCause;
use waddle_xmpp::Stanza;
use xmpp_parsers::disco::{DiscoInfoResult, DiscoItemsResult};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::cleanup::{get_room_actor, is_muc_room_jid, sweep_abandoned_room_occupancy};
use super::handlers::iq::muc_disco_info_for_remote;
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
    LocalRooms, RoomOperation, RoomReplies, RoomRequest, Seat, SeatChange, SeatEnd,
};
use crate::server::routes::interpret::effects::RoomExecutionPath;
use crate::server::routes::interpret::plan_remote_occupant_groupchat;

/// XEP-0045 status code 110: the presence is the occupant's own.
const STATUS_SELF: &str = "110";

/// Longest one ask of a room actor waits.
const ROOM_ASK_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest the effects of one accepted room message may take.
const MESSAGE_EFFECTS_TIMEOUT: Duration = Duration::from_secs(5);

/// [`LocalRooms`] over the WebSocket state's rooms, grants and archive.
///
/// It holds the state weakly: the state holds the router, which holds this.
pub struct WebSocketLocalRooms {
    state: Weak<WebSocketState>,
}

impl WebSocketLocalRooms {
    /// Host the rooms of `state` for remote occupants.
    pub fn new(state: &Arc<WebSocketState>) -> Self {
        Self {
            state: Arc::downgrade(state),
        }
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
pub(crate) fn host_rooms(
    state: &Arc<WebSocketState>,
    router: &crate::s2s::route::S2sRouter,
    global: crate::db::Database,
) {
    router.attach_rooms(
        Arc::new(WebSocketLocalRooms::new(state)),
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
}

/// One request, as its handlers read it.
struct Asked<'a> {
    state: &'a WebSocketState,
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

/// A remote occupant's presence as the room sees it: its availability
/// (`show`, `status`, `priority`) and, for an attempt to enter, the bare
/// `muc` element. Every other payload is dropped, as XEP-0045 §17.1.1 lets
/// a service do: the presence extensions of this deployment (calls, hats)
/// carry meaning only for its own sessions.
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
    reduced
}

/// A remote occupant's message as the room sees it: its text, its thread
/// and the identifier its client gave it (XEP-0359 `origin-id`). Every
/// other payload is dropped before the room plans the message. The room's
/// pipeline acts on several of them (a subject change, a pin, a forum
/// action, a displayed marker, a correction, retraction or reaction naming
/// an archived message), and none of those has been reviewed for a remote
/// principal. `None` when the message carries no text.
fn reduced_message(message: &Message) -> Option<Message> {
    if message.bodies.is_empty() {
        return None;
    }
    let mut reduced = Message::new_with_type(MessageType::Groupchat, message.to.clone());
    reduced.from = message.from.clone();
    reduced.id = message.id.clone();
    reduced.bodies = message.bodies.clone();
    reduced.thread = message.thread.clone();
    reduced.payloads = message
        .payloads
        .iter()
        .filter(|payload| payload.is("origin-id", waddle_xmpp_core::xep0359::NS_SID))
        .cloned()
        .collect();
    Some(reduced)
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

    // --- service discovery and ping ------------------------------------------

    async fn disco_info(&self, iq: &Iq) -> RoomReplies {
        let Iq::Get { payload, .. } = iq else {
            return self.refuse(unavailable());
        };
        // No node of the service or of a room is described to remote
        // askers.
        if payload.attr("node").is_some() {
            return self.refuse(error(
                ErrorType::Cancel,
                DefinedCondition::ItemNotFound,
                "No such node.",
            ));
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
            return self.refuse(error(
                ErrorType::Cancel,
                DefinedCondition::ItemNotFound,
                "Requested item not found.",
            ));
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
            // The one error XEP-0410 §4 has a client take no decision from.
            RoomGate::Unavailable => {
                return self.refuse(error(
                    ErrorType::Wait,
                    DefinedCondition::RemoteServerTimeout,
                    "The room's grants could not be read; try again.",
                ));
            }
        }
        let Some(actor) = get_room_actor(self.state, &seat.room).await else {
            return not_joined();
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
            Err(_) => self.refuse(internal()),
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
        let not_found = || {
            self.refuse(error(
                ErrorType::Cancel,
                DefinedCondition::ItemNotFound,
                "Requested item not found.",
            ))
        };
        if !is_muc_room_jid(self.state, &room).await {
            return not_found();
        }
        let Ok(channel) = get_managed_channel_for_room(self.state, &room).await else {
            return self.refuse(internal());
        };
        // A group conversation of accounts of this deployment has no remote
        // member.
        if channel.as_ref().is_some_and(|channel| {
            channel.channel_type == waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM
        }) {
            return self.refuse(forbidden("Operation not permitted."));
        }
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
            // The room does not seat the occupant any more: the presence
            // is a join again.
        }
        match get_managed_channel_for_room(self.state, room).await {
            // A group conversation of accounts of this deployment admits
            // no remote occupant.
            Ok(Some(channel))
                if channel.channel_type == waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM =>
            {
                return RoomReplies {
                    stanzas: refuse(
                        &self.request.stanza,
                        forbidden("This room does not admit occupants from other servers."),
                    ),
                    seat: SeatChange::Refused,
                };
            }
            Ok(_) => {}
            Err(_) => {
                return RoomReplies {
                    stanzas: refuse(&self.request.stanza, internal()),
                    seat: SeatChange::Refused,
                };
            }
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
                // A room another replica holds refuses the join here: a
                // remote principal's request is decided where its stream
                // landed.
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
            // XEP-0045 §8.1: a subject change by an occupant that may not
            // make one is `forbidden`. Anything else without text (a chat
            // state, a marker, a reaction) is not relayed and not answered.
            return if message.subjects.is_empty() {
                RoomReplies::none()
            } else {
                self.refuse(forbidden(
                    "Occupants from other servers cannot change the room subject.",
                ))
            };
        };
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
        if matches!(plan.room_execution, RoomExecutionPath::Remote { .. }) {
            return self.refuse(error(
                ErrorType::Wait,
                DefinedCondition::ResourceConstraint,
                "This room is held by another server node; try again later.",
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
        match (request.operation, &request.stanza) {
            (RoomOperation::DiscoInfo, Stanza::Iq(iq)) => asked.disco_info(iq).await,
            (RoomOperation::DiscoItems, Stanza::Iq(iq)) => asked.disco_items(iq),
            (RoomOperation::Ping, Stanza::Iq(iq)) => asked.ping(iq).await,
            (RoomOperation::Archive, Stanza::Iq(iq)) => asked.archive(iq).await,
            (RoomOperation::Presence, Stanza::Presence(presence)) => asked.presence(presence).await,
            (RoomOperation::Message, Stanza::Message(message)) => asked.message(message).await,
            _ => asked.refuse(unavailable()),
        }
    }

    async fn unseat(&self, seat: &Seat, end: SeatEnd) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        tracing::debug!(?end, "removing a remote occupant whose seat ended");
        // The removal is pinned to the seat's occupancy generation, so a
        // later join of the same full JID keeps its seat. Those who stay
        // are told with status 333 (XEP-0045 §11.1); the removed occupant's
        // own copy finds no seat and is not sent.
        sweep_abandoned_room_occupancy(
            &state,
            &seat.room,
            &seat.occupant,
            LeaveSessionSelector::Generation(seat.session),
            MucRemovalCause::TechnicalProblem,
        )
        .await;
    }

    async fn seats(&self, seat: &Seat) -> Option<bool> {
        let state = self.state.upgrade()?;
        let Some(actor) = get_room_actor(&state, &seat.room).await else {
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
}
