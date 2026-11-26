//! XEP-0045 voice requests (§7.13) and their approval (§8.6).
//!
//! A visitor's `muc#request` form, sent to the room, becomes a voice approval
//! form from the room to every moderator session; a moderator's submitted
//! approval is applied as the §8.3 role change through the same `muc#admin`
//! path an IQ takes, so authorization, durability and the presence broadcast
//! are the ones the IQ gets. Neither form is archived or carbon-copied: both
//! are room control traffic, answered by this handler instead of the message
//! ingress pipeline.
//!
//! The decision runs where the room's actor lives ([`decide_voice_form`]).
//! A session on another replica of the cluster hands its form to the room's
//! owner over the relay (`muc_voice_relay`), and a principal at another
//! server reaches it through the S2S room host, which already runs on the
//! owner; all three apply the same decision.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
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
#[cfg(feature = "clustering")]
use std::sync::Arc;

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
        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
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

/// What the room holding the actor decided about a form.
pub(crate) enum VoiceDecision {
    /// This node runs no actor for the room.
    NotHere,
    /// The room answered; the batch holds the frames for the sender.
    Answered(ResponseBatch),
}

/// Who sent a form to the room and the stream evidence its standing is
/// honoured under: none for a session of this deployment, and for a
/// principal at another server the S2S stream that carried it.
pub(crate) struct VoiceSender<'a> {
    pub jid: &'a FullJid,
    pub evidence: &'a PrincipalEvidence,
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
    let form = match parse_voice_form(&incoming) {
        Ok(Some(form)) => form,
        Ok(None) => return ResponseBatch::default(),
        Err(_) => return reject_invalid_form(&incoming, bound_jid, &deps),
    };
    let Some(room_jid) = incoming.to.as_ref().map(Jid::to_bare) else {
        return ResponseBatch::default();
    };
    let sender = VoiceSender {
        jid: bound_jid,
        evidence: &PrincipalEvidence::NoStream,
    };
    match decide_voice_form(
        &incoming, state, &deps, &sender, &room_jid, form, muc_domain,
    )
    .await
    {
        VoiceDecision::Answered(batch) => batch,
        VoiceDecision::NotHere => {
            #[cfg(feature = "clustering")]
            {
                use super::muc_voice_relay::{forward_voice_form, ForwardedVoice};
                return match forward_voice_form(state, &room_jid, bound_jid, &incoming).await {
                    ForwardedVoice::Frames(stanzas) => ResponseBatch::from(stanzas),
                    ForwardedVoice::NoOwner => reject_room_not_found(&incoming, bound_jid, &deps),
                    ForwardedVoice::Unavailable => ResponseBatch::from(vec![message_error_frame(
                        &incoming,
                        bound_jid,
                        &deps,
                        ErrorType::Wait,
                        DefinedCondition::ResourceConstraint,
                        "The room's owner is unavailable; try again shortly.",
                    )]),
                };
            }
            #[cfg(not(feature = "clustering"))]
            reject_room_not_found(&incoming, bound_jid, &deps)
        }
    }
}

fn reject_invalid_form(incoming: &Message, sender: &FullJid, deps: &Deps<'_>) -> ResponseBatch {
    ResponseBatch::from(vec![message_error_frame(
        incoming,
        sender,
        deps,
        ErrorType::Modify,
        DefinedCondition::BadRequest,
        "The voice request form is not valid.",
    )])
}

fn reject_room_not_found(incoming: &Message, sender: &FullJid, deps: &Deps<'_>) -> ResponseBatch {
    ResponseBatch::from(vec![message_error_frame(
        incoming,
        sender,
        deps,
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        "Requested room not found.",
    )])
}

