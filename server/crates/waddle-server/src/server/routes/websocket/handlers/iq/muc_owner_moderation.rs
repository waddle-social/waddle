use super::*;

pub(super) async fn handle_muc_owner_and_moderation_iq(
    iq: &xmpp_parsers::iq::Iq,
    id: &str,
    payload_ns: &str,
    target_to: Option<&str>,
    has_destroy: bool,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    authenticated_session: &Option<Session>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    // MUC owner IQ (XEP-0045): instant room config submit and room destroy.
    // This is needed for clients that create a room by:
    // 1) joining via presence
    // 2) submitting an empty owner form (`jabber:x:data` type='submit')
    if payload_ns == "http://jabber.org/protocol/muc#owner" {
        let Some(target) = target_to else {
            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        };

        let room_target = target.split('/').next().unwrap_or(target);
        let Ok(room_jid) = room_target.parse::<BareJid>() else {
            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                jid_malformed_iq_error("Malformed JID in IQ addressing."),
            )];
        };

        if !is_muc_room_jid(state, &room_jid).await {
            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                item_not_found_iq_error("Requested item not found."),
            )];
        }

        let Some(sender_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )];
        };
        match muc_owner_authorized(state, &room_jid, sender_jid, authenticated_session.as_ref())
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return vec![build_iq_error_xml_typed(
                    &id,
                    response_from,
                    response_to,
                    forbidden_iq_error("Operation not permitted."),
                )];
            }
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = %error,
                    "Failed to authorize MUC owner IQ"
                );
                return vec![build_iq_error_xml_typed(
                    &id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        }

        if has_destroy {
            if destroy_room_actor(state, &room_jid).await {
                debug!(room = %room_jid, "Destroyed MUC room via owner IQ");
                let room_jid_string = room_jid.to_string();
                return vec![build_iq_result_xml(
                    &id,
                    Some(room_jid_string.as_str()),
                    response_to,
                    None,
                )];
            }

            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                item_not_found_iq_error("Requested item not found."),
            )];
        }

        if matches!(&iq.payload, xmpp_parsers::iq::IqType::Get(_)) {
            match build_muc_owner_config_response(state, &room_jid, &id, response_to).await {
                Ok(response) => return vec![response],
                Err(error) => {
                    warn!(
                        room = %room_jid,
                        error = %error,
                        "Failed to build MUC owner config response"
                    );
                    return vec![build_iq_error_xml_typed(
                        &id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )];
                }
            }
        }

        if let Err(error) =
            apply_muc_owner_config(state, &room_jid, &iq, authenticated_session.as_ref()).await
        {
            warn!(
                room = %room_jid,
                error = %error,
                "Failed to apply MUC owner config"
            );
            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }

        // Treat all other owner IQ sets as successful config submit for instant rooms.
        let room_jid_string = room_jid.to_string();
        return vec![build_iq_result_xml(
            &id,
            Some(room_jid_string.as_str()),
            response_to,
            None,
        )];
    }

    if let Some(request) = parse_moderation_iq(&iq) {
        let Some(sender_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )];
        };
        let Some(room_jid) = iq.to.as_ref().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        };
        let Some(room_actor) = get_room_actor(state, &room_jid).await else {
            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                item_not_found_iq_error("Requested item not found."),
            )];
        };
        let context = match room_actor
            .ask(GetAdminContext {
                sender_jid: sender_jid.clone(),
            })
            .await
        {
            Ok(context) => context,
            Err(_) => {
                return vec![build_iq_error_xml_typed(
                    &id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        };
        // XEP-0425 §"only moderators are allowed to moderate" combined with
        // XEP-0045 §5.1.2: runtime moderation privilege is role-bound, not
        // affiliation-bound. Owner/Admin affiliations only matter to the
        // extent that they cause the room to grant the Moderator *role* on
        // entry; if an owner has explicitly taken a non-moderator role
        // (e.g. visitor), that signal is intentional and must be honoured.
        if !matches!(context.role, waddle_xmpp::Role::Moderator) {
            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                forbidden_iq_error("Operation not permitted."),
            )];
        }
        match state
            .deps
            .protocol
            .mam_storage
            .get_message(&request.target_id)
            .await
        {
            Ok(Some(message)) if message.to.to_string() == room_jid.to_string() => {}
            Ok(Some(_)) => {
                return vec![build_iq_error_xml_typed(
                    &id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )];
            }
            Ok(None) => {
                return vec![build_iq_error_xml_typed(
                    &id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )];
            }
            Err(error) => {
                warn!(room = %room_jid, target = %request.target_id, error = %error, "Failed to look up moderation target");
                return vec![build_iq_error_xml_typed(
                    &id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        }

        let moderator_nick = context
            .nick
            .unwrap_or_else(|| sender_jid.resource().to_string());
        let moderated_by = format!("{room_jid}/{moderator_nick}");
        let stamp_time = chrono::Utc::now();
        let stamp = stamp_time.to_rfc3339();
        let mut moderation = build_moderation_result_message(
            Some(jid::Jid::from(room_jid.clone())),
            &request.target_id,
            &moderated_by,
            &stamp,
            request.reason.as_deref(),
        );
        let archive_id = uuid::Uuid::now_v7().to_string();
        let room_jid_full = jid::Jid::from(room_jid.clone());
        add_stanza_id_xep0359(
            &mut moderation,
            &Xep0359StanzaId::new(archive_id.as_str(), room_jid_full.clone()),
        );

        if let (Some(target_id), Ok(moderator_jid)) = (
            RichMessageId::new(request.target_id.clone()),
            moderated_by.parse::<Jid>(),
        ) {
            let archived = ArchivedMessage {
                id: archive_id.clone(),
                timestamp: chrono::Utc::now(),
                from: jid::Jid::from(room_jid.clone()),
                to: jid::Jid::from(room_jid.clone()),
                // XEP-0425 moderation tombstone has no `<body>` — `None`
                // is the wire-faithful "no body element" form.
                body: None,
                stanza_id: moderation
                    .id
                    .clone()
                    .map(|id| Xep0359StanzaId::new(id, room_jid_full.clone())),
                // XEP-0425 moderation tombstone: leak-prone fields are
                // already cleared by construction (this row is a fresh
                // tombstone, not a scrub of an existing message).
                thread: None,
                reply: None,
                origin_id: None,
                message_type: xmpp_parsers::message::MessageType::Groupchat,
                stanza_xml: None,
                rich: Some(ArchivedRichMessage {
                    payload: Some(ArchivedRichPayload::Moderation(ArchivedModeration {
                        target_id,
                        moderated_by: moderator_jid,
                        stamp: Some(stamp_time),
                        reason: request.reason.as_deref().and_then(RichText::new),
                    })),
                    reply: None,
                    references: Vec::new(),
                    mentions: Vec::new(),
                }),
                nickname_generation: None,
            };
            if let Err(error) = state
                .deps
                .protocol
                .mam_storage
                .store_message(&room_jid, &archived)
                .await
            {
                warn!(room = %room_jid, target = %request.target_id, error = %error, "Failed to archive moderation event");
            }

            // XEP-0425 §"the archiving service MAY replace the
            // retracted message with a tombstone": replace the
            // original room archive row with a moderation tombstone
            // whose `<retracted/>` carries `<moderated by/>` and the
            // optional reason.
            let original_lookup = state
                .deps
                .protocol
                .mam_storage
                .get_message(&request.target_id)
                .await;
            match original_lookup {
                Ok(Some(original)) if original.to.to_string() == room_jid.to_string() => {
                    // Use the moderation message's server-assigned
                    // archive id (XEP-0359 stanza-id stamped via
                    // `add_stanza_id_xep0359` above). That's the id
                    // clients see on the live moderation broadcast
                    // and need to correlate against the tombstone —
                    // `moderation.id` is the client message-id
                    // attribute, which would not match the archive
                    // entry clients can resolve.
                    let tombstone = waddle_xmpp::mam::ArchivedTombstone {
                        retraction_id: waddle_xmpp::mam::RichMessageId::new(archive_id.clone()),
                        stamp: stamp_time,
                        moderation: Some(ArchivedModeration {
                            target_id: waddle_xmpp::mam::RichMessageId::new(
                                request.target_id.clone(),
                            )
                            .expect("target id is non-empty here"),
                            moderated_by: moderated_by
                                .parse::<Jid>()
                                .expect("moderated_by parsed earlier"),
                            stamp: Some(stamp_time),
                            reason: request.reason.as_deref().and_then(RichText::new),
                        }),
                    };
                    if let Err(error) = state
                        .deps
                        .protocol
                        .mam_storage
                        .replace_with_tombstone(&original.id, tombstone)
                        .await
                    {
                        warn!(
                            room = %room_jid,
                            target = %request.target_id,
                            error = %error,
                            "Failed to replace original with moderation tombstone"
                        );
                    }
                    // XEP-0425 §Tombstones / XEP-0198: scrub the
                    // pre-tombstone groupchat reflection from any
                    // detached resume queues so a recipient mid-resume
                    // does not replay the moderated content. Best
                    // effort — tombstone is already applied to the
                    // archive. Scope by the room JID so the matcher's
                    // stanza-id branch finds groupchat reflections
                    // that key by the room's XEP-0359 stamp, and so a
                    // colliding wire id in another conversation is not
                    // accidentally scrubbed (Codex P1, Copilot review
                    // on PR #305).
                    use waddle_xmpp::stream_management::SmSessionRegistry as _;
                    let target_id = request.target_id.as_str();
                    let room_jid_str = room_jid.to_string();
                    match state
                        .deps
                        .protocol
                        .sm_session_registry
                        .scrub_unacked_for_tombstone(target_id, &room_jid_str)
                        .await
                    {
                        Ok(removed) if removed > 0 => debug!(
                            room = %room_jid,
                            target = target_id,
                            removed,
                            "XEP-0425 moderation: scrubbed unacked SM queue entries"
                        ),
                        Ok(_) => {}
                        Err(error) => warn!(
                            room = %room_jid,
                            target = target_id,
                            %error,
                            "XEP-0425 moderation: scrub_unacked_for_tombstone failed; pre-scrub stanza may still replay on resume"
                        ),
                    }
                }
                Ok(_) => {}
                Err(error) => warn!(
                    room = %room_jid,
                    target = %request.target_id,
                    error = %error,
                    "Failed to look up moderation target for tombstone"
                ),
            }
        }

        let mut frames = Vec::new();
        if let Ok(snapshot) = room_actor.ask(GetSnapshot).await {
            for occupant in snapshot.room.occupants.values() {
                for occupant_jid in snapshot.room.get_occupant_sessions(&occupant.nick) {
                    let mut outbound = moderation.clone();
                    outbound.to = Some(jid::Jid::from(occupant_jid.clone()));
                    if occupant_jid == *sender_jid {
                        frames.push(stanza_to_xml(&Stanza::Message(outbound)));
                        continue;
                    }
                    let _ = state
                        .deps
                        .protocol
                        .connection_registry
                        .try_send_to(&occupant_jid, Stanza::Message(outbound));
                }
            }
        }

        frames.push(build_iq_result_xml(&id, response_from, response_to, None));
        return frames;
    }
    Vec::new()
}
