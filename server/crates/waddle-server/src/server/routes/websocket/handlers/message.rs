use chrono::Utc;
use jid::{BareJid, FullJid};
use tracing::{debug, info, warn};
use waddle_xmpp::{
    inbox::{
        runtime::{groupchat_entry, groupchat_thread_entry, preview_text, should_project_message},
        InboxEntry,
    },
    mam::{
        add_stanza_id as add_mam_stanza_id, ArchivedMention, ArchivedMessage, ArchivedReactionSet,
        ArchivedReference, ArchivedReply, ArchivedRetraction, ArchivedRichMessage,
        ArchivedRichPayload, RichMessageId, RichText, STANZA_ID_NS,
    },
    muc::room_actor::{BuildGroupchatBroadcast, GetNicknameGeneration, RoomActor, RoomActorError},
    parser::message_to_string,
    protocol::{frame::InboundFrame, InboundEvent, XmppStateMachine},
    registry::BroadcastOutcome,
    xep::xep0430::build_inbox_push,
    xep::{
        extract_correction_from_message, extract_explicit_mentions, extract_reactions_from_message,
        extract_references_from_message, extract_retraction_from_message, has_file_sharing,
        is_moderation_request_message, is_moderation_result_message, is_reaction_message,
        is_retraction_message, is_sticker_message, parse_reply_from_message, remove_stanza_ids_by,
        should_skip_storage, RetractionKind, NS_EXPLICIT_MENTIONS, NS_MESSAGE_CORRECT,
        NS_MESSAGE_RETRACT, NS_REACTIONS, NS_REFERENCE, NS_REPLY,
    },
    Stanza,
};
use xmpp_parsers::message::MessageType as XmppMessageType;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::super::{
    build_interpret_deps, drive_interpret_loop, get_room_actor, stanza_to_xml, WebSocketState,
};
use crate::auth::Session;
use crate::permissions::{CheckPermission, Object, ObjectType, Permission, Subject};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use kameo::actor::ActorRef;
use waddle_xmpp::protocol::ConnectionPhase;

/// Thin transport adapter that drives the sans-I/O dispatcher
/// (#229 PR16). The state machine's [`InboundEvent::FrameReceived`]
/// path runs the locked-Q2(a) chain (`BlockingFilter →
/// RichTargetValidation → Canonicalize → EnrichmentDispatch → Archive →
/// CarbonsMessage → Inbox → Route`) and emits typed
/// [`waddle_xmpp::protocol::OutboundEvent`]s; the interpreter
/// ([`crate::server::routes::interpret::interpret`]) executes the I/O
/// side effects (route to peer, persist to MAM, project inbox, fan
/// XEP-0280 carbons).
///
/// MUC `<message type='groupchat'>` traffic flows through the same
/// adapter: the dispatcher emits
/// [`waddle_xmpp::protocol::OutboundEvent::DispatchToRoom`] which the
/// interpreter bridges to the legacy
/// [`deliver_groupchat_via_room_actor`] helper until #229 PR17 lands the
/// dedicated room handler chain.
pub async fn handle_message(
    incoming: xmpp_parsers::message::Message,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    state_machine: Option<&mut XmppStateMachine>,
    authenticated_session: Option<&Session>,
) -> Vec<String> {
    if phase.bound_jid().is_none() {
        warn!("Message received without authenticated session");
        return vec![];
    };
    let Some(sm) = state_machine else {
        warn!(
            "Message received before per-connection state machine was initialized; \
             dropping. This indicates a stanza arrived before bind completed."
        );
        return vec![];
    };

    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(incoming),
    ))));
    let deps = build_interpret_deps(state, authenticated_session);
    let (frames, _close) = drive_interpret_loop(events, sm, &deps).await;
    frames
}

