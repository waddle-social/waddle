//! XEP-0045 voice requests (§7.13) and their approval (§8.6).
//!
//! A visitor's `muc#request` form, sent to the room, becomes a voice approval
//! form from the room to every moderator session; a moderator's submitted
//! approval is applied as the §8.3 role change through the same `muc#admin`
//! path an IQ takes, so authorization, durability and the presence broadcast
//! are the ones the IQ gets. Neither form is archived or carbon-copied: both
//! are room control traffic, answered by this handler instead of the message
//! ingress pipeline.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use jid::{BareJid, FullJid, Jid};
use waddle_xmpp::muc::room_actor::{GetSnapshot, RoomSnapshot};
use waddle_xmpp::muc::room_registry_actor::GetRoom;
use waddle_xmpp::muc::voice::{build_voice_approval_message, parse_voice_form, VoiceForm};
use waddle_xmpp::protocol::handlers::errors::message_error_reply;
use waddle_xmpp::protocol::ConnectionPhase;
use waddle_xmpp::{Affiliation, Role, Stanza};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::super::frame::{ResponseBatch, ResponseFrame};
use super::super::interpret_loop::build_interpret_deps;
use super::super::{ResolvedPrincipal, WebSocketState};
use super::iq::handle_muc_admin_iq;
use super::message::muc_direct::{deliver_pm_to_session, message_error_frame};
use crate::auth::Session;
use crate::permissions::host_grant::PrincipalEvidence;
use crate::server::routes::interpret::Deps;

/// A visitor may ask again this long after its last request in a room; a
/// request sooner is refused, so one occupant cannot flood the moderators.
const VOICE_REQUEST_MIN_INTERVAL: Duration = Duration::from_secs(30);
/// Above this many remembered requests, the expired ones are dropped.
const VOICE_REQUEST_TRACKED_LIMIT: usize = 4096;
const ROOM_ASK_TIMEOUT: Duration = Duration::from_secs(5);

static RECENT_VOICE_REQUESTS: LazyLock<Mutex<HashMap<(BareJid, BareJid), Instant>>> =
    LazyLock::new(Default::default);

/// Whether `message` is a voice request or approval for a room of this
/// service: it is then answered here and never enters the ingress pipeline.
pub(crate) fn is_voice_form(message: &Message, muc_domain: &str) -> bool {
    message.to.as_ref().is_some_and(|to| {
        to.node().is_some() && to.resource().is_none() && to.domain().as_str() == muc_domain
    }) && !matches!(parse_voice_form(message), Ok(None))
}

fn admit_voice_request(room: &BareJid, requester: &BareJid, now: Instant) -> bool {
    let mut recent = RECENT_VOICE_REQUESTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if recent.len() >= VOICE_REQUEST_TRACKED_LIMIT {
        recent.retain(|_, at| now.duration_since(*at) < VOICE_REQUEST_MIN_INTERVAL);
    }
    let key = (room.clone(), requester.clone());
    if recent
        .get(&key)
        .is_some_and(|at| now.duration_since(*at) < VOICE_REQUEST_MIN_INTERVAL)
    {
        return false;
    }
    recent.insert(key, now);
    true
}

pub(crate) async fn handle_voice_form(
    incoming: Message,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    authenticated_session: Option<&Session>,
    muc_domain: &str,
) -> ResponseBatch {
    let Some(bound_jid) = phase.bound_jid() else {
        return ResponseBatch::default();
    };
    let principal = authenticated_session.map(ResolvedPrincipal::from_authenticated_session);
    let deps = build_interpret_deps(state, principal);
    let reject = |condition, error_type, text| {
        ResponseBatch::from(vec![message_error_frame(
            &incoming, bound_jid, &deps, error_type, condition, text,
        )])
    };
    let form = match parse_voice_form(&incoming) {
        Ok(Some(form)) => form,
        Ok(None) => return ResponseBatch::default(),
        Err(_) => {
            return reject(
                DefinedCondition::BadRequest,
                ErrorType::Modify,
                "The voice request form is not valid.",
            );
        }
    };
    let Some(room_jid) = incoming.to.as_ref().map(Jid::to_bare) else {
        return ResponseBatch::default();
    };
    let snapshot = match room_snapshot(state, &room_jid).await {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            return reject(
                DefinedCondition::ItemNotFound,
                ErrorType::Cancel,
                "Requested room not found.",
            );
        }
        Err(()) => {
            return reject(
                DefinedCondition::InternalServerError,
                ErrorType::Wait,
                "Internal server error.",
            );
        }
    };
    let Some(sender) = snapshot.room.find_occupant_by_real_jid(bound_jid) else {
        return reject(
            DefinedCondition::NotAcceptable,
            ErrorType::Cancel,
            "Only room occupants may use voice requests.",
        );
    };
    match form {
        VoiceForm::Request => {
            if sender.role != Role::Visitor {
                // Only a visitor lacks voice to ask for.
                return ResponseBatch::default();
            }
            if !admit_voice_request(&room_jid, &bound_jid.to_bare(), Instant::now()) {
                return reject(
                    DefinedCondition::ResourceConstraint,
                    ErrorType::Wait,
                    "A voice request was sent recently; try again later.",
                );
            }
            relay_request_to_moderators(&snapshot, &room_jid, bound_jid, &sender.nick, &deps).await;
            ResponseBatch::default()
        }
        VoiceForm::Approval { jid, nick, allow } => {
            let moderates = sender.role == Role::Moderator
                || matches!(sender.affiliation, Affiliation::Owner | Affiliation::Admin);
            if !moderates {
                return reject(
                    DefinedCondition::Forbidden,
                    ErrorType::Auth,
                    "Only a moderator may answer a voice request.",
                );
            }
            if !allow {
                return ResponseBatch::default();
            }
            let names_the_requester = snapshot
                .room
                .occupant(&nick)
                .is_some_and(|occupant| occupant.real_jid.to_bare() == jid.to_bare());
            if !names_the_requester {
                return reject(
                    DefinedCondition::ItemNotFound,
                    ErrorType::Cancel,
                    "Requested occupant not found.",
                );
            }
            grant_voice(&incoming, state, bound_jid, &room_jid, &nick, muc_domain).await
        }
    }
}

