use chrono::Utc;
use jid::{BareJid, FullJid};
use tracing::{debug, info, warn};
use waddle_extensions::message_has_embed_for_namespaces;
use waddle_xmpp::{
    carbons::{build_received_carbon, build_sent_carbon, should_copy_message},
    inbox::{
        runtime::{
            direct_message_entry, groupchat_entry, groupchat_thread_entry, preview_text,
            should_project_message,
        },
        InboxEntry,
    },
    mam::{add_stanza_id as add_mam_stanza_id, ArchivedMessage, STANZA_ID_NS},
    muc::room_actor::BuildGroupchatBroadcast,
    registry::{BroadcastOutcome, SendResult},
    xep::xep0430::build_inbox_push,
    xep::{
        has_file_sharing, is_moderation_request_message, is_moderation_result_message,
        is_reaction_message, is_retraction_message, is_sticker_message, should_skip_storage,
        NS_REPLY,
    },
    Stanza,
};
use xmpp_parsers::message::MessageType as XmppMessageType;

use super::super::{get_room_actor, stanza_to_xml, WebSocketState};
use crate::auth::Session;
use waddle_xmpp::protocol::ConnectionPhase;

fn archived_stanza_xml(message: &xmpp_parsers::message::Message) -> String {
    stanza_to_xml(&Stanza::Message(message.clone()))
}

pub async fn handle_message(
    mut incoming: xmpp_parsers::message::Message,
    muc_domain: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    _authenticated_session: &Option<Session>,
) -> Vec<String> {
    let Some(sender_jid) = phase.bound_jid() else {
        warn!("Message received without authenticated session");
        return vec![];
    };

    // Always stamp the authenticated sender.
    incoming.from = Some(jid::Jid::from(sender_jid.clone()));

    // Handle groupchat messages
    if incoming.type_ == XmppMessageType::Groupchat {
        let Some(to_jid) = incoming.to.as_ref() else {
            warn!("Groupchat message without 'to' attribute");
            return vec![];
        };

        // Parse room JID (strip resource if present)
        let room_jid = to_jid.to_bare();

        if room_jid.domain().as_str() != muc_domain {
            warn!(to = %to_jid, "Groupchat message to non-MUC JID");
            return vec![];
        }

        debug!(room = %room_jid, sender = %sender_jid, "Groupchat message");

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

        // Enrich: detect GitHub links and append embed XML elements (fail-open)
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
        let mut local_messages = broadcast.federated_messages.local;
        let occupant_bare_jids = broadcast.occupant_bare_jids;

        let from_room_jid = format!("{}/{}", room_jid, sender_nick);
        if let Ok(from_jid) = from_room_jid.parse::<FullJid>() {
            prototype.from = Some(jid::Jid::from(from_jid));
        } else {
            prototype.from = Some(jid::Jid::from(sender_jid.clone()));
        }
        prototype.to = None;

        // Archive body-bearing room messages in XMPP MAM storage.
        let archive_id = archive_groupchat_message(state, &room_jid, &prototype).await;
        if let Some(ref archive_id) = archive_id {
            add_mam_stanza_id(&mut prototype, archive_id.as_str(), &room_jid.to_string());
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
                // Resolve thread title: XEP-0508 thread-create title, or first message preview
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

    // Handle direct messages (chat)
    if incoming.type_ == XmppMessageType::Chat {
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
            prototype.type_ = XmppMessageType::Chat;
            let should_carbon =
                prototype.type_ == XmppMessageType::Chat && should_copy_message(&prototype);

            // Enrich: detect GitHub links and append embed XML elements
            let _embeds_added = state
                .deps
                .protocol
                .extension_manager
                .enrich_message(&mut prototype)
                .await;
            let has_github_embed = message_has_embed_for_namespaces(
                &prototype,
                state.deps.protocol.extension_manager.feature_namespaces(),
            );

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

            if has_github_embed {
                let echo = prototype.clone();
                return vec![stanza_to_xml(&Stanza::Message(echo))];
            }
        } else {
            warn!("Direct chat message without 'to' attribute");
        }
        return vec![];
    }

    debug!(msg_type = ?incoming.type_, "Message stanza received");
    vec![]
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
/// Mirrors the `should_archive_timeline_message` predicate in `connection.rs`:
/// body/subject-bearing messages are always archived; body-less protocol
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

async fn archive_groupchat_message(
    state: &WebSocketState,
    room_jid: &BareJid,
    message: &xmpp_parsers::message::Message,
) -> Option<String> {
    if !should_archive_groupchat_message(message) {
        return None;
    }

    let body = prototype_body(message)
        .map(|value| value.trim().to_string())
        .unwrap_or_default();

    let (reply_to_id, reply_to_jid) = extract_reply_reference(message);
    let origin_id = extract_origin_id(message);

    let archived = ArchivedMessage {
        id: String::new(),
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
        stanza_xml: Some(archived_stanza_xml(message)),
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

/// Archive a direct (type="chat") message to both the sender's and recipient's
/// personal MAM archives.  Only messages with a `<body>` are stored.
async fn archive_direct_message(
    state: &WebSocketState,
    sender_jid: &FullJid,
    to_jid: &jid::Jid,
    message: &xmpp_parsers::message::Message,
) {
    let Some(body) = prototype_body(message)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return;
    };

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
        stanza_xml: Some(archived_stanza_xml(message)),
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