/// Deliver a groupchat `<message type='groupchat'>` to a local MUC
/// room: managed-room owner check, `BuildGroupchatBroadcast`, rich-target
/// validation, retraction tombstones, MAM archive write, inbox
/// projection, and per-occupant fan-out via the connection registry.
///
/// Shared between the legacy `handle_message` Groupchat branch and the
/// sans-I/O dispatcher's [`OutboundEvent::DispatchToRoom`] interpreter
/// arm, so MUC fan-out semantics stay bit-for-bit identical regardless
/// of which routing path triggered delivery. PR17 in #229 will retire
/// this helper in favour of a dedicated room handler chain (Q7
/// option C); until then both call sites share one implementation here.
///
/// Returns the wire frames the caller should write back to the sender —
/// today this is the sender's own echo (or a stanza-level error reply
/// when validation fails).
///
/// [`OutboundEvent::DispatchToRoom`]: waddle_xmpp::protocol::OutboundEvent::DispatchToRoom
pub(crate) async fn deliver_groupchat_via_room_actor(
    state: &WebSocketState,
    room_jid: BareJid,
    sender_jid: FullJid,
    incoming: xmpp_parsers::message::Message,
    authenticated_session: &Option<Session>,
) -> Vec<String> {
    debug!(room = %room_jid, sender = %sender_jid, "Groupchat message");

    if waddle_xmpp::parse_managed_room_jid(&room_jid).as_deref() == Some("announcements")
        && !session_is_server_owner(state, authenticated_session).await
    {
        return vec![stanza_to_xml(&Stanza::Message(forbidden_message_error(
            &incoming,
            &room_jid,
            &sender_jid,
        )))];
    }

    let Some(room_actor) = get_room_actor(state, &room_jid).await else {
        warn!(room = %room_jid, "Message to non-existent room");
        return vec![];
    };

    // Build a prototype message, enrich once, then ask the room actor to fan it out.
    let mut prototype = incoming.clone();
    prototype.id = prototype
        .id
        .clone()
        .or_else(|| Some(uuid::Uuid::new_v4().to_string()));
    prototype.type_ = XmppMessageType::Groupchat;
    remove_stanza_ids_by(&mut prototype, &room_jid.to_string());

    // Enrich links with extension-provided XML elements (fail-open).
    let _embeds_added = state
        .deps
        .protocol
        .extension_manager
        .enrich_message(&mut prototype)
        .await;

    let broadcast = match room_actor
        .ask(BuildGroupchatBroadcast {
            sender_jid: sender_jid.clone(),
            message: prototype.clone(),
        })
        .await
    {
        Ok(broadcast) => broadcast,
        // The room actor's typed `BuildGroupchatBroadcast` errors map
        // 1:1 to wire stanza errors (Codex P2 + Qodo bug #3 review on
        // PR #277): only genuine non-occupant senders see XEP-0045
        // §7.4 `<not-acceptable/>`; visitors in moderated rooms see
        // §7.5 `<forbidden/>`; everything else (broadcast prep
        // failures, unrelated `RoomActorError` variants) is a
        // server-side fault and must surface as
        // `<internal-server-error/>` so clients don't get a
        // misleading "you're not in the room" reply during a
        // transient issue. Transport-level kameo errors (mailbox
        // closed, etc.) also fall to internal-server-error.
        Err(kameo::error::SendError::HandlerError(RoomActorError::SenderNotOccupant(_))) => {
            warn!(
                sender = %sender_jid,
                room = %room_jid,
                "XEP-0045 §7.4: sender is not an occupant of the room"
            );
            return vec![stanza_to_xml(&Stanza::Message(not_occupant_message_error(
                &incoming,
                &room_jid,
                &sender_jid,
            )))];
        }
        Err(kameo::error::SendError::HandlerError(RoomActorError::VisitorMayNotSpeak(_))) => {
            warn!(
                sender = %sender_jid,
                room = %room_jid,
                "XEP-0045 §7.5: visitor may not speak in moderated room"
            );
            return vec![stanza_to_xml(&Stanza::Message(
                visitor_may_not_speak_message_error(&incoming, &room_jid, &sender_jid),
            ))];
        }
        Err(error) => {
            warn!(
                sender = %sender_jid,
                room = %room_jid,
                error = ?error,
                "Groupchat broadcast preparation failed; replying with internal-server-error"
            );
            return vec![stanza_to_xml(&Stanza::Message(
                internal_server_error_message(&incoming, &room_jid, &sender_jid),
            ))];
        }
    };
    let sender_nick = broadcast.sender_nick;
    let mut local_messages = broadcast.messages;
    let occupant_bare_jids = broadcast.occupant_bare_jids;
    let sender_nickname_generation = broadcast.sender_nickname_generation;

    let from_room_jid = format!("{}/{}", room_jid, sender_nick);
    if let Ok(from_jid) = from_room_jid.parse::<FullJid>() {
        prototype.from = Some(jid::Jid::from(from_jid));
    } else {
        prototype.from = Some(jid::Jid::from(sender_jid.clone()));
    }
    prototype.to = None;

    if let Err(error) =
        validate_rich_message_targets(state, &room_jid, &prototype, true, Some(&room_actor)).await
    {
        return vec![stanza_to_xml(&Stanza::Message(error_message(
            &incoming,
            &jid::Jid::from(room_jid.clone()),
            &jid::Jid::from(sender_jid.clone()),
            error,
        )))];
    }

    // XEP-0424 §"prevent further distribution… by replacing the
    // original message with a tombstone": after authorization
    // passes, mutate the room archive's original row in place.
    if let Some(RetractionKind::Request(retraction)) = extract_retraction_from_message(&prototype) {
        apply_retraction_tombstones(
            state,
            std::slice::from_ref(&room_jid),
            &prototype,
            &retraction.retracts_id,
            Utc::now(),
            true,
        )
        .await;
    }

    // Archive body-bearing and rich protocol room messages in XMPP MAM storage.
    let archive_id =
        archive_groupchat_message(state, &room_jid, &mut prototype, sender_nickname_generation)
            .await;

    if should_project_message(&prototype) {
        let timestamp = Utc::now().timestamp();
        let sender_bare = sender_jid.to_bare();
        let entry = groupchat_entry(room_jid.clone(), &prototype, timestamp);

        if let Err(error) = state
            .deps
            .protocol
            .inbox_storage
            .upsert(&sender_bare, entry.clone(), false)
            .await
        {
            warn!(jid = %sender_bare, room = %room_jid, error = %error, "Failed to update sender inbox for groupchat");
        }

        // Thread-level inbox projection: if the message carries a <thread/>,
        // upsert a thread-scoped entry alongside the channel-level one.
        let thread_entry =
            prototype.thread.as_ref().map(|thread| {
                // Resolve thread title from Waddle thread metadata or first message preview.
                let forum_title = waddle_xmpp::xep::xep0508::extract_forum_action(&prototype)
                    .and_then(|action| match action {
                        waddle_xmpp::xep::xep0508::ForumAction::CreateThread(tc) => Some(tc.title),
                        _ => None,
                    });
                let title = forum_title.or_else(|| preview_text(&prototype));
                let author_nick = prototype
                    .from
                    .as_ref()
                    .and_then(|jid| jid.resource().map(|r| r.to_string()));
                groupchat_thread_entry(
                    room_jid.clone(),
                    &prototype,
                    timestamp,
                    &thread.0,
                    title.as_deref(),
                    author_nick.as_deref(),
                )
            });

        let mut projected_bares = std::collections::HashSet::new();
        for occupant_bare in occupant_bare_jids
            .iter()
            .filter_map(|jid| jid.parse::<BareJid>().ok())
            .filter(|jid| projected_bares.insert(jid.clone()))
        {
            match state
                .deps
                .protocol
                .inbox_storage
                .upsert(&occupant_bare, entry.clone(), true)
                .await
            {
                Ok(updated) => push_inbox_update(state, &occupant_bare, &updated).await,
                Err(error) => {
                    warn!(jid = %occupant_bare, room = %room_jid, error = %error, "Failed to update occupant inbox for groupchat");
                }
            }

            // Push thread-level entry too
            if let Some(ref thread_entry) = thread_entry {
                match state
                    .deps
                    .protocol
                    .inbox_storage
                    .upsert(&occupant_bare, thread_entry.clone(), true)
                    .await
                {
                    Ok(updated) => push_inbox_update(state, &occupant_bare, &updated).await,
                    Err(error) => {
                        warn!(jid = %occupant_bare, room = %room_jid, error = %error, "Failed to update occupant thread inbox");
                    }
                }
            }
        }

        // Upsert thread entry for sender too (without incrementing unread)
        if let Some(ref thread_entry) = thread_entry {
            if let Err(error) = state
                .deps
                .protocol
                .inbox_storage
                .upsert(&sender_bare, thread_entry.clone(), false)
                .await
            {
                warn!(jid = %sender_bare, room = %room_jid, error = %error, "Failed to update sender thread inbox");
            }
        }
    }

    // Send to all occupants. Groupchat broadcasts are fire-and-forget:
    // message bodies are already archived to MAM, so any occupant the
    // server can't reach right now (backpressured or stale) will pick up
    // the message on their next MAM catch-up. Blocking here is what
    // caused join cascades under zombie load.
    //
    // Accounting invariant for the broadcast log below:
    //   `intended = delivered + dropped_full + dropped_closed + not_connected`
    // The sender is always one of `occupants` in a groupchat send but is
    // reached via the direct echo response (not `try_send_to`), so the
    // echo path counts as one `delivered` to keep the invariant true.
    let mut echo_response = None;
    let mut delivered = 0u32;
    let mut dropped_full = 0u32;
    let mut dropped_closed = 0u32;
    let mut not_connected = 0u32;
    let intended = local_messages.len();
    // XEP-0421: stamp the sender's stable occupant-id on every
    // reflected copy. Closes the gap PR16 documented (legacy fan-out
    // path stamped occupant-id only on presence joins/leaves, never
    // on outgoing groupchat reflections). The id is server-derived
    // from `(sender_bare, room_bare)` via HMAC-SHA256 keyed by the
    // process-wide `OCCUPANT_ID_SECRET` constant — same user across
    // nicks / sessions yields the same id; can't be spoofed by
    // clients. The constant is shared with `presence.rs` so MUC
    // presence and outgoing groupchat reflections produce matching
    // ids; a future change can thread a per-deployment configured
    // secret through `WebSocketState` if rotation / per-tenant
    // isolation becomes a requirement (Copilot review on PR #277).
    let sender_occupant_id = waddle_xmpp::xep::xep0421::generate_occupant_id(
        &sender_jid.to_bare(),
        &room_jid,
        waddle_xmpp::muc::presence::OCCUPANT_ID_SECRET,
    );
    for mut outbound in local_messages.drain(..) {
        if let Some(ref archive_id) = archive_id {
            add_mam_stanza_id(&mut outbound.message, archive_id, &room_jid.to_string());
        }
        waddle_xmpp::xep::xep0421::set_occupant_id_on_message(
            &mut outbound.message,
            &sender_occupant_id,
        );

        if outbound.to == sender_jid {
            // Echo back to sender — serialize the enriched prototype
            echo_response = Some(stanza_to_xml(&Stanza::Message(outbound.message)));
            delivered += 1;
        } else {
            let stanza = Stanza::Message(outbound.message);
            match state
                .deps
                .protocol
                .connection_registry
                .try_send_to(&outbound.to, stanza.clone())
            {
                BroadcastOutcome::Delivered => delivered += 1,
                BroadcastOutcome::DroppedFull => dropped_full += 1,
                BroadcastOutcome::DroppedClosed => match state
                    .deps
                    .protocol
                    .sm_session_registry
                    .record_stanza_for_detached_bound_resource(&outbound.to, &stanza)
                    .await
                {
                    Ok(true) => delivered += 1,
                    Ok(false) => dropped_closed += 1,
                    Err(error) => {
                        warn!(
                            jid = %outbound.to,
                            error = %error,
                            "Failed to record groupchat stanza for detached resource after closed live send"
                        );
                        dropped_closed += 1;
                    }
                },
                BroadcastOutcome::NotConnected => {
                    match state
                        .deps
                        .protocol
                        .sm_session_registry
                        .record_stanza_for_detached_bound_resource(&outbound.to, &stanza)
                        .await
                    {
                        Ok(true) => delivered += 1,
                        Ok(false) => not_connected += 1,
                        Err(error) => {
                            warn!(
                                jid = %outbound.to,
                                error = %error,
                                "Failed to record groupchat stanza for detached resource"
                            );
                            not_connected += 1;
                        }
                    }
                }
            }
        }
    }

    debug_assert_eq!(
        intended as u32,
        delivered + dropped_full + dropped_closed + not_connected,
        "broadcast accounting must cover every occupant exactly once"
    );

    info!(
        room = %room_jid,
        sender = %sender_nick,
        intended,
        delivered,
        dropped_full,
        dropped_closed,
        not_connected,
        "Groupchat message broadcast"
    );

    // Return the echo to the sender
    echo_response.into_iter().collect()
}

