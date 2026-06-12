use super::*;

mod access;
mod xml;

pub use access::{get_managed_channel_for_room, parse_room_jid_context};

use access::{resolve_managed_channel_affiliation, server_permission_allowed};
use xml::{
    build_muc_conflict_presence_xml, build_muc_presence_error_xml, build_muc_self_unavailable_xml,
    create_presence_stanza,
};
pub(super) use xml::{build_muc_join_presence_xml, MucJoinPresence};

pub async fn handle_muc_join(
    state: &WebSocketState,
    domain: &str,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    presence_show: Option<crate::notification_activity::NotificationPresenceShow>,
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
                if channel.members_only {
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
                Some(Affiliation::None)
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
                    members_only: channel.members_only,
                    public_room: channel.public_room,
                    moderated: channel.channel_type == "announcement",
                    forum: channel.channel_type == "forum",
                    group_dm: channel.channel_type == "group-dm",
                    // #422: load persisted pin policy so the actor's
                    // snapshot matches the channel's last-saved value
                    // even after eviction.
                    pin_permission: channel.pin_permission,
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
    } else if room_jid
        .node()
        .is_some_and(|node| node.as_str().starts_with("group-dm-"))
    {
        Affiliation::None
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
    let self_muji = join_outcome
        .existing_occupants
        .iter()
        .find(|existing| existing.nick == nick && existing.jid == *sender_jid)
        .and_then(|existing| existing.muji.as_ref());

    info!(room = %room_jid, nick = %nick, occupants = occupant_count, "User joined MUC room");

    // Notification activity ingest (slice 2b): a successful MUC join
    // bumps `(sender_bare, room)` activity. The XEP-0513 `<active/>`
    // filter consults this projection to admit ActiveChannelMention
    // pushes for users who are present in the room. `presence_show` is
    // passed in by the caller (`handle_presence`) when the incoming
    // presence carried a typed `<show/>` token; on first join (or
    // when no `<show/>` is present) we record `None` so the column
    // stays NULL until the user actually broadcasts a state.
    crate::server::routes::interpret::record_presence_available_activity_on_state(
        state,
        &sender_jid.to_bare(),
        room_jid,
        presence_show,
    )
    .await;

    let mut responses = Vec::new();

    // Replay one base occupant presence per nick to the joiner, then
    // append extra same-nick Muji payloads for additional sessions
    // that own call state. Active call membership is nick-level, but
    // XEP-0272 preparing is resource-owned coordination state, so the
    // joiner needs the exact full JID that advertised it.
    let mut replayed_nicks = std::collections::HashSet::new();
    let replay_occupants: Vec<_> = join_outcome
        .existing_occupants
        .iter()
        .filter(|existing| existing.nick != nick)
        .collect();
    for existing in replay_occupants
        .iter()
        .copied()
        .filter(|existing| replayed_nicks.insert(existing.nick.clone()))
    {
        // XEP-0045 §7.2 conformant occupant-list replay, plus the
        // typed `<call xmlns='urn:waddle:muc-call:0'/>` extension when
        // the room actor's snapshot still has an active advertisement
        // for that occupant. Without this enrichment the joiner only
        // sees the chip light up via the NEXT presence update from a
        // call participant, which never happens if the call is steady
        // state — the late joiner is the one we're trying to help.
        responses.push(build_muc_join_presence_xml(MucJoinPresence {
            occupant_id_secret: &state.deps.occupant_id_secret,
            room_jid,
            nick: &existing.nick,
            to_jid: sender_jid,
            affiliation: existing.affiliation,
            role: existing.role,
            real_jid: &existing.jid,
            include_self_status: false,
            muji: existing.muji.as_ref(),
        }));

        for extra in replay_occupants.iter().copied().filter(|candidate| {
            candidate.nick == existing.nick
                && candidate.jid != existing.jid
                && candidate.muji.is_some()
        }) {
            responses.push(build_muc_join_presence_xml(MucJoinPresence {
                occupant_id_secret: &state.deps.occupant_id_secret,
                room_jid,
                nick: &extra.nick,
                to_jid: sender_jid,
                affiliation: extra.affiliation,
                role: extra.role,
                real_jid: &extra.jid,
                include_self_status: false,
                muji: extra.muji.as_ref(),
            }));
        }
    }

    // Broadcast the new occupant's presence to all existing occupants.
    // Non-blocking: a zombied/slow consumer must never stall the join path,
    // which is how "Timed out waiting for self-presence" cascades start.
    // Drop accounting is handled inside `try_send_to` (logs + metrics);
    // per-occupant outcome is discarded here because a missed join
    // presence self-heals via the next MUC presence/probe round-trip.
    if !join_outcome.is_same_bare_multi_session_join && !join_outcome.is_existing_session_rejoin {
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
        muji: self_muji,
    }));

    // Same-account sibling resources share one MUC nick. If a
    // sibling already advertised Muji for this nick, reflect exact
    // per-session snapshots after the new session's own plain
    // self-presence so a refresh/new tab can show "call active on
    // another device" without misattributing `<preparing/>` to the
    // joining resource.
    for existing in join_outcome.existing_occupants.iter().filter(|existing| {
        existing.nick == nick
            && existing.jid.to_bare() == sender_jid.to_bare()
            && existing.jid != *sender_jid
            && existing.muji.is_some()
    }) {
        responses.push(build_muc_join_presence_xml(MucJoinPresence {
            occupant_id_secret: &state.deps.occupant_id_secret,
            room_jid,
            nick,
            to_jid: sender_jid,
            affiliation: existing.affiliation,
            role: existing.role,
            real_jid: &existing.jid,
            include_self_status: true,
            muji: existing.muji.as_ref(),
        }));
    }

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

    // Notification activity ingest (slice 2b): a XEP-0045 leave
    // (explicit `<presence type='unavailable'/>`) is still an
    // engagement signal — the user just acted on the room — so we
    // bump `(sender_bare, room)` activity and clear the persisted
    // `<show/>`. We record before the room-actor teardown so a
    // missing room actor doesn't suppress the activity write; the
    // typed signal happened on the wire regardless.
    crate::server::routes::interpret::record_presence_unavailable_activity_on_state(
        state,
        &sender_jid.to_bare(),
        room_jid,
    )
    .await;

    let Some(room_actor) = get_room_actor(state, room_jid).await else {
        debug!(room = %room_jid, "Room not found for leave");
        // Idempotent on the SFU side — the user could have an SFU
        // participant even when the room actor is gone (process
        // restart, eviction). Tear that down too.
        super::super::super::muc_call_sfu::unregister_participant_from_room(
            state, room_jid, sender_jid,
        );
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
            // No occupant slot to remove, but a stale SFU
            // participant could still exist — clear it.
            super::super::super::muc_call_sfu::unregister_participant_from_room(
                state, room_jid, sender_jid,
            );
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

    // SFU teardown runs after `LeaveByRealJid` so the MUC's
    // authoritative view drops the occupant first; the membership
    // gate immediately reports the user as a non-occupant and any
    // subsequent `request-join` is rejected before the SFU is
    // touched again. Closes the gap where a client leaves the MUC
    // without sending the call-specific `request-leave` — their SFU
    // participant would otherwise linger until LiveKit's timeout.
    super::super::super::muc_call_sfu::unregister_participant_from_room(
        state, room_jid, sender_jid,
    );

    // Broadcast unavailable presence to remaining occupants (non-blocking).
    // Drop accounting is handled inside `try_send_to`. The same helper
    // is used by `cleanup_muc_presence` for unclean disconnects, so
    // both the explicit-leave path and the tab-close path produce the
    // same wire shape.
    super::super::super::cleanup::broadcast_muc_leave_to_remaining(
        state, room_jid, sender_jid, &outcome,
    );
    super::super::super::cleanup::broadcast_muc_muji_clear_to_remaining(
        state, room_jid, sender_jid, &outcome,
    );

    let response = vec![build_muc_self_unavailable_xml(
        state,
        room_jid,
        &outcome.nick,
        sender_jid,
    )];
    super::super::super::cleanup::maybe_evict_empty_room(state, room_jid, &outcome).await;
    response
}