/// Decide `form`, which `sender` put to the room, against the room's actor
/// on this node: a visitor's request goes to the moderators (§7.13) and a
/// moderator's approval is applied as the role change (§8.6).
pub(crate) async fn decide_voice_form(
    incoming: &Message,
    state: &WebSocketState,
    deps: &Deps<'_>,
    sender: &VoiceSender<'_>,
    room_jid: &BareJid,
    form: VoiceForm,
    muc_domain: &str,
) -> VoiceDecision {
    let reject = |condition, error_type, text| {
        VoiceDecision::Answered(ResponseBatch::from(vec![message_error_frame(
            incoming, sender.jid, deps, error_type, condition, text,
        )]))
    };
    let snapshot = match room_snapshot(state, room_jid).await {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return VoiceDecision::NotHere,
        Err(()) => {
            return reject(
                DefinedCondition::InternalServerError,
                ErrorType::Wait,
                "Internal server error.",
            );
        }
    };
    let Some(occupant) = snapshot.room.find_occupant_by_real_jid(sender.jid) else {
        return reject(
            DefinedCondition::NotAcceptable,
            ErrorType::Cancel,
            "Only room occupants may use voice requests.",
        );
    };
    match form {
        VoiceForm::Request => {
            if occupant.role != Role::Visitor {
                // Only a visitor lacks voice to ask for.
                return VoiceDecision::Answered(ResponseBatch::default());
            }
            if !admit_voice_request(room_jid, &sender.jid.to_bare(), Instant::now()) {
                return reject(
                    DefinedCondition::ResourceConstraint,
                    ErrorType::Wait,
                    "A voice request was sent recently; try again later.",
                );
            }
            relay_request_to_moderators(&snapshot, room_jid, sender.jid, &occupant.nick, deps)
                .await;
            VoiceDecision::Answered(ResponseBatch::default())
        }
        VoiceForm::Approval { jid, nick, allow } => {
            let moderates = occupant.role == Role::Moderator
                || matches!(
                    occupant.affiliation,
                    Affiliation::Owner | Affiliation::Admin
                );
            if !moderates {
                return reject(
                    DefinedCondition::Forbidden,
                    ErrorType::Auth,
                    "Only a moderator may answer a voice request.",
                );
            }
            if !allow {
                return VoiceDecision::Answered(ResponseBatch::default());
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
            VoiceDecision::Answered(
                grant_voice(incoming, state, sender, room_jid, &nick, muc_domain).await,
            )
        }
    }
}

/// The decision of a form a replica carried here for one of its sessions:
/// the frames the room answers the sender with. `None` when this node runs
/// no actor for the room, and nothing was decided.
#[cfg(feature = "clustering")]
pub(crate) async fn relayed_voice_form(
    state: &Arc<WebSocketState>,
    message: crate::clustering::relay::RelayMucVoiceForm,
) -> crate::clustering::relay::RelayMucVoiceFormReply {
    use crate::clustering::codec::RemoteStanza;
    use crate::clustering::relay::RelayMucVoiceFormReply;
    let RemoteStanza(Stanza::Message(mut incoming)) = message.message else {
        return RelayMucVoiceFormReply::Handled(Vec::new());
    };
    // The origin vouches for the session; the form says nothing of who sent it.
    incoming.from = Some(Jid::from(message.sender.clone()));
    incoming.to = Some(Jid::from(message.room.clone()));
    let Ok(Some(form)) = parse_voice_form(&incoming) else {
        return RelayMucVoiceFormReply::Handled(Vec::new());
    };
    let deps = build_interpret_deps(state, None);
    let sender = VoiceSender {
        jid: &message.sender,
        evidence: &PrincipalEvidence::NoStream,
    };
    let muc_domain = state.deps.service_domains.muc.as_str();
    let decision = decide_voice_form(
        &incoming,
        state,
        &deps,
        &sender,
        &message.room,
        form,
        muc_domain,
    )
    .await;
    match decision {
        VoiceDecision::NotHere => RelayMucVoiceFormReply::NotOwner,
        VoiceDecision::Answered(batch) => {
            match super::super::s2s_rooms::settle_answered(state, batch).await {
                Some(stanzas) => {
                    RelayMucVoiceFormReply::Handled(stanzas.into_iter().map(RemoteStanza).collect())
                }
                None => {
                    tracing::warn!(room = %message.room, "a relayed voice form's answer was not a stanza");
                    RelayMucVoiceFormReply::Handled(Vec::new())
                }
            }
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
    sender: &VoiceSender<'_>,
    room_jid: &BareJid,
    nick: &str,
    muc_domain: &str,
) -> ResponseBatch {
    let moderator = sender.jid;
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
        sender.evidence,
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
