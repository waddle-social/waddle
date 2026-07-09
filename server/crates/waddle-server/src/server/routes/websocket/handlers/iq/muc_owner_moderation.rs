use super::errors::resource_constraint_iq_error;
use super::*;
use waddle_xmpp::muc::room_registry_actor::{
    DestroyRoomExactAfterEffects, RoomDestroyEffectsDone, RoomDestroyEffectsReserved,
};
use waddle_xmpp::muc::{build_destroy_notification, DestroyRequest, NS_MUC_OWNER};

/// Extract the optional alternate venue, reason, and password from a
/// `<destroy xmlns='muc#owner'/>` child of the owner-config IQ. All
/// fields are optional per XEP-0045 §10.9, so a malformed or empty
/// element yields `DestroyRequest::default()` and the destroy still
/// proceeds.
fn parse_destroy_request(iq: &xmpp_parsers::iq::Iq) -> Option<DestroyRequest> {
    let xmpp_parsers::iq::Iq::Set { payload: query, .. } = iq else {
        return None;
    };
    let destroy = query.get_child("destroy", NS_MUC_OWNER)?;
    let mut request = DestroyRequest::default();
    if let Some(jid_str) = destroy.attr("jid") {
        request.alternate_venue = jid_str.parse().ok();
    }
    for child in destroy.children() {
        match child.name() {
            "reason" => {
                let text = child.text();
                if !text.is_empty() {
                    request.reason = Some(text);
                }
            }
            "password" => {
                let text = child.text();
                if !text.is_empty() {
                    request.password = Some(text);
                }
            }
            _ => {}
        }
    }
    Some(request)
}