async fn session_is_server_owner(state: &WebSocketState, session: &Option<Session>) -> bool {
    let Some(session) = session.as_ref() else {
        return false;
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
            subject: Subject::user(&session.user_id),
            permission: Permission::Owner,
        })
        .await
        .is_ok_and(|response| response.allowed)
}

/// Build the XEP-0045 §7.4 typed `<not-acceptable type='cancel'/>` reply
/// for a non-occupant sender. Mirrors the reply
/// `protocol::room::OccupancyValidationHandler` emits — the chain runs
/// against an `OccupantSnapshot` list and halts with this exact shape
/// when `sender_snapshot()` returns `None` (sender is not in the room).
fn not_occupant_message_error(
    incoming: &xmpp_parsers::message::Message,
    room_jid: &BareJid,
    sender_jid: &FullJid,
) -> xmpp_parsers::message::Message {
    error_message(
        incoming,
        &jid::Jid::from(room_jid.clone()),
        &jid::Jid::from(sender_jid.clone()),
        StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::NotAcceptable,
            "en",
            "Only room occupants may send messages to this room.",
        ),
    )
}

fn forbidden_message_error(
    incoming: &xmpp_parsers::message::Message,
    room_jid: &BareJid,
    sender_jid: &FullJid,
) -> xmpp_parsers::message::Message {
    error_message(
        incoming,
        &jid::Jid::from(room_jid.clone()),
        &jid::Jid::from(sender_jid.clone()),
        StanzaError::new(
            ErrorType::Auth,
            DefinedCondition::Forbidden,
            "en",
            "Sender is not permitted to address this resource.",
        ),
    )
}