async fn room_snapshot(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<Option<RoomSnapshot>, ()> {
    let room = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .reply_timeout(ROOM_ASK_TIMEOUT)
        .await
        .map_err(|_| ())?;
    let Some(room_actor) = room else {
        return Ok(None);
    };
    room_actor
        .ask(GetSnapshot)
        .reply_timeout(ROOM_ASK_TIMEOUT)
        .await
        .map(Some)
        .map_err(|_| ())
}

/// §8.6: the approval form goes from the room to every moderator session.
async fn relay_request_to_moderators(
    snapshot: &RoomSnapshot,
    room_jid: &BareJid,
    requester: &FullJid,
    requester_nick: &str,
    deps: &Deps<'_>,
) {
    let moderator_sessions: Vec<FullJid> = snapshot
        .room
        .occupants
        .values()
        .filter(|occupant| occupant.role == Role::Moderator)
        .flat_map(|occupant| snapshot.room.occupant_sessions(&occupant.nick))
        .collect();
    for session in moderator_sessions {
        let form = build_voice_approval_message(
            room_jid,
            &session,
            &uuid::Uuid::new_v4().to_string(),
            requester,
            requester_nick,
        );
        deliver_pm_to_session(deps, &session, &Stanza::Message(form)).await;
    }
}

/// Apply the approved request as the §8.3 role change of `nick`, then answer
/// the moderator as a message rather than an IQ: the moderator sent a message.
async fn grant_voice(
    incoming: &Message,
    state: &WebSocketState,
    moderator: &FullJid,
    room_jid: &BareJid,
    nick: &str,
    muc_domain: &str,
) -> ResponseBatch {
    let item = Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
        .attr(minidom::rxml::xml_ncname!("nick").to_owned(), nick)
        .attr(minidom::rxml::xml_ncname!("role").to_owned(), "participant")
        .build();
    let query = Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
        .append(item)
        .build();
    let iq = Iq::Set {
        from: Some(Jid::from(moderator.clone())),
        to: Some(Jid::from(room_jid.clone())),
        id: format!("voice-approval-{}", uuid::Uuid::new_v4()),
        payload: query,
    };
    let batch = handle_muc_admin_iq(
        &iq,
        muc_domain,
        state,
        Some(moderator),
        &PrincipalEvidence::NoStream,
        None,
        None,
    )
    .await;
    answer_as_message(batch, incoming, moderator)
}

/// Replace the IQ result with nothing and an IQ error with the same error on
/// the moderator's message; every other frame (the presence updates) stays.
fn answer_as_message(
    batch: ResponseBatch,
    incoming: &Message,
    moderator: &FullJid,
) -> ResponseBatch {
    let ResponseBatch {
        frames,
        completions,
        completion_frame_indices,
        ingress_reports,
    } = batch;
    let mut kept = Vec::with_capacity(frames.len());
    let mut new_index = HashMap::new();
    for (index, frame) in frames.into_iter().enumerate() {
        let is_completion_frame = completion_frame_indices.contains(&index);
        let replacement = match &frame {
            ResponseFrame::Stanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(iq) => match iq.as_ref() {
                    Iq::Result { .. } => None,
                    Iq::Error { error, .. } => Some(error_message(incoming, moderator, error)),
                    _ => Some(frame.clone()),
                },
                _ => Some(frame.clone()),
            },
            _ => Some(frame.clone()),
        };
        // A frame that carries a room-effect completion is never dropped.
        let replacement = replacement.or(is_completion_frame.then_some(frame));
        if let Some(replacement) = replacement {
            new_index.insert(index, kept.len());
            kept.push(replacement);
        }
    }
    ResponseBatch {
        frames: kept,
        completion_frame_indices: completion_frame_indices
            .iter()
            .filter_map(|index| new_index.get(index).copied())
            .collect(),
        completions,
        ingress_reports,
    }
}

fn error_message(incoming: &Message, moderator: &FullJid, error: &StanzaError) -> ResponseFrame {
    let mut stamped = incoming.clone();
    stamped.from = Some(Jid::from(moderator.clone()));
    ResponseFrame::from(Stanza::Message(message_error_reply(
        &stamped,
        error.clone(),
    )))
}
