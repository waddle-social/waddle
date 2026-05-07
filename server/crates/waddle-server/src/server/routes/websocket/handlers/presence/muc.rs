use super::*;

mod access;
mod xml;

pub use access::{get_managed_channel_for_room, parse_room_jid_context};

use access::{resolve_managed_channel_affiliation, server_permission_allowed};
use xml::{
    MucJoinPresence, build_muc_conflict_presence_xml, build_muc_join_presence_xml,
    build_muc_presence_error_xml, build_muc_self_unavailable_xml, create_presence_stanza,
};

pub async fn handle_muc_join(
    state: &WebSocketState,
    domain: &str,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    authenticated_session: &Option<Session>,
) -> Vec<String> {
    info!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC join request");

    let managed_channel = match get_managed_channel_for_room(state, room_jid).await {
        Ok(channel) => channel,
        Err(error) => {
            warn!(room = %room_jid, error = %error, "Failed to resolve managed MUC channel");
            return vec![build_muc_presence_error_xml(
                room_jid,
                nick,
                sender_jid,
                StanzaError::new(
                    ErrorType::Wait,
                    DefinedCondition::InternalServerError,
                    "en",
                    "Failed to resolve managed channel for room.",
                ),
            )];
        }
    };
    let managed_affiliation = if let Some(channel) = managed_channel.as_ref() {
        let Some(session) = authenticated_session else {
            return vec![build_muc_presence_error_xml(
                room_jid,
                nick,
                sender_jid,
                StanzaError::new(
                    ErrorType::Auth,
                    DefinedCondition::NotAuthorized,
                    "en",
                    "Authentication required to join managed channel.",
                ),
            )];
        };
        match resolve_managed_channel_affiliation(state, session, &channel.id).await {
            Ok(Some(Affiliation::Outcast)) => {
                return vec![build_muc_presence_error_xml(
                    room_jid,
                    nick,
                    sender_jid,
                    StanzaError::new(
                        ErrorType::Auth,
                        DefinedCondition::Forbidden,
                        "en",
                        "Banned from managed channel.",
                    ),
                )];
            }
            Ok(Some(affiliation)) => Some(affiliation),
            Ok(None) => {
                return vec![build_muc_presence_error_xml(
                    room_jid,
                    nick,
                    sender_jid,
                    StanzaError::new(
                        ErrorType::Auth,
                        DefinedCondition::RegistrationRequired,
                        "en",
                        "Membership required to join managed channel.",
                    ),
                )];
            }
            Err(()) => {
                return vec![build_muc_presence_error_xml(
                    room_jid,
                    nick,
                    sender_jid,
                    StanzaError::new(
                        ErrorType::Wait,
                        DefinedCondition::InternalServerError,
                        "en",
                        "Failed to resolve managed-channel affiliation.",
                    ),
                )];
            }
        }
    } else {
        None
    };

    let existing_room_actor = get_room_actor(state, room_jid).await;
    let (room_actor, created_instant_room) = match existing_room_actor {
        Some(actor) => (actor, false),
        None => {
            if managed_channel.is_none()
                && !server_permission_allowed(
                    state,
                    authenticated_session.as_ref(),
                    Permission::CreateMuc,
                )
                .await
                .unwrap_or(false)
            {
                return vec![build_muc_presence_error_xml(
                    room_jid,
                    nick,
                    sender_jid,
                    StanzaError::new(
                        ErrorType::Cancel,
                        DefinedCondition::NotAllowed,
                        "en",
                        "Creating new MUC rooms is not permitted for this account.",
                    ),
                )];
            }

            let config = managed_channel
                .as_ref()
                .map(|channel| RoomConfig {
                    name: channel.name.clone(),
                    description: channel.description.clone(),
                    members_only: true,
                    moderated: channel.channel_type == "announcement",
                    forum: channel.channel_type == "forum",
                    ..Default::default()
                })
                .unwrap_or_else(|| RoomConfig {
                    name: room_jid
                        .node()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "Room".to_string()),
                    members_only: false,
                    ..Default::default()
                });

            let (waddle_id, channel_id) = managed_channel
                .as_ref()
                .map(|channel| {
                    let (waddle_id, _) = parse_room_jid_context(room_jid);
                    (waddle_id, channel.id.clone())
                })
                .unwrap_or_else(|| parse_room_jid_context(room_jid));

            let Some(actor) =
                get_or_create_room_actor(state, room_jid, config, waddle_id, channel_id).await
            else {
                return vec![];
            };
            (actor, managed_channel.is_none())
        }
    };

    let effective_affiliation = if created_instant_room {
        Affiliation::Owner
    } else if let Some(affiliation) = managed_affiliation {
        affiliation
    } else {
        Affiliation::Member
    };

    let join_outcome = match room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender_jid.clone(),
            nick: nick.to_string(),
            effective_affiliation,
            local_domain: domain.to_string(),
        })
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let nick_collision = matches!(
                &error,
                kameo::error::SendError::HandlerError(
                    waddle_xmpp::muc::room_actor::RoomActorError::NickAlreadyInUse(_)
                )
            );
            if nick_collision {
                warn!(
                    room = %room_jid,
                    nick = %nick,
                    sender = %sender_jid,
                    "MUC nick collision; returning conflict"
                );
                return vec![build_muc_conflict_presence_xml(room_jid, nick, sender_jid)];
            }
            warn!(room = %room_jid, nick = %nick, error = ?error, "Failed to join MUC room");
            return vec![];
        }
    };

    let occupant_count = join_outcome.occupant_count;

    info!(room = %room_jid, nick = %nick, occupants = occupant_count, "User joined MUC room");

    let mut responses = Vec::new();

    // Replay one occupant presence per nick to the joiner. Same-bare multi-session
    // joins must not turn into duplicate room occupants on the wire.
    let mut replayed_nicks = std::collections::HashSet::new();
    for existing in join_outcome
        .existing_occupants
        .iter()
        .filter(|existing| existing.nick != nick)
        .filter(|existing| replayed_nicks.insert(existing.nick.clone()))
    {
        responses.push(build_muc_join_presence_xml(MucJoinPresence {
            occupant_id_secret: &state.deps.occupant_id_secret,
            room_jid,
            nick: &existing.nick,
            to_jid: sender_jid,
            affiliation: existing.affiliation,
            role: existing.role,
            real_jid: &existing.jid,
            include_self_status: false,
        }));
    }

    // Broadcast the new occupant's presence to all existing occupants.
    // Non-blocking: a zombied/slow consumer must never stall the join path,
    // which is how "Timed out waiting for self-presence" cascades start.
    // Drop accounting is handled inside `try_send_to` (logs + metrics);
    // per-occupant outcome is discarded here because a missed join
    // presence self-heals via the next MUC presence/probe round-trip.
    if !join_outcome.is_same_bare_multi_session_join {
        for existing in &join_outcome.existing_occupants {
            let presence_stanza = create_presence_stanza(
                state,
                room_jid,
                nick,
                sender_jid,
                &existing.jid,
                join_outcome.new_occupant_affiliation,
                join_outcome.new_occupant_role,
            );
            let stanza = Stanza::Presence(presence_stanza);
            let _outcome = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(&existing.jid, stanza);
        }
    }

    // Send self-presence to the joining user (with status code 110)
    responses.push(build_muc_join_presence_xml(MucJoinPresence {
        occupant_id_secret: &state.deps.occupant_id_secret,
        room_jid,
        nick,
        to_jid: sender_jid,
        affiliation: join_outcome.new_occupant_affiliation,
        role: join_outcome.new_occupant_role,
        real_jid: sender_jid,
        include_self_status: true,
    }));

    // XEP-0045 §7.2.15 historical room subject. The typed builder
    // produces the conformant envelope: nick-form `from` + `<delay/>`
    // + XEP-0421 `<occupant-id/>` when a setter is known, or bare-from
    // empty `<subject/>` for a never-set room (matching the established
    // resolution of XEP-0421 §3 vs §7.2.15 on never-set rooms).
    let subject_msg = build_subject_message(
        room_jid,
        sender_jid,
        join_outcome.subject_state.as_ref(),
        &state.deps.occupant_id_secret,
    );
    responses.push(stanza_to_xml(&Stanza::Message(subject_msg)));

    responses
}