/// XEP-0045 §7.5 typed reply for a visitor in a moderated room.
/// The sender IS an occupant but their role is `visitor` and the
/// room's `moderated` flag forbids visitors from sending messages
/// to the room. Maps `RoomActorError::VisitorMayNotSpeak` to the
/// wire reply `<error type='auth'><forbidden/></error>`.
fn visitor_may_not_speak_message_error(
    incoming: &xmpp_parsers::message::Message,
    room_jid: &BareJid,
    sender_jid: &FullJid,
) -> xmpp_parsers::message::Message {
    error_message(
        incoming,
        &jid::Jid::from(room_jid.clone()),
        &jid::Jid::from(sender_jid.clone()),
        StanzaError::new(
            ErrorType::Auth,
            DefinedCondition::Forbidden,
            "en",
            "Visitors may not send messages to this moderated room.",
        ),
    )
}

/// Typed reply for transient server-side broadcast failures (room
/// actor mailbox closed, internal broadcast preparation error, etc.).
/// Maps to the wire reply
/// `<error type='wait'><internal-server-error/></error>` per
/// RFC 6120 §8.3.2's guidance for transient server faults — the
/// client may retry. Mirrors `internal_server_error_for_lookup` and
/// other repo-wide internal-server-error sites (Copilot review on
/// PR #277). Distinct from `<not-acceptable/>` so clients can tell
/// "your message is malformed" from "the server hiccuped".
fn internal_server_error_message(
    incoming: &xmpp_parsers::message::Message,
    room_jid: &BareJid,
    sender_jid: &FullJid,
) -> xmpp_parsers::message::Message {
    error_message(
        incoming,
        &jid::Jid::from(room_jid.clone()),
        &jid::Jid::from(sender_jid.clone()),
        StanzaError::new(
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "en",
            "Internal server error while delivering groupchat message.",
        ),
    )
}

