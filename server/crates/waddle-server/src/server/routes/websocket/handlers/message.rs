use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use jid::{BareJid, FullJid};
use tracing::{debug, info, warn};
use waddle_xmpp::{
    carbons::{build_received_carbon, build_sent_carbon, should_copy_message, CARBONS_NS},
    inbox::{
        runtime::{
            direct_message_entry, groupchat_entry, groupchat_thread_entry, preview_text,
            should_project_message,
        },
        InboxEntry,
    },
    mam::{
        add_stanza_id as add_mam_stanza_id, ArchivedMention, ArchivedMessage, ArchivedReactionSet,
        ArchivedReference, ArchivedReply, ArchivedRetraction, ArchivedRichMessage,
        ArchivedRichPayload, RichMessageId, RichText, STANZA_ID_NS,
    },
    muc::room_actor::{BuildGroupchatBroadcast, GetNicknameGeneration, GetSnapshot, RoomActor},
    parser::message_to_string,
    registry::{BroadcastOutcome, SendResult},
    xep::xep0430::build_inbox_push,
    xep::xep0482::{
        has_call_invite_payload, try_extract_call_invite_payload, CallInvite, CallInvitePayload,
        JoinMethod,
    },
    xep::{
        extract_correction_from_message, extract_explicit_mentions, extract_reactions_from_message,
        extract_references_from_message, extract_retraction_from_message, has_file_sharing,
        is_moderation_request_message, is_moderation_result_message, is_reaction_message,
        is_retraction_message, is_sticker_message, message_has_direct_invite,
        parse_reply_from_message, remove_stanza_ids_by, should_skip_carbons, should_skip_storage,
        RetractionKind, NS_EXPLICIT_MENTIONS, NS_MESSAGE_CORRECT, NS_MESSAGE_RETRACT, NS_REACTIONS,
        NS_REFERENCE, NS_REPLY,
    },
    Stanza,
};
use xmpp_parsers::message::MessageType as XmppMessageType;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::super::{get_room_actor, stanza_to_xml, WebSocketState};
use crate::auth::Session;
use crate::db::blocking::DatabaseBlockingStorage;
use crate::media::{MediaGatewayError, MediaSessionId, MediaSessionScope};
use crate::permissions::{CheckPermission, Object, ObjectType, Permission, Subject};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use kameo::actor::ActorRef;
use waddle_xmpp::protocol::ConnectionPhase;