/// Handle MUC room leave
pub async fn handle_muc_leave(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
) -> Vec<String> {
    info!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave request");

    let Some(room_actor) = get_room_actor(state, room_jid).await else {
        debug!(room = %room_jid, "Room not found for leave");
        return vec![build_muc_self_unavailable_xml(
            state, room_jid, nick, sender_jid,
        )];
    };

    let outcome = match room_actor
        .ask(LeaveByRealJid {
            sender_jid: sender_jid.clone(),
        })
        .await
    {
        Ok(Some(outcome)) => outcome,
        Ok(None) => {
            debug!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave for absent occupant");
            return vec![build_muc_self_unavailable_xml(
                state, room_jid, nick, sender_jid,
            )];
        }
        Err(error) => {
            warn!(room = %room_jid, nick = %nick, sender = %sender_jid, error = ?error, "Failed to leave MUC room");
            return vec![build_muc_self_unavailable_xml(
                state, room_jid, nick, sender_jid,
            )];
        }
    };

    // Broadcast unavailable presence to remaining occupants (non-blocking).
    // Drop accounting is handled inside `try_send_to`.
    if outcome.removed_last_session {
        for occupant_jid in &outcome.remaining_occupants {
            let from_jid = room_jid
                .clone()
                .with_resource_str(&outcome.nick)
                .unwrap_or_else(|_| sender_jid.clone());
            let sender_bare = sender_jid.to_bare();
            let presence = waddle_xmpp::muc::build_leave_presence(
                &from_jid,
                occupant_jid,
                Affiliation::Member,
                false,
                &waddle_xmpp::xep::xep0421::OccupantIdentity {
                    bare_jid: &sender_bare,
                    real_jid: Some(sender_jid),
                    secret: &state.deps.occupant_id_secret,
                },
            );
            let stanza = Stanza::Presence(presence);
            let _outcome = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(occupant_jid, stanza);
        }
    }

    vec![build_muc_self_unavailable_xml(
        state,
        room_jid,
        &outcome.nick,
        sender_jid,
    )]
}