fn error_message(
    incoming: &xmpp_parsers::message::Message,
    from: &jid::Jid,
    to: &jid::Jid,
    error: StanzaError,
) -> xmpp_parsers::message::Message {
    let mut reply = incoming.clone();
    reply.type_ = XmppMessageType::Error;
    reply.from = Some(from.clone());
    reply.to = Some(to.clone());
    reply.payloads.push(Element::from(error));
    reply
}

async fn validate_rich_message_targets(
    state: &WebSocketState,
    archive_jid: &BareJid,
    message: &xmpp_parsers::message::Message,
    groupchat: bool,
    room_actor: Option<&ActorRef<RoomActor>>,
) -> Result<(), StanzaError> {
    let Some(sender) = message.from.as_ref() else {
        return Ok(());
    };
    if has_malformed_rich_payload(message) {
        return Err(bad_request_error(
            "Rich-message payload is missing required identifier.",
        ));
    }

    if let Some(correction) = extract_correction_from_message(message) {
        let original = match lookup_correction_target_message(
            state,
            archive_jid,
            &correction.replaces_id,
            groupchat,
        )
        .await
        {
            Ok(Some(original)) => original,
            Ok(None) => return Err(item_not_found_error("Correction target not found.")),
            Err(_) => return Err(internal_server_error_for_lookup()),
        };
        if !same_rich_sender(sender, &original.from, groupchat) {
            return Err(forbidden_error(
                "Only the original sender may correct a message.",
            ));
        }
        if groupchat {
            verify_muc_occupancy_generation(sender, &original, room_actor).await?;
        }
    }

    if let Some(RetractionKind::Request(retraction)) = extract_retraction_from_message(message) {
        let original = match lookup_retraction_target_message(
            state,
            archive_jid,
            &retraction.retracts_id,
            groupchat,
        )
        .await
        {
            Ok(Some(original)) => original,
            Ok(None) => return Err(item_not_found_error("Retraction target not found.")),
            Err(_) => return Err(internal_server_error_for_lookup()),
        };
        if !same_rich_sender(sender, &original.from, groupchat) {
            return Err(forbidden_error(
                "Only the original sender may retract a message.",
            ));
        }
    }

    // Reactions (XEP-0444) and replies (XEP-0461) intentionally skip
    // target-existence validation. Both specs are silent on
    // server-side target verification — XEP-0444 §"It is up to
    // receiving entities" and XEP-0461 placing no obligation on the
    // server — and rejecting on missing target would break legitimate
    // cases the server cannot disambiguate: cross-server messages
    // (s2s), replies to messages before archive retention, reactions
    // to messages cached by the client but not by the server. The
    // well-formedness check above (`has_malformed_rich_payload`)
    // already rejects malformed payloads with `<bad-request/>`.

    Ok(())
}

/// Replace the retraction-target row in each given archive with a
/// XEP-0424 tombstone. Called after `validate_rich_message_targets`
/// has confirmed sender authorization. Failures are logged but do not
/// propagate — the retraction message is still archived and broadcast,
/// matching the spec's "best effort" framing for tombstones (the SHOULD
/// is on archive distribution, not on the retraction itself).
async fn apply_retraction_tombstones(
    state: &WebSocketState,
    archive_jids: &[BareJid],
    retraction_message: &xmpp_parsers::message::Message,
    target_id: &str,
    stamp: chrono::DateTime<chrono::Utc>,
    groupchat: bool,
) {
    let retraction_id = retraction_message
        .id
        .clone()
        .and_then(waddle_xmpp::mam::RichMessageId::new);

    for archive_jid in archive_jids {
        let original = match lookup_retraction_target_message(
            state,
            archive_jid,
            target_id,
            groupchat,
        )
        .await
        {
            Ok(Some(original)) => original,
            Ok(None) => {
                debug!(
                    archive = %archive_jid,
                    target = %target_id,
                    "Retraction target not found in this archive; tombstone skipped"
                );
                continue;
            }
            Err(error) => {
                warn!(
                    archive = %archive_jid,
                    target = %target_id,
                    error = %error,
                    "Failed to look up retraction target for tombstone"
                );
                continue;
            }
        };

        let tombstone = waddle_xmpp::mam::ArchivedTombstone {
            retraction_id: retraction_id.clone(),
            stamp,
            moderation: None,
        };

        match state
            .deps
            .protocol
            .mam_storage
            .replace_with_tombstone(&original.id, tombstone)
            .await
        {
            Ok(true) => {
                debug!(archive = %archive_jid, original_id = %original.id, "Replaced with tombstone")
            }
            Ok(false) => warn!(
                archive = %archive_jid,
                original_id = %original.id,
                "Tombstone replacement found no row to update"
            ),
            Err(error) => warn!(
                archive = %archive_jid,
                original_id = %original.id,
                error = %error,
                "Tombstone replacement failed"
            ),
        }
    }
}