pub async fn handle_message(
    mut incoming: xmpp_parsers::message::Message,
    muc_domain: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    authenticated_session: &Option<Session>,
) -> Vec<String> {
    let Some(sender_jid) = phase.bound_jid() else {
        warn!("Message received without authenticated session");
        return vec![];
    };

    // Always stamp the authenticated sender.
    incoming.from = Some(jid::Jid::from(sender_jid.clone()));

    // Handle local MUC groupchat messages. Full-JID groupchat stanzas that are
    // not addressed to the local MUC service fall through to direct routing.
    if incoming.type_ == XmppMessageType::Groupchat
        && incoming
            .to
            .as_ref()
            .is_some_and(|jid| jid.to_bare().domain().as_str() == muc_domain)
    {
        let Some(to_jid) = incoming.to.as_ref() else {
            warn!("Groupchat message without 'to' attribute");
            return vec![];
        };

        // Parse room JID (strip resource if present)
        let room_jid = to_jid.to_bare();

        debug!(room = %room_jid, sender = %sender_jid, "Groupchat message");

        if waddle_xmpp::parse_managed_room_jid(&room_jid).as_deref() == Some("announcements")
            && !session_is_server_owner(state, authenticated_session).await
        {
            return vec![stanza_to_xml(&Stanza::Message(forbidden_message_error(
                &incoming, &room_jid, sender_jid,
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

        let prepared_call_payload = match prepare_groupchat_call_payload(
            state,
            &prototype,
            &room_jid,
            sender_jid,
            authenticated_session,
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return vec![stanza_to_xml(&Stanza::Message(error_message(
                    &incoming,
                    &jid::Jid::from(room_jid.clone()),
                    &jid::Jid::from(sender_jid.clone()),
                    error,
                )))];
            }
        };

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
            Err(error) => {
                warn!(
                    sender = %sender_jid,
                    room = %room_jid,
                    error = ?error,
                    "Sender not permitted to broadcast to MUC room"
                );
                return vec![];
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
            validate_rich_message_targets(state, &room_jid, &prototype, true, Some(&room_actor))
                .await
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
        if let Some(RetractionKind::Request(retraction)) =
            extract_retraction_from_message(&prototype)
        {
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

        let created_call_session_id =
            if matches!(prepared_call_payload, PreparedCallPayload::GatewayInvite) {
                if should_skip_storage(&prototype) {
                    return vec![stanza_to_xml(&Stanza::Message(error_message(
                        &incoming,
                        &jid::Jid::from(room_jid.clone()),
                        &jid::Jid::from(sender_jid.clone()),
                        bad_request_error("MUC call invites require an archived stanza id."),
                    )))];
                }
                match ensure_muc_call_invite_session(state, &mut prototype, &room_jid, sender_jid) {
                    Ok(Some(session_id)) => {
                        for outbound in &mut local_messages {
                            outbound.message.payloads = prototype.payloads.clone();
                        }
                        Some(session_id)
                    }
                    Ok(None) => {
                        return vec![stanza_to_xml(&Stanza::Message(error_message(
                            &incoming,
                            &jid::Jid::from(room_jid.clone()),
                            &jid::Jid::from(sender_jid.clone()),
                            bad_request_error("Call invite did not create a media session."),
                        )))];
                    }
                    Err(error) => {
                        return vec![stanza_to_xml(&Stanza::Message(error_message(
                            &incoming,
                            &jid::Jid::from(room_jid.clone()),
                            &jid::Jid::from(sender_jid.clone()),
                            media_gateway_stanza_error(error),
                        )))];
                    }
                }
            } else {
                None
            };

        // Archive body-bearing and rich protocol room messages in XMPP MAM storage.
        let archive_id =
            archive_groupchat_message(state, &room_jid, &mut prototype, sender_nickname_generation)
                .await;
        match (
            &prepared_call_payload,
            created_call_session_id.as_ref(),
            archive_id.as_ref(),
        ) {
            (PreparedCallPayload::GatewayInvite, Some(session_id), Some(archive_id)) => {
                if let Err(error) = bind_call_invite_reference(
                    state,
                    session_id,
                    muc_call_conversation_key(&room_jid),
                    archive_id,
                ) {
                    state
                        .deps
                        .protocol
                        .media_gateway
                        .discard_session(session_id);
                    return vec![stanza_to_xml(&Stanza::Message(error_message(
                        &incoming,
                        &jid::Jid::from(room_jid.clone()),
                        &jid::Jid::from(sender_jid.clone()),
                        media_gateway_stanza_error(error),
                    )))];
                }
            }
            (PreparedCallPayload::GatewayInvite, Some(session_id), None) => {
                state
                    .deps
                    .protocol
                    .media_gateway
                    .discard_session(session_id);
                return vec![stanza_to_xml(&Stanza::Message(error_message(
                    &incoming,
                    &jid::Jid::from(room_jid.clone()),
                    &jid::Jid::from(sender_jid.clone()),
                    internal_server_error("Call invite could not be archived."),
                )))];
            }
            (PreparedCallPayload::GatewayLifecycle, _, _) => observe_call_lifecycle(
                state,
                &prototype,
                muc_call_conversation_key(&room_jid),
                sender_jid,
            ),
            _ => {}
        }

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
            let thread_entry = prototype.thread.as_ref().map(|thread| {
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
        for mut outbound in local_messages.drain(..) {
            if let Some(ref archive_id) = archive_id {
                add_mam_stanza_id(&mut outbound.message, archive_id, &room_jid.to_string());
            }

            if outbound.to == *sender_jid {
                // Echo back to sender — serialize the enriched prototype
                echo_response = Some(stanza_to_xml(&Stanza::Message(outbound.message)));
                delivered += 1;
            } else {
                let stanza = Stanza::Message(outbound.message);
                match state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&outbound.to, stanza)
                {
                    BroadcastOutcome::Delivered => delivered += 1,
                    BroadcastOutcome::DroppedFull => dropped_full += 1,
                    BroadcastOutcome::DroppedClosed => dropped_closed += 1,
                    BroadcastOutcome::NotConnected => not_connected += 1,
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
        return echo_response.into_iter().collect();
    }

    // Handle direct messages, including default/normal messages and XEP-0249
    // direct MUC invites which are often sent as normal messages.
    let direct_full_jid_message = incoming
        .to
        .as_ref()
        .is_some_and(|to| to.clone().try_into_full().is_ok())
        && matches!(
            incoming.type_,
            XmppMessageType::Groupchat | XmppMessageType::Error
        );
    if matches!(
        incoming.type_,
        XmppMessageType::Chat | XmppMessageType::Normal | XmppMessageType::Headline
    ) || message_has_direct_invite(&incoming)
        || direct_full_jid_message
    {
        if let Some(to_jid) = incoming.to.as_ref() {
            debug!(to = %to_jid, from = %sender_jid, "Direct chat message");

            // Build a prototype message and enrich it with embeds before routing.
            // Enrichment is fail-open: errors are logged but never block delivery.
            let mut prototype = incoming.clone();
            if prototype.id.is_none() {
                prototype.id = extract_origin_id(&prototype)
                    .or_else(|| Some(uuid::Uuid::new_v4().to_string()));
            }
            prototype.from = Some(jid::Jid::from(sender_jid.clone()));
            let blocking =
                DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone());
            match blocking
                .is_blocked(&to_jid.to_bare(), &sender_jid.to_bare())
                .await
            {
                Ok(true) => {
                    info!(sender = %sender_jid, recipient = %to_jid, "Blocked direct message");
                    return vec![];
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(error = %error, sender = %sender_jid, recipient = %to_jid, "Failed to check blocklist before routing direct message");
                    return vec![];
                }
            }

            let prepared_call_payload = match prepare_direct_call_payload(
                state,
                &prototype,
                sender_jid,
                to_jid,
                authenticated_session,
            )
            .await
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    return vec![stanza_to_xml(&Stanza::Message(error_message(
                        &incoming,
                        to_jid,
                        &jid::Jid::from(sender_jid.clone()),
                        error,
                    )))];
                }
            };
            // Enrich links with extension-provided XML elements.
            let _embeds_added = state
                .deps
                .protocol
                .extension_manager
                .enrich_message(&mut prototype)
                .await;

            if let Err(error) =
                validate_rich_message_targets(state, &sender_jid.to_bare(), &prototype, false, None)
                    .await
            {
                return vec![stanza_to_xml(&Stanza::Message(error_message(
                    &incoming,
                    to_jid,
                    &jid::Jid::from(sender_jid.clone()),
                    error,
                )))];
            }

            // XEP-0424 §"prevent further distribution": when a DM is a
            // retraction request, tombstone the original in BOTH
            // archives that hold it (sender's and recipient's
            // personal MAM).
            if let Some(RetractionKind::Request(retraction)) =
                extract_retraction_from_message(&prototype)
            {
                let archives = [sender_jid.to_bare(), to_jid.to_bare()];
                apply_retraction_tombstones(
                    state,
                    &archives,
                    &prototype,
                    &retraction.retracts_id,
                    Utc::now(),
                    false,
                )
                .await;
            }

            let direct_call_binding =
                if matches!(prepared_call_payload, PreparedCallPayload::GatewayInvite) {
                    let Some(invite_id) = direct_call_invite_reference_id(&prototype) else {
                        return vec![stanza_to_xml(&Stanza::Message(error_message(
                            &incoming,
                            to_jid,
                            &jid::Jid::from(sender_jid.clone()),
                            bad_request_error("Call invite is missing a reference id."),
                        )))];
                    };
                    match ensure_direct_call_invite_session(
                        state,
                        &mut prototype,
                        sender_jid,
                        to_jid,
                    ) {
                        Ok(Some(session_id)) => Some((session_id, invite_id)),
                        Ok(None) => {
                            return vec![stanza_to_xml(&Stanza::Message(error_message(
                                &incoming,
                                to_jid,
                                &jid::Jid::from(sender_jid.clone()),
                                bad_request_error("Call invite did not create a media session."),
                            )))];
                        }
                        Err(error) => {
                            return vec![stanza_to_xml(&Stanza::Message(error_message(
                                &incoming,
                                to_jid,
                                &jid::Jid::from(sender_jid.clone()),
                                media_gateway_stanza_error(error),
                            )))];
                        }
                    }
                } else {
                    None
                };

            if let Some((session_id, invite_id)) = direct_call_binding {
                if let Err(error) = bind_call_invite_reference(
                    state,
                    &session_id,
                    direct_call_conversation_key(sender_jid, to_jid),
                    &invite_id,
                ) {
                    state
                        .deps
                        .protocol
                        .media_gateway
                        .discard_session(&session_id);
                    return vec![stanza_to_xml(&Stanza::Message(error_message(
                        &incoming,
                        to_jid,
                        &jid::Jid::from(sender_jid.clone()),
                        media_gateway_stanza_error(error),
                    )))];
                }
            }

            // Archive body-bearing DMs to both sender's and recipient's personal MAM.
            archive_direct_message(state, sender_jid, to_jid, &prototype).await;

            if should_project_message(&prototype) {
                let timestamp = Utc::now().timestamp();
                let sender_bare = sender_jid.to_bare();
                let recipient_bare = to_jid.to_bare();

                if let Err(error) = state
                    .deps
                    .protocol
                    .inbox_storage
                    .upsert(
                        &sender_bare,
                        direct_message_entry(recipient_bare.clone(), &prototype, timestamp),
                        false,
                    )
                    .await
                {
                    warn!(jid = %sender_bare, partner = %recipient_bare, error = %error, "Failed to update sender inbox for direct message");
                }

                if recipient_bare.domain() == sender_bare.domain() {
                    match state
                        .deps
                        .protocol
                        .inbox_storage
                        .upsert(
                            &recipient_bare,
                            direct_message_entry(sender_bare.clone(), &prototype, timestamp),
                            true,
                        )
                        .await
                    {
                        Ok(updated) => push_inbox_update(state, &recipient_bare, &updated).await,
                        Err(error) => {
                            warn!(jid = %recipient_bare, partner = %sender_bare, error = %error, "Failed to update recipient inbox for direct message");
                        }
                    }
                }
            }

            // Route the enriched message
            let delivered_full_jid = if let Ok(to_full_jid) = to_jid.clone().try_into_full() {
                let mut msg = prototype.clone();
                msg.to = Some(jid::Jid::from(to_full_jid.clone()));
                let stanza = Stanza::Message(msg);
                match state
                    .deps
                    .protocol
                    .connection_registry
                    .send_to(&to_full_jid, stanza)
                    .await
                {
                    SendResult::Sent => Some(to_full_jid),
                    SendResult::NotConnected | SendResult::ChannelClosed => None,
                }
            } else {
                let to_bare_jid = to_jid.to_bare();
                let resources = state
                    .deps
                    .protocol
                    .connection_registry
                    .get_resources_for_user(&to_bare_jid);
                for resource_jid in resources {
                    let mut msg = prototype.clone();
                    msg.to = Some(jid::Jid::from(resource_jid.clone()));
                    let stanza = Stanza::Message(msg);
                    let _ = state
                        .deps
                        .protocol
                        .connection_registry
                        .send_to(&resource_jid, stanza)
                        .await;
                }
                None
            };

            let should_carbon = should_send_direct_carbon(&prototype);
            if should_carbon {
                if let Some(ref recipient_full_jid) = delivered_full_jid {
                    send_received_carbons_to_websocket_resources(
                        state,
                        recipient_full_jid,
                        &prototype,
                    )
                    .await;
                }
                send_sent_carbons_to_websocket_resources(state, sender_jid, &prototype).await;
            }
            if matches!(prepared_call_payload, PreparedCallPayload::GatewayLifecycle) {
                observe_call_lifecycle(
                    state,
                    &prototype,
                    direct_call_conversation_key(sender_jid, to_jid),
                    sender_jid,
                );
            }
        } else {
            warn!("Direct chat message without 'to' attribute");
        }
        return vec![];
    }

    debug!(msg_type = ?incoming.type_, "Message stanza received");
    vec![]
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum PreparedCallPayload {
    None,
    GatewayInvite,
    GatewayLifecycle,
    Passthrough,
}

async fn prepare_groupchat_call_payload(
    state: &WebSocketState,
    message: &xmpp_parsers::message::Message,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    authenticated_session: &Option<Session>,
) -> Result<PreparedCallPayload, StanzaError> {
    let Some(payload) = try_extract_call_invite_payload(message)
        .map_err(|_| bad_request_error("Invalid call invite payload."))?
    else {
        return Ok(PreparedCallPayload::None);
    };
    if has_multiple_call_invite_payloads(message) {
        return Err(bad_request_error(
            "Only one call invite payload is permitted.",
        ));
    }

    match payload {
        CallInvitePayload::Invite(invite) => {
            if !is_gateway_call_invite(&invite, &state.deps.service_domains.media) {
                return Ok(PreparedCallPayload::Passthrough);
            }
            if !muc_call_start_authorized(state, room_jid, sender_jid, authenticated_session).await
            {
                return Err(forbidden_error("Sender is not permitted to start calls."));
            }
            Ok(PreparedCallPayload::GatewayInvite)
        }
        _ => {
            match groupchat_call_lifecycle_authorized(
                state,
                room_jid,
                sender_jid,
                authenticated_session,
                &payload,
            )
            .await?
            {
                CallLifecycleAuthorization::Gateway => Ok(PreparedCallPayload::GatewayLifecycle),
                CallLifecycleAuthorization::Passthrough => Ok(PreparedCallPayload::Passthrough),
                CallLifecycleAuthorization::Forbidden => Err(forbidden_error(
                    "Sender is not permitted to update this call.",
                )),
            }
        }
    }
}

async fn prepare_direct_call_payload(
    state: &WebSocketState,
    message: &xmpp_parsers::message::Message,
    sender_jid: &FullJid,
    to_jid: &jid::Jid,
    authenticated_session: &Option<Session>,
) -> Result<PreparedCallPayload, StanzaError> {
    let Some(payload) = try_extract_call_invite_payload(message)
        .map_err(|_| bad_request_error("Invalid call invite payload."))?
    else {
        return Ok(PreparedCallPayload::None);
    };
    if has_multiple_call_invite_payloads(message) {
        return Err(bad_request_error(
            "Only one call invite payload is permitted.",
        ));
    }

    match payload {
        CallInvitePayload::Invite(invite) => {
            if !is_gateway_call_invite(&invite, &state.deps.service_domains.media) {
                return Ok(PreparedCallPayload::Passthrough);
            }
            if !direct_call_start_authorized(state, sender_jid, to_jid, authenticated_session).await
            {
                return Err(forbidden_error(
                    "Sender is not permitted to start direct calls.",
                ));
            }
            Ok(PreparedCallPayload::GatewayInvite)
        }
        _ => match direct_call_lifecycle_authorized(state, sender_jid, to_jid, &payload).await? {
            CallLifecycleAuthorization::Gateway => Ok(PreparedCallPayload::GatewayLifecycle),
            CallLifecycleAuthorization::Passthrough => Ok(PreparedCallPayload::Passthrough),
            CallLifecycleAuthorization::Forbidden => Err(forbidden_error(
                "Sender is not permitted to update this call.",
            )),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallLifecycleAuthorization {
    Gateway,
    Passthrough,
    Forbidden,
}

fn is_gateway_call_invite(invite: &CallInvite, media_domain: &str) -> bool {
    invite
        .methods
        .iter()
        .any(|method| is_waddle_jingle_join_method(method, media_domain))
}

fn is_waddle_jingle_join_method(method: &JoinMethod, media_domain: &str) -> bool {
    matches!(
        method,
        JoinMethod::Jingle {
            sid,
            jid: Some(jid),
        } if gateway_jingle_jid_matches(jid, media_domain, sid.as_str())
    )
}

fn gateway_jingle_jid_matches(jid: &jid::Jid, media_domain: &str, sid: &str) -> bool {
    jid.clone().try_into_full().ok().is_some_and(|full| {
        full.to_bare().as_str() == media_domain && full.resource().to_string() == sid
    })
}

fn has_multiple_call_invite_payloads(message: &xmpp_parsers::message::Message) -> bool {
    message
        .payloads
        .iter()
        .filter(|payload| waddle_xmpp::xep::xep0482::is_call_invite_element(payload))
        .count()
        > 1
}

fn observe_call_lifecycle(
    state: &WebSocketState,
    message: &xmpp_parsers::message::Message,
    conversation: crate::media::CallInviteConversationKey,
    sender_jid: &FullJid,
) {
    state
        .deps
        .protocol
        .media_gateway
        .observe_call_lifecycle(message, conversation, sender_jid);
}

fn ensure_muc_call_invite_session(
    state: &WebSocketState,
    message: &mut xmpp_parsers::message::Message,
    room_jid: &BareJid,
    sender_jid: &FullJid,
) -> Result<Option<MediaSessionId>, MediaGatewayError> {
    state.deps.protocol.media_gateway.ensure_invite_session(
        message,
        MediaSessionScope::Muc,
        jid::Jid::from(room_jid.clone()),
        sender_jid,
        &state.deps.service_domains.media,
    )
}

fn ensure_direct_call_invite_session(
    state: &WebSocketState,
    message: &mut xmpp_parsers::message::Message,
    sender_jid: &FullJid,
    to_jid: &jid::Jid,
) -> Result<Option<MediaSessionId>, MediaGatewayError> {
    state.deps.protocol.media_gateway.ensure_invite_session(
        message,
        MediaSessionScope::Direct,
        jid::Jid::from(to_jid.to_bare()),
        sender_jid,
        &state.deps.service_domains.media,
    )
}

fn bind_call_invite_reference(
    state: &WebSocketState,
    session_id: &MediaSessionId,
    conversation: crate::media::CallInviteConversationKey,
    invite_id: &str,
) -> Result<(), MediaGatewayError> {
    state
        .deps
        .protocol
        .media_gateway
        .bind_invite_reference(session_id, conversation, invite_id)
}

fn muc_call_conversation_key(room_jid: &BareJid) -> crate::media::CallInviteConversationKey {
    crate::media::CallInviteConversationKey::muc(room_jid.clone())
}

fn direct_call_conversation_key(
    sender_jid: &FullJid,
    to_jid: &jid::Jid,
) -> crate::media::CallInviteConversationKey {
    crate::media::CallInviteConversationKey::direct(sender_jid.to_bare(), to_jid.to_bare())
}

fn direct_call_invite_reference_id(message: &xmpp_parsers::message::Message) -> Option<String> {
    extract_origin_id(message).or_else(|| message.id.clone())
}

fn should_send_direct_carbon(message: &xmpp_parsers::message::Message) -> bool {
    if should_skip_carbons(message) || has_private_carbon(message) {
        return false;
    }
    message.type_ == XmppMessageType::Chat
        && (should_copy_message(message) || has_call_invite_payload(message))
}

fn has_private_carbon(message: &xmpp_parsers::message::Message) -> bool {
    message
        .payloads
        .iter()
        .any(|payload| payload.name() == "private" && payload.ns() == CARBONS_NS)
}

async fn groupchat_call_lifecycle_authorized(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    authenticated_session: &Option<Session>,
    payload: &CallInvitePayload,
) -> Result<CallLifecycleAuthorization, StanzaError> {
    let Some(reference_id) = payload.reference_id() else {
        return Ok(CallLifecycleAuthorization::Forbidden);
    };
    let Some(session) = state
        .deps
        .protocol
        .media_gateway
        .get_session_for_invite_reference(
            muc_call_conversation_key(room_jid),
            reference_id.as_str(),
        )
        .map_err(media_gateway_stanza_error)?
    else {
        return Ok(CallLifecycleAuthorization::Passthrough);
    };
    if session.scope != MediaSessionScope::Muc || session.anchor_jid.to_bare() != *room_jid {
        return Ok(CallLifecycleAuthorization::Forbidden);
    }
    if !muc_occupant_present(state, room_jid, sender_jid).await {
        return Ok(CallLifecycleAuthorization::Forbidden);
    }

    match payload {
        CallInvitePayload::Retract(_) => {
            if session.creator_jid == *sender_jid {
                return Ok(CallLifecycleAuthorization::Gateway);
            }
            if muc_call_permission_authorized(
                state,
                room_jid,
                authenticated_session,
                Permission::ManageCall,
            )
            .await
            {
                Ok(CallLifecycleAuthorization::Gateway)
            } else {
                Ok(CallLifecycleAuthorization::Forbidden)
            }
        }
        CallInvitePayload::Accept { method, .. } => {
            if !gateway_accept_method_matches_session(state, &session, method) {
                return Ok(CallLifecycleAuthorization::Forbidden);
            }
            if muc_call_permission_authorized(
                state,
                room_jid,
                authenticated_session,
                Permission::JoinCall,
            )
            .await
            {
                Ok(CallLifecycleAuthorization::Gateway)
            } else {
                Ok(CallLifecycleAuthorization::Forbidden)
            }
        }
        CallInvitePayload::Reject(_) | CallInvitePayload::Left(_) => {
            Ok(CallLifecycleAuthorization::Gateway)
        }
        CallInvitePayload::Invite(_) => Ok(CallLifecycleAuthorization::Forbidden),
    }
}

async fn direct_call_lifecycle_authorized(
    state: &WebSocketState,
    sender_jid: &FullJid,
    to_jid: &jid::Jid,
    payload: &CallInvitePayload,
) -> Result<CallLifecycleAuthorization, StanzaError> {
    let Some(reference_id) = payload.reference_id() else {
        return Ok(CallLifecycleAuthorization::Forbidden);
    };
    let Some(session) = state
        .deps
        .protocol
        .media_gateway
        .get_session_for_invite_reference(
            direct_call_conversation_key(sender_jid, to_jid),
            reference_id.as_str(),
        )
        .map_err(media_gateway_stanza_error)?
    else {
        return Ok(CallLifecycleAuthorization::Passthrough);
    };
    if session.scope != MediaSessionScope::Direct {
        return Ok(CallLifecycleAuthorization::Forbidden);
    }

    let creator = session.creator_jid.to_bare();
    let invitee = session.anchor_jid.to_bare();
    let sender = sender_jid.to_bare();
    let recipient = to_jid.to_bare();
    let sender_is_party = sender == creator || sender == invitee;
    let recipient_is_party = recipient == creator || recipient == invitee;
    if !sender_is_party || !recipient_is_party || sender == recipient {
        return Ok(CallLifecycleAuthorization::Forbidden);
    }

    let authorized = match payload {
        CallInvitePayload::Retract(_) => sender == creator,
        CallInvitePayload::Accept { method, .. } => {
            sender == invitee && gateway_accept_method_matches_session(state, &session, method)
        }
        CallInvitePayload::Reject(_) => sender == invitee,
        CallInvitePayload::Left(_) => true,
        CallInvitePayload::Invite(_) => false,
    };
    if authorized {
        Ok(CallLifecycleAuthorization::Gateway)
    } else {
        Ok(CallLifecycleAuthorization::Forbidden)
    }
}

fn gateway_accept_method_matches_session(
    state: &WebSocketState,
    session: &crate::media::MediaSession,
    method: &JoinMethod,
) -> bool {
    let JoinMethod::Jingle { sid, jid } = method else {
        return false;
    };
    sid.as_str() == session.id.as_str()
        && jid.as_ref().is_some_and(|jid| {
            gateway_jingle_jid_matches(jid, &state.deps.service_domains.media, session.id.as_str())
        })
}

async fn muc_call_start_authorized(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    authenticated_session: &Option<Session>,
) -> bool {
    if !muc_occupant_present(state, room_jid, sender_jid).await {
        return false;
    }
    muc_call_permission_authorized(
        state,
        room_jid,
        authenticated_session,
        Permission::StartCall,
    )
    .await
}

async fn muc_call_permission_authorized(
    state: &WebSocketState,
    room_jid: &BareJid,
    authenticated_session: &Option<Session>,
    permission: Permission,
) -> bool {
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
        return permission != Permission::ManageCall;
    };
    let Some(session) = authenticated_session.as_ref() else {
        return false;
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            object: Object::new(ObjectType::Channel, &channel_id),
            subject: Subject::user(&session.user_id),
            permission,
        })
        .await
        .is_ok_and(|response| response.allowed)
}

async fn muc_occupant_present(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
) -> bool {
    let Some(room_actor) = get_room_actor(state, room_jid).await else {
        return false;
    };
    room_actor.ask(GetSnapshot).await.is_ok_and(|snapshot| {
        snapshot
            .room
            .find_occupant_by_real_jid(sender_jid)
            .is_some()
    })
}

async fn direct_call_start_authorized(
    state: &WebSocketState,
    sender_jid: &FullJid,
    to_jid: &jid::Jid,
    authenticated_session: &Option<Session>,
) -> bool {
    if sender_jid.to_bare() == to_jid.to_bare() {
        return false;
    }
    let Some(session) = authenticated_session.as_ref() else {
        return false;
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            object: Object::new(
                ObjectType::Dm,
                direct_message_permission_object_id(&sender_jid.to_bare(), &to_jid.to_bare()),
            ),
            subject: Subject::user(&session.user_id),
            permission: Permission::Send,
        })
        .await
        .is_ok_and(|response| response.allowed)
}

pub(crate) fn direct_message_permission_object_id(
    first_jid: &BareJid,
    second_jid: &BareJid,
) -> String {
    let mut participants = [first_jid.to_string(), second_jid.to_string()];
    participants.sort_unstable();
    let raw = format!("{}\0{}", participants[0], participants[1]);
    format!("dm_{}", URL_SAFE_NO_PAD.encode(raw.as_bytes()))
}

fn media_gateway_stanza_error(error: MediaGatewayError) -> StanzaError {
    match error {
        MediaGatewayError::Disabled
        | MediaGatewayError::LiveKitUnavailable
        | MediaGatewayError::JingleBridgeUnavailable
        | MediaGatewayError::CapacityExceeded => StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "en",
            "Media gateway is unavailable.",
        ),
        MediaGatewayError::Forbidden => {
            forbidden_error("Sender is not permitted to use this call.")
        }
        MediaGatewayError::UnknownSession
        | MediaGatewayError::MissingSessionId
        | MediaGatewayError::MissingInviteReference
        | MediaGatewayError::SessionEnded => item_not_found_error("Call session not found."),
        MediaGatewayError::UnsupportedInviteMethod => {
            bad_request_error("Unsupported call invite join method.")
        }
        MediaGatewayError::InvalidSessionId => bad_request_error("Invalid call session id."),
        _ => bad_request_error("Invalid call invite payload."),
    }
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
    internal_server_error("Archive lookup failed while validating rich-message target.")
}

fn internal_server_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Wait,
        DefinedCondition::InternalServerError,
        "en",
        text,
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

async fn send_sent_carbons_to_websocket_resources(
    state: &WebSocketState,
    sender_jid: &FullJid,
    message: &xmpp_parsers::message::Message,
) {
    let sender_bare = sender_jid.to_bare();
    let resources = state
        .deps
        .protocol
        .connection_registry
        .get_other_carbon_resources_for_user(&sender_bare, sender_jid);

    for resource_jid in resources {
        let carbon =
            match build_sent_carbon(message, &sender_bare.to_string(), &resource_jid.to_string()) {
                Ok(carbon) => carbon,
                Err(error) => {
                    warn!(error = %error, to = %resource_jid, "Failed to build sent carbon");
                    continue;
                }
            };
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource_jid, Stanza::Message(carbon))
            .await;
    }
}

async fn send_received_carbons_to_websocket_resources(
    state: &WebSocketState,
    recipient_jid: &FullJid,
    message: &xmpp_parsers::message::Message,
) {
    let recipient_bare = recipient_jid.to_bare();
    let resources = state
        .deps
        .protocol
        .connection_registry
        .get_other_carbon_resources_for_user(&recipient_bare, recipient_jid);

    for resource_jid in resources {
        let carbon = match build_received_carbon(
            message,
            &recipient_bare.to_string(),
            &resource_jid.to_string(),
        ) {
            Ok(carbon) => carbon,
            Err(error) => {
                warn!(error = %error, to = %resource_jid, "Failed to build received carbon");
                continue;
            }
        };
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource_jid, Stanza::Message(carbon))
            .await;
    }
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
        || has_call_invite_payload(msg)
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

fn serialize_direct_stanza_xml(message: &xmpp_parsers::message::Message) -> Option<String> {
    match message_to_string(message) {
        Ok(xml) => Some(xml),
        Err(error) => {
            warn!(error = %error, "Failed to serialize direct message stanza XML for MAM archive");
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

/// Archive a direct message to both the sender's and recipient's personal MAM
/// archives. Body-less rich protocol events, including call invites, are kept
/// so XMPP lifecycle replay does not lose call state.
async fn archive_direct_message(
    state: &WebSocketState,
    sender_jid: &FullJid,
    to_jid: &jid::Jid,
    message: &xmpp_parsers::message::Message,
) {
    let body = prototype_body(message)
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    let rich = rich_archive_payload(message);
    if should_skip_storage(message) {
        return;
    }
    if body.is_empty() && rich.is_none() && !has_call_invite_payload(message) {
        return;
    }

    let stanza_xml = serialize_direct_stanza_xml(message);

    let (reply_to_id, reply_to_jid) = extract_reply_reference(message);
    let origin_id = extract_origin_id(message);

    let archived = ArchivedMessage {
        id: String::new(),
        timestamp: Utc::now(),
        from: sender_jid.to_bare().to_string(),
        to: to_jid.to_bare().to_string(),
        body,
        stanza_id: message.id.clone(),
        thread_id: message.thread.as_ref().map(|thread| thread.0.clone()),
        reply_to_id,
        reply_to_jid,
        origin_id,
        message_type: mam_message_type(&message.type_),
        stanza_xml,
        rich,
        nickname_generation: None,
    };

    // Store in sender's personal archive
    let sender_bare = sender_jid.to_bare().to_string();
    if let Err(err) = state
        .deps
        .protocol
        .mam_storage
        .store_message(sender_bare.as_str(), &archived)
        .await
    {
        warn!(
            from = %sender_jid,
            to = %to_jid,
            error = %err,
            "Failed to archive DM to sender's personal MAM"
        );
    }

    // Store in recipient's personal archive
    let recipient_bare = to_jid.to_bare().to_string();
    if let Err(err) = state
        .deps
        .protocol
        .mam_storage
        .store_message(recipient_bare.as_str(), &archived)
        .await
    {
        warn!(
            from = %sender_jid,
            to = %to_jid,
            error = %err,
            "Failed to archive DM to recipient's personal MAM"
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::xep::{
        xep0334::{build_hint_element, Hint},
        xep0359::build_origin_id_element,
        xep0482::{build_accept_element, CallInvite, CallInviteId, JingleSessionId, JoinMethod},
    };

    fn accept_element(invite_id: &CallInviteId) -> xmpp_parsers::minidom::Element {
        build_accept_element(
            invite_id,
            &JoinMethod::Jingle {
                sid: JingleSessionId::new("sid-a").expect("sid"),
                jid: Some("media.example.test/sid-a".parse().expect("jid")),
            },
        )
    }

    #[test]
    fn direct_call_invite_reference_prefers_origin_id() {
        let mut message = xmpp_parsers::message::Message::new(None);
        message.type_ = XmppMessageType::Chat;
        message.id = Some("message-id".to_string());
        message.payloads.push(build_origin_id_element("origin-id"));

        assert_eq!(
            direct_call_invite_reference_id(&message).as_deref(),
            Some("origin-id")
        );
    }

    #[test]
    fn bodyless_call_invite_lifecycle_is_carbon_eligible() {
        let mut message = xmpp_parsers::message::Message::new(None);
        message.type_ = XmppMessageType::Chat;
        let invite_id = CallInviteId::new("origin-id").expect("id");
        message.payloads.push(accept_element(&invite_id));

        assert!(should_send_direct_carbon(&message));
    }

    #[test]
    fn no_copy_suppresses_bodyless_call_invite_lifecycle_carbon() {
        let mut message = xmpp_parsers::message::Message::new(None);
        message.type_ = XmppMessageType::Chat;
        let invite_id = CallInviteId::new("origin-id").expect("id");
        message.payloads.push(accept_element(&invite_id));
        message.payloads.push(build_hint_element(Hint::NoCopy));

        assert!(!should_send_direct_carbon(&message));
    }

    #[test]
    fn private_suppresses_bodyless_call_invite_lifecycle_carbon() {
        let mut message = xmpp_parsers::message::Message::new(None);
        message.type_ = XmppMessageType::Chat;
        let invite_id = CallInviteId::new("origin-id").expect("id");
        message.payloads.push(accept_element(&invite_id));
        message
            .payloads
            .push(Element::builder("private", CARBONS_NS).build());

        assert!(!should_send_direct_carbon(&message));
    }

    #[test]
    fn multiple_call_invite_payloads_are_rejected() {
        let mut message = xmpp_parsers::message::Message::new(None);
        let invite_id = CallInviteId::new("origin-id").expect("id");
        message.payloads.push(accept_element(&invite_id));
        message.payloads.push(accept_element(&invite_id));

        assert!(has_multiple_call_invite_payloads(&message));
    }

    #[test]
    fn bodyless_plain_message_is_not_carbon_eligible() {
        let mut message = xmpp_parsers::message::Message::new(None);
        message.type_ = XmppMessageType::Chat;

        assert!(!should_send_direct_carbon(&message));
    }

    #[test]
    fn gateway_call_invite_detects_waddle_jingle_method() {
        let unaddressed = CallInvite::new().with_method(JoinMethod::Jingle {
            sid: JingleSessionId::new("sid-a").expect("sid"),
            jid: None,
        });
        assert!(!is_gateway_call_invite(&unaddressed, "media.example.test"));

        let addressed = CallInvite::new().with_method(JoinMethod::Jingle {
            sid: JingleSessionId::new("sid-a").expect("sid"),
            jid: Some("media.example.test/sid-a".parse().expect("jid")),
        });
        assert!(is_gateway_call_invite(&addressed, "media.example.test"));

        let external = CallInvite::new().with_method(JoinMethod::External {
            uri: "https://calls.example.test/room".parse().expect("uri"),
        });
        assert!(!is_gateway_call_invite(&external, "media.example.test"));

        let addressed_with_external = CallInvite::new()
            .with_method(JoinMethod::External {
                uri: "https://calls.example.test/room".parse().expect("uri"),
            })
            .with_method(JoinMethod::Jingle {
                sid: JingleSessionId::new("sid-a").expect("sid"),
                jid: Some("media.example.test/sid-a".parse().expect("jid")),
            });
        assert!(is_gateway_call_invite(
            &addressed_with_external,
            "media.example.test"
        ));

        let duplicate_waddle_methods = CallInvite::new()
            .with_method(JoinMethod::Jingle {
                sid: JingleSessionId::new("sid-a").expect("sid"),
                jid: Some("media.example.test/sid-a".parse().expect("jid")),
            })
            .with_method(JoinMethod::Jingle {
                sid: JingleSessionId::new("sid-b").expect("sid"),
                jid: Some("media.example.test/sid-b".parse().expect("jid")),
            });
        assert!(is_gateway_call_invite(
            &duplicate_waddle_methods,
            "media.example.test"
        ));

        let other_jingle = CallInvite::new().with_method(JoinMethod::Jingle {
            sid: JingleSessionId::new("sid-a").expect("sid"),
            jid: Some("elsewhere.example.test/sid-a".parse().expect("jid")),
        });
        assert!(!is_gateway_call_invite(&other_jingle, "media.example.test"));

        let addressed_with_node = CallInvite::new().with_method(JoinMethod::Jingle {
            sid: JingleSessionId::new("sid-a").expect("sid"),
            jid: Some("node@media.example.test/sid-a".parse().expect("jid")),
        });
        assert!(!is_gateway_call_invite(
            &addressed_with_node,
            "media.example.test"
        ));
    }
}