pub(super) async fn handle_muc_owner_and_moderation_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    authenticated_session: &Option<Session>,
) -> Vec<String> {
    let iq = ctx.iq;
    let id = ctx.id;
    let payload_ns = ctx.payload_ns;
    let target_to = ctx.target_to;
    let has_destroy = ctx.has_destroy;
    let response_from = ctx.response_from;
    let response_to = ctx.response_to;

    // MUC owner IQ (XEP-0045): instant room config submit and room destroy.
    // This is needed for clients that create a room by:
    // 1) joining via presence
    // 2) submitting an empty owner form (`jabber:x:data` type='submit')
    if payload_ns == "http://jabber.org/protocol/muc#owner" {
        let Some(target) = target_to else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        };

        let room_target = target.split('/').next().unwrap_or(target);
        let Ok(room_jid) = room_target.parse::<BareJid>() else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                jid_malformed_iq_error("Malformed JID in IQ addressing."),
            )];
        };

        if !is_muc_room_jid(state, &room_jid).await {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                item_not_found_iq_error("Requested item not found."),
            )];
        }

        let Some(sender_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_typed(
                id,
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
                    id,
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
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        }

        if has_destroy {
            // XEP-0045 §10.9: parse the optional alternate venue and
            // reason out of the `<destroy/>` payload, snapshot the
            // current occupants from the room actor, then broadcast a
            // typed destroy presence to each session before tearing
            // the actor down. The sender's frame is returned inline
            // alongside the IQ result so it lands on the same socket;
            // others are routed via the connection registry.
            let destroy_request = parse_destroy_request(iq).unwrap_or_default();
            let mut frames = Vec::new();
            let Some(room_actor) = get_room_actor(state, &room_jid).await else {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    resource_constraint_iq_error(
                        "Room ownership cannot currently be verified; please retry.",
                    ),
                )];
            };
            // Prove this exact actor incarnation before asking the registry
            // to seal its admission mailbox and return the final snapshot.
            match fence_room_effects(state, &room_jid, &room_actor).await {
                FencedRoomEffectsOutcome::Authorized => {}
                FencedRoomEffectsOutcome::NotOwner => {
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        resource_constraint_iq_error(
                            "Room ownership recently moved; please retry.",
                        ),
                    )];
                }
                FencedRoomEffectsOutcome::OwnershipUncertain => {
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        resource_constraint_iq_error(
                            "Room ownership cannot currently be verified; please retry.",
                        ),
                    )];
                }
            }

            // The registry performs the final exact claim + actor-ref proof,
            // removes E1, and then holds its serialized mailbox (and E1's
            // exact claim grant) until this caller completes the irreversible
            // presence/SFU batch. A queued E2 create cannot run in the gap.
            let (effects_reserved_sender, mut effects_reserved_receiver) =
                tokio::sync::oneshot::channel();
            let (effects_done_sender, effects_done_receiver) = tokio::sync::oneshot::channel();
            let room_registry = state.deps.protocol.room_registry.clone();
            let destroy_room_jid = room_jid.clone();
            let destroy_expected_actor = room_actor.clone();
            let destroy_barrier = async move {
                room_registry
                    .ask(DestroyRoomExactAfterEffects {
                        room_jid: destroy_room_jid,
                        expected_actor: destroy_expected_actor,
                        effects_reserved: effects_reserved_sender,
                        effects_done: effects_done_receiver,
                    })
                    .await
            };
            tokio::pin!(destroy_barrier);

            let reservation = tokio::select! {
                reservation = &mut effects_reserved_receiver => reservation,
                result = &mut destroy_barrier => {
                    warn!(
                        room = %room_jid,
                        result = ?result,
                        "Exact room destroy ended before reserving its external effects"
                    );
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        resource_constraint_iq_error(
                            "Room ownership changed before destruction committed; please retry.",
                        ),
                    )];
                }
            };
            let snapshot = match reservation {
                Ok(RoomDestroyEffectsReserved { snapshot }) => snapshot,
                Err(_) => {
                    let result = destroy_barrier.await;
                    warn!(
                        room = %room_jid,
                        result = ?result,
                        "Exact room destroy refused its external-effect reservation"
                    );
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        resource_constraint_iq_error(
                            "Room ownership changed before destruction committed; please retry.",
                        ),
                    )];
                }
            };

            for occupant in snapshot.room.occupants.values() {
                let is_self_occupant = occupant.real_jid == *sender_jid;
                // XEP-0421: the destroy notification is the
                // occupant's final unavailable presence from
                // the room and MUST carry their occupant-id
                // (#1268).
                let occupant_bare = occupant.real_jid.to_bare();
                let identity = waddle_xmpp::xep::xep0421::OccupantIdentity {
                    bare_jid: &occupant_bare,
                    real_jid: Some(&occupant.real_jid),
                    secret: &state.deps.occupant_id_secret,
                };
                let presence = build_destroy_notification(
                    &room_jid,
                    &occupant.nick,
                    &occupant.real_jid,
                    &destroy_request,
                    is_self_occupant,
                    &identity,
                );
                if is_self_occupant {
                    frames.push(stanza_to_xml(&Stanza::Presence(presence)));
                } else {
                    let _ = state
                        .deps
                        .protocol
                        .connection_registry
                        .try_send_to(&occupant.real_jid, Stanza::Presence(presence));
                }
                // XEP-0045 §10.9 destroy ends every occupant's
                // session in the room — their LiveKit
                // participant must end with it. Without this
                // the SFU keeps the room populated until its
                // own timeout even though the XMPP room is
                // gone. Idempotent for non-call participants.
                super::super::super::muc_call_sfu::unregister_participant_from_room(
                    state,
                    &room_jid,
                    &occupant.real_jid,
                );
            }

            let _ = effects_done_sender.send(RoomDestroyEffectsDone);
            let destroy_result = destroy_barrier.await;
            if !matches!(destroy_result, Ok(true)) {
                // The exact actor was already removed and its effects were
                // committed once the reservation was granted. A lost cleanup
                // reply cannot roll them back and must not turn success into a
                // contradictory IQ error.
                warn!(
                    room = %room_jid,
                    result = ?destroy_result,
                    "Exact room destroy committed effects but cleanup acknowledgement was unavailable"
                );
            }
            debug!(room = %room_jid, "Destroyed MUC room via owner IQ");
            let room_jid_string = room_jid.to_string();
            frames.push(build_iq_result_xml(
                id,
                Some(room_jid_string.as_str()),
                response_to,
                None,
            ));
            return frames;
        }

        if matches!(iq, xmpp_parsers::iq::Iq::Get { .. }) {
            match build_muc_owner_config_response(state, &room_jid, id, response_to).await {
                Ok(response) => return vec![response],
                Err(error) => {
                    warn!(
                        room = %room_jid,
                        error = %error,
                        "Failed to build MUC owner config response"
                    );
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )];
                }
            }
        }

        match apply_muc_owner_config(state, &room_jid, iq, authenticated_session.as_ref()).await {
            Ok(()) => {}
            Err(super::muc_owner_config::MucOwnerConfigError::NotOwner) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    resource_constraint_iq_error("Room ownership recently moved; please retry."),
                )];
            }
            Err(super::muc_owner_config::MucOwnerConfigError::OwnershipUncertain) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    resource_constraint_iq_error(
                        "Room ownership cannot currently be verified; please retry.",
                    ),
                )];
            }
            Err(super::muc_owner_config::MucOwnerConfigError::PersistFailed(error)) => {
                warn!(room = %room_jid, %error, "MUC owner config durable persist failed");
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error(
                        "The room change could not converge durably; please retry.",
                    ),
                )];
            }
            Err(error) => {
                warn!(room = %room_jid, %error, "Failed to apply MUC owner config");
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        }

        // Treat all other owner IQ sets as successful config submit for instant rooms.
        let room_jid_string = room_jid.to_string();
        return vec![build_iq_result_xml(
            id,
            Some(room_jid_string.as_str()),
            response_to,
            None,
        )];
    }

    if let Some(request) = parse_moderation_iq(iq) {
        let Some(sender_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )];
        };
        let Some(room_jid) = iq.to().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        };
        let Some(room_actor) = get_room_actor(state, &room_jid).await else {
            return vec![build_iq_error_xml_typed(
                id,
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
                    id,
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
                id,
                response_from,
                response_to,
                forbidden_iq_error("Operation not permitted."),
            )];
        }
        let original = match state
            .deps
            .protocol
            .mam_storage
            .get_message(&request.target_id)
            .await
        {
            Ok(Some(message)) if message.to.to_bare() == room_jid => message,
            Ok(Some(_)) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )];
            }
            Ok(None) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )];
            }
            Err(error) => {
                warn!(room = %room_jid, target = %request.target_id, error = %error, "Failed to look up moderation target");
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        };

        #[cfg(feature = "clustering")]
        let room_fence = {
            let clustering = &state.deps.app_state.clustering_claims;
            if clustering.claim_store.is_some() {
                if clustering.muc_durable_store.is_none() {
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        resource_constraint_iq_error(
                            "Room ownership cannot currently be verified; please retry.",
                        ),
                    )];
                }
                let fence = match room_actor.ask(GetRoomClaimFence).await {
                    Ok(Some(fence)) => fence,
                    Ok(None) | Err(_) => {
                        return vec![build_iq_error_xml_typed(
                            id,
                            response_from,
                            response_to,
                            resource_constraint_iq_error(
                                "Room ownership cannot currently be verified; please retry.",
                            ),
                        )];
                    }
                };
                if fence.entity
                    != waddle_xmpp::ownership::Entity::new(
                        waddle_xmpp::ownership::EntityType::RoomActor,
                        room_jid.to_string(),
                    )
                {
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        resource_constraint_iq_error(
                            "Room ownership cannot currently be verified; please retry.",
                        ),
                    )];
                }
                Some(fence)
            } else {
                None
            }
        };

        let moderator_nick = context
            .nick
            .unwrap_or_else(|| sender_jid.resource().to_string());
        let moderated_by = format!("{room_jid}/{moderator_nick}");
        let stamp_time = chrono::Utc::now();
        // XEP-0425 v1 §3: the broadcast `<moderated>` element
        // carries the moderator's XEP-0421 occupant-id so
        // clients have a stable attribution identifier alongside
        // the bare JID in `by=`.
        let moderator_occupant_id = waddle_xmpp::xep::xep0421::generate_occupant_id(
            &sender_jid.to_bare(),
            &room_jid,
            &state.deps.occupant_id_secret,
        );
        let mut moderation = build_moderation_result_message(
            Some(jid::Jid::from(room_jid.clone())),
            &request.target_id,
            &moderated_by,
            Some(moderator_occupant_id.as_str()),
            request.reason.as_deref(),
        );
        let Some(moderation_wire_id) = moderation.id.as_ref().map(|id| id.0.clone()) else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        };
        let archive_id = uuid::Uuid::now_v7().to_string();
        let room_jid_full = jid::Jid::from(room_jid.clone());
        add_stanza_id_xep0359(
            &mut moderation,
            &Xep0359StanzaId::new(archive_id.as_str(), room_jid_full.clone()),
        );

        let Some(target_id) = RichMessageId::new(original.id.clone()) else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed moderation target."),
            )];
        };
        let Ok(moderator_jid) = moderated_by.parse::<Jid>() else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        };
        let archived = ArchivedMessage {
            id: archive_id.clone(),
            timestamp: stamp_time,
            from: room_jid_full.clone(),
            to: room_jid_full.clone(),
            body: None,
            // `ArchivedMessage.id` is the room-assigned archive/stanza ID;
            // `stanza_id` carries the live moderation message's wire `id` so
            // MAM replay preserves it and the tombstone can cite it exactly.
            stanza_id: Some(Xep0359StanzaId::new(
                moderation_wire_id.clone(),
                room_jid_full.clone(),
            )),
            thread: None,
            reply: None,
            origin_id: None,
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            stanza_xml: None,
            rich: Some(ArchivedRichMessage {
                payload: Some(ArchivedRichPayload::Moderation(ArchivedModeration {
                    target_id: target_id.clone(),
                    moderated_by: moderator_jid.clone(),
                    stamp: Some(stamp_time),
                    reason: request.reason.as_deref().and_then(RichText::new),
                })),
                reply: None,
                references: Vec::new(),
                mentions: Vec::new(),
                // Room-authored moderation event row: no occupant
                // sender to identify.
                occupant_id: None,
                muc_sender: None,
            }),
            nickname_generation: None,
        };
        let tombstone = waddle_xmpp::mam::ArchivedTombstone {
            retraction_id: RichMessageId::new(moderation_wire_id),
            stamp: stamp_time,
            moderation: Some(ArchivedModeration {
                target_id,
                moderated_by: moderator_jid,
                stamp: Some(stamp_time),
                reason: request.reason.as_deref().and_then(RichText::new),
            }),
        };

        #[cfg(feature = "clustering")]
        let persistence_result = if let Some(fence) = room_fence.as_ref() {
            state
                .deps
                .protocol
                .mam_storage
                .moderate_message_fenced(
                    &room_jid,
                    &archived,
                    &original.id,
                    tombstone.clone(),
                    fence,
                )
                .await
        } else {
            match state
                .deps
                .protocol
                .mam_storage
                .store_message(&room_jid, &archived)
                .await
            {
                Ok(_) => {
                    state
                        .deps
                        .protocol
                        .mam_storage
                        .replace_with_tombstone(&original.id, tombstone.clone())
                        .await
                }
                Err(error) => Err(error),
            }
        };
        #[cfg(not(feature = "clustering"))]
        let persistence_result = match state
            .deps
            .protocol
            .mam_storage
            .store_message(&room_jid, &archived)
            .await
        {
            Ok(_) => {
                state
                    .deps
                    .protocol
                    .mam_storage
                    .replace_with_tombstone(&original.id, tombstone)
                    .await
            }
            Err(error) => Err(error),
        };

        match persistence_result {
            Ok(true) => {}
            Ok(false) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )];
            }
            Err(waddle_xmpp::mam::MamStorageError::NotOwner { .. }) => {
                demote_exact_room_actor(state, &room_jid, &room_actor).await;
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    resource_constraint_iq_error("Room ownership recently moved; please retry."),
                )];
            }
            Err(waddle_xmpp::mam::MamStorageError::FencingUnavailable { .. }) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    resource_constraint_iq_error(
                        "Room ownership cannot currently be verified; please retry.",
                    ),
                )];
            }
            Err(error) => {
                warn!(room = %room_jid, target = %request.target_id, %error, "Failed to commit moderation event and tombstone");
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        }

        if let Some(stanza_id) = original.stanza_id.as_ref() {
            crate::server::routes::websocket::link_preview_refs::clear_current_message_preview_refs(
                state.deps.app_state.db_pool.global_actor(),
                &room_jid,
                &stanza_id.id,
            )
            .await;
        }

        use waddle_xmpp::stream_management::SmSessionRegistry as _;
        let scrub_target = waddle_xmpp::tombstone::TombstoneTarget::Groupchat {
            stanza_id: request.target_id.clone(),
            room: room_jid.clone(),
        };
        if let Err(error) = state
            .deps
            .protocol
            .sm_session_registry
            .scrub_unacked_for_tombstone(&scrub_target)
            .await
        {
            warn!(room = %room_jid, target = %request.target_id, %error, "XEP-0425 moderation: SM scrub failed after commit");
        }
        if let Err(error) = state
            .deps
            .protocol
            .pending_delivery_storage
            .scrub_for_tombstone(&scrub_target)
            .await
        {
            warn!(room = %room_jid, target = %request.target_id, %error, "XEP-0425 moderation: pending-delivery scrub failed after commit");
        }

        let snapshot = room_actor.ask(GetSnapshot).await.ok();
        // The archive commit and every scrub above can yield long enough for
        // E1 to be replaced. Re-prove the actor's immutable fence only after
        // all of them and after the final snapshot await, immediately before
        // live fanout.
        match fence_room_effects(state, &room_jid, &room_actor).await {
            FencedRoomEffectsOutcome::Authorized => {}
            FencedRoomEffectsOutcome::NotOwner => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    resource_constraint_iq_error("Room ownership recently moved; please retry."),
                )];
            }
            FencedRoomEffectsOutcome::OwnershipUncertain => {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    resource_constraint_iq_error(
                        "Room ownership cannot currently be verified; please retry.",
                    ),
                )];
            }
        }

        let mut frames = Vec::new();
        if let Some(snapshot) = snapshot {
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

        frames.push(build_iq_result_xml(id, response_from, response_to, None));
        return frames;
    }
    Vec::new()
}