/// Enforce XEP-0308 §3 SHOULD #2: a full-JID that left the room and
/// rejoined under the same nickname MUST NOT be allowed to correct
/// messages from the previous occupancy. Implemented by comparing the
/// per-nickname occupancy generation captured in the archived row
/// (set at archive-write time from
/// [`super::super::super::super::muc::room_actor::GroupchatBroadcastResult::sender_nickname_generation`])
/// against the room's current generation for the same nickname.
async fn verify_muc_occupancy_generation(
    sender: &jid::Jid,
    original: &waddle_xmpp::mam::ArchivedMessage,
    room_actor: Option<&ActorRef<RoomActor>>,
) -> Result<(), StanzaError> {
    let Some(actor) = room_actor else {
        // No room actor available — the correction handler caller did
        // not supply one. This is a wiring bug; refuse rather than
        // silently allow.
        return Err(forbidden_error(
            "Room state unavailable for occupancy continuity check.",
        ));
    };

    let Some(nick) = sender.resource().map(|r| r.to_string()) else {
        // Sender JID has no nickname (resource) — should not happen for
        // a server-stamped MUC reflection, but bail safely.
        return Err(forbidden_error(
            "Correction sender has no MUC nickname for occupancy check.",
        ));
    };

    let Some(archived_generation) = original.nickname_generation else {
        // The original archive row predates occupancy-generation
        // tracking (or was written by a non-MUC path). Per XEP-0308
        // §3, we cannot prove continuity, so refuse.
        return Err(forbidden_error(
            "Original message predates occupancy tracking; correction window has closed.",
        ));
    };

    let current_generation = match actor
        .ask(GetNicknameGeneration { nick: nick.clone() })
        .await
    {
        Ok(value) => value,
        Err(_) => return Err(internal_server_error_for_lookup()),
    };

    if current_generation != archived_generation {
        return Err(forbidden_error(
            "Occupancy generation has advanced; correction is no longer permitted across the leave/rejoin boundary.",
        ));
    }

    Ok(())
}

fn bad_request_error(text: &str) -> StanzaError {
    StanzaError::new(ErrorType::Modify, DefinedCondition::BadRequest, "en", text)
}

fn item_not_found_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        "en",
        text,
    )
}

fn forbidden_error(text: &str) -> StanzaError {
    StanzaError::new(ErrorType::Auth, DefinedCondition::Forbidden, "en", text)
}

fn internal_server_error_for_lookup() -> StanzaError {
    StanzaError::new(
        ErrorType::Wait,
        DefinedCondition::InternalServerError,
        "en",
        "Archive lookup failed while validating rich-message target.",
    )
}

async fn lookup_correction_target_message(
    state: &WebSocketState,
    archive_jid: &BareJid,
    target_id: &str,
    _groupchat: bool,
) -> Result<Option<ArchivedMessage>, waddle_xmpp::mam::MamStorageError> {
    state
        .deps
        .protocol
        .mam_storage
        .get_message_by_message_id(&archive_jid.to_string(), target_id)
        .await
}

async fn lookup_retraction_target_message(
    state: &WebSocketState,
    archive_jid: &BareJid,
    target_id: &str,
    groupchat: bool,
) -> Result<Option<ArchivedMessage>, waddle_xmpp::mam::MamStorageError> {
    if groupchat {
        return lookup_rich_target_message(state, archive_jid, target_id, true).await;
    }

    state
        .deps
        .protocol
        .mam_storage
        .get_message_by_message_id(&archive_jid.to_string(), target_id)
        .await
}

async fn lookup_rich_target_message(
    state: &WebSocketState,
    archive_jid: &BareJid,
    target_id: &str,
    groupchat: bool,
) -> Result<Option<ArchivedMessage>, waddle_xmpp::mam::MamStorageError> {
    if groupchat {
        let archive_str = archive_jid.to_string();
        return state
            .deps
            .protocol
            .mam_storage
            .get_message(target_id)
            .await
            .map(|message| message.filter(|message| message.to == archive_str));
    }

    state
        .deps
        .protocol
        .mam_storage
        .get_message_by_stanza_id(&archive_jid.to_string(), target_id)
        .await
}

fn same_rich_sender(sender: &jid::Jid, original_from: &str, groupchat: bool) -> bool {
    if groupchat {
        return sender.to_string() == original_from;
    }
    original_from
        .parse::<jid::Jid>()
        .is_ok_and(|original| original.to_bare() == sender.to_bare())
}

fn has_malformed_rich_payload(message: &xmpp_parsers::message::Message) -> bool {
    message.payloads.iter().any(|payload| {
        // XEP-0308 / 0424 / 0444 / 0461: each requires a non-empty 'id'
        // attribute on its top-level element.
        (payload.ns() == NS_MESSAGE_CORRECT
            && payload.name() == "replace"
            && payload.attr("id").is_none_or(str::is_empty))
            || (payload.ns() == NS_MESSAGE_RETRACT
                && payload.name() == "retract"
                && payload.attr("id").is_none_or(str::is_empty))
            || (payload.ns() == NS_REACTIONS
                && payload.name() == "reactions"
                && payload.attr("id").is_none_or(str::is_empty))
            || (payload.ns() == NS_REPLY
                && payload.name() == "reply"
                && payload.attr("id").is_none_or(str::is_empty))
            // XEP-0372: '<reference/>' MUST contain 'type' and 'uri'.
            || (payload.ns() == NS_REFERENCE
                && payload.name() == "reference"
                && (payload.attr("type").is_none_or(str::is_empty)
                    || payload.attr("uri").is_none_or(str::is_empty)))
            // XEP-0513: a '<mention/>' MUST carry at least one of
            // 'jid', 'occupantid', or 'mentions' so the receiver can
            // identify the target. Pure decorative mentions are not
            // useful and are likely client bugs.
            || (payload.ns() == NS_EXPLICIT_MENTIONS
                && payload.name() == "mention"
                && payload.attr("jid").is_none_or(str::is_empty)
                && payload.attr("occupantid").is_none_or(str::is_empty)
                && payload.attr("mentions").is_none_or(str::is_empty))
    })
}

/// Push an inbox update headline to all connected sessions of a user.
async fn push_inbox_update(state: &WebSocketState, user: &BareJid, entry: &InboxEntry) {
    let resources = state
        .deps
        .protocol
        .connection_registry
        .get_resources_for_user(user);
    for resource_jid in resources {
        let msg = build_inbox_push(jid::Jid::from(resource_jid.clone()), entry);
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource_jid, Stanza::Message(msg))
            .await;
    }
}

/// Returns true if this groupchat message should be written to the MAM archive.
///
/// Body/subject-bearing messages are always archived; body-less protocol
/// events (reactions, retractions, moderation, file-shares, stickers) are
/// archived too so that MAM replay faithfully reproduces the room timeline.
/// Error messages and messages carrying a `<no-store/>` hint are excluded.
fn should_archive_groupchat_message(msg: &xmpp_parsers::message::Message) -> bool {
    if matches!(msg.type_, XmppMessageType::Error) || should_skip_storage(msg) {
        return false;
    }

    if !msg.bodies.is_empty() || !msg.subjects.is_empty() {
        return true;
    }

    is_reaction_message(msg)
        || is_retraction_message(msg)
        || is_moderation_request_message(msg)
        || is_moderation_result_message(msg)
        || has_file_sharing(msg)
        || is_sticker_message(msg)
}

fn serialize_groupchat_stanza_xml(message: &xmpp_parsers::message::Message) -> Option<String> {
    let mut msg = message.clone();
    msg.to = None;
    match message_to_string(&msg) {
        Ok(xml) => Some(xml),
        Err(error) => {
            warn!(error = %error, "Failed to serialize groupchat stanza XML for MAM archive");
            None
        }
    }
}

async fn archive_groupchat_message(
    state: &WebSocketState,
    room_jid: &BareJid,
    message: &mut xmpp_parsers::message::Message,
    sender_nickname_generation: u64,
) -> Option<String> {
    if !should_archive_groupchat_message(message) {
        return None;
    }

    let archive_id = uuid::Uuid::now_v7().to_string();
    add_mam_stanza_id(message, archive_id.as_str(), &room_jid.to_string());

    let body = prototype_body(message)
        .map(|value| value.trim().to_string())
        .unwrap_or_default();

    let (reply_to_id, reply_to_jid) = extract_reply_reference(message);
    let origin_id = extract_origin_id(message);
    let rich = rich_archive_payload(message);

    let stanza_xml = serialize_groupchat_stanza_xml(message);

    let archived = ArchivedMessage {
        id: archive_id,
        timestamp: Utc::now(),
        from: message
            .from
            .as_ref()
            .map(|jid| jid.to_string())
            .unwrap_or_default(),
        to: room_jid.to_string(),
        body,
        stanza_id: message.id.clone(),
        thread_id: message.thread.as_ref().map(|thread| thread.0.clone()),
        reply_to_id,
        reply_to_jid,
        origin_id,
        message_type: mam_message_type(&message.type_),
        stanza_xml,
        rich,
        nickname_generation: Some(sender_nickname_generation),
    };

    let archive_jid = room_jid.to_string();
    match state
        .deps
        .protocol
        .mam_storage
        .store_message(archive_jid.as_str(), &archived)
        .await
    {
        Ok(archive_id) => Some(archive_id),
        Err(err) => {
            warn!(
                room = %room_jid,
                error = %err,
                "Failed to archive groupchat message to MAM"
            );
            None
        }
    }
}

fn mam_message_type(message_type: &XmppMessageType) -> String {
    match message_type {
        XmppMessageType::Chat => "chat".to_string(),
        XmppMessageType::Error => "error".to_string(),
        XmppMessageType::Groupchat => "groupchat".to_string(),
        XmppMessageType::Headline => "headline".to_string(),
        XmppMessageType::Normal => "normal".to_string(),
    }
}

fn extract_reply_reference(
    message: &xmpp_parsers::message::Message,
) -> (Option<String>, Option<String>) {
    let Some(reply) = message
        .payloads
        .iter()
        .find(|payload| payload.name() == "reply" && payload.ns() == NS_REPLY)
    else {
        return (None, None);
    };

    (
        reply.attr("id").map(ToOwned::to_owned),
        reply.attr("to").map(ToOwned::to_owned),
    )
}

fn rich_archive_payload(message: &xmpp_parsers::message::Message) -> Option<ArchivedRichMessage> {
    let payload = extract_correction_from_message(message)
        .and_then(|correction| {
            RichMessageId::new(correction.replaces_id)
                .map(|replaces_id| ArchivedRichPayload::Correction { replaces_id })
        })
        .or_else(|| {
            extract_retraction_from_message(message).and_then(|kind| match kind {
                RetractionKind::Request(retraction) => RichMessageId::new(retraction.retracts_id)
                    .map(|target_id| {
                        ArchivedRichPayload::Retraction(ArchivedRetraction {
                            target_id,
                            stamp: None,
                            retraction_id: message.id.clone().and_then(RichMessageId::new),
                        })
                    }),
                RetractionKind::Tombstone(retracted) => message.id.clone().and_then(|id| {
                    RichMessageId::new(id).map(|target_id| {
                        ArchivedRichPayload::Retraction(ArchivedRetraction {
                            target_id,
                            stamp: chrono::DateTime::parse_from_rfc3339(&retracted.stamp)
                                .ok()
                                .map(|stamp| stamp.with_timezone(&Utc)),
                            retraction_id: None,
                        })
                    })
                }),
            })
        })
        .or_else(|| {
            extract_reactions_from_message(message).and_then(|reactions| {
                RichMessageId::new(reactions.message_id).map(|target_id| {
                    ArchivedRichPayload::Reactions(ArchivedReactionSet {
                        target_id,
                        emojis: reactions
                            .emojis
                            .into_iter()
                            .filter_map(RichText::new)
                            .collect(),
                    })
                })
            })
        });

    let reply = parse_reply_from_message(message).and_then(|reply| {
        RichMessageId::new(reply.id).map(|id| ArchivedReply {
            id,
            to: reply.to.and_then(|to| to.parse().ok()),
        })
    });

    let references = extract_references_from_message(message)
        .into_iter()
        .filter_map(|reference| {
            let ref_type = RichText::new(reference.ref_type.as_str())?;
            Some(ArchivedReference {
                ref_type,
                begin: reference.begin.and_then(|value| value.try_into().ok()),
                end: reference.end.and_then(|value| value.try_into().ok()),
                uri: reference.uri.and_then(RichText::new),
                anchor: reference.anchor.and_then(RichText::new),
            })
        })
        .collect::<Vec<_>>();

    let mentions = extract_explicit_mentions(message)
        .map(|mentions| {
            mentions
                .mentions
                .into_iter()
                .map(|mention| ArchivedMention {
                    begin: mention.begin,
                    end: mention.end,
                    jid: mention.jid,
                    occupant_id: mention.occupant_id.and_then(RichText::new),
                    mentions: mention.mentions.and_then(RichText::new),
                    uri: mention.uri.and_then(RichText::new),
                    active: mention.active,
                    noping: mention.noping,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if payload.is_none() && reply.is_none() && references.is_empty() && mentions.is_empty() {
        None
    } else {
        Some(ArchivedRichMessage {
            payload,
            reply,
            references,
            mentions,
        })
    }
}

fn extract_origin_id(message: &xmpp_parsers::message::Message) -> Option<String> {
    message
        .payloads
        .iter()
        .find(|payload| payload.name() == "origin-id" && payload.ns() == STANZA_ID_NS)
        .and_then(|origin| origin.attr("id").map(ToOwned::to_owned))
}

fn prototype_body(message: &xmpp_parsers::message::Message) -> Option<String> {
    message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .map(|body| body.0.clone())
}
