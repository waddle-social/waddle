use super::*;
use super::{
    replay::drain_outbound_into_replay, state::WsConnState, stream_management::sm_show_from_name,
};
use waddle_xmpp::muc::room_actor::LeaveOutcome;
use waddle_xmpp::xep::xep0272::Muji;
use waddle_xmpp::xep::xep0421::OccupantIdentity;

fn muji_reflection_rank(muji: &Muji) -> u8 {
    if muji.is_empty() {
        0
    } else if muji.is_active() {
        2
    } else {
        1
    }
}

/// Broadcast a `<presence type='unavailable'/>` from the leaving
/// occupant's room-nick JID to every remaining occupant when their
/// LAST session for that nick departs.
///
/// XEP-0045 §7.14: the room is responsible for telling remaining
/// occupants that an occupant has left. The wire shape (room/nick
/// `from`, hat-less, `<x xmlns='muc#user'>` with role/affiliation,
/// XEP-0421 `<occupant-id/>`) is produced by the typed
/// `waddle_xmpp::muc::build_leave_presence` builder.
///
/// Used by both the explicit-leave path (`handle_muc_leave`) and the
/// unclean-disconnect path (`cleanup_muc_presence`) so the two cannot
/// drift. Prior to this helper, `cleanup_muc_presence` skipped the
/// broadcast entirely, which manifested as the "N in call" chip
/// staying lit forever after a tab close: other occupants never
/// received the leave signal, so their client-side
/// `$mucCallParticipants` never cleared the stale nick.
pub(crate) fn broadcast_muc_leave_to_remaining(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    outcome: &LeaveOutcome,
) {
    if !outcome.removed_last_session {
        return;
    }
    let from_jid = room_jid
        .clone()
        .with_resource_str(&outcome.nick)
        .unwrap_or_else(|_| sender_jid.clone());
    let sender_bare = sender_jid.to_bare();
    let identity = OccupantIdentity {
        bare_jid: &sender_bare,
        real_jid: Some(sender_jid),
        secret: &state.deps.occupant_id_secret,
    };
    for occupant_jid in &outcome.remaining_occupants {
        let presence = waddle_xmpp::muc::build_leave_presence(
            &from_jid,
            occupant_jid,
            outcome.affiliation,
            false,
            &identity,
        );
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(occupant_jid, Stanza::Presence(presence));
    }
}

/// Broadcast canonical available room/nick presence after one
/// resource's Muji state changes while another same-nick session
/// remains. Sibling Muji state is emitted under the exact full JID
/// that owns it so resource-scoped XEP-0272 preparing state stays
/// attributable.
pub(crate) fn broadcast_muc_muji_clear_to_remaining(
    state: &WebSocketState,
    room_jid: &BareJid,
    leaving_real_jid: &FullJid,
    outcome: &LeaveOutcome,
) {
    if outcome.removed_last_session || !outcome.cleared_muji_state {
        return;
    }
    let from_jid = room_jid
        .clone()
        .with_resource_str(&outcome.nick)
        .unwrap_or_else(|_| {
            outcome
                .remaining_nick_real_jid
                .clone()
                .unwrap_or_else(|| outcome.leaving_room_jid.clone())
        });
    let mut entries = Vec::with_capacity(outcome.remaining_muji_sessions.len() + 1);
    entries.push((leaving_real_jid.clone(), Muji::default()));
    entries.extend(outcome.remaining_muji_sessions.iter().cloned());
    entries.sort_by_key(|(owner_jid, muji)| (muji_reflection_rank(muji), owner_jid.to_string()));
    for occupant_jid in &outcome.remaining_occupants {
        for (owner_jid, muji) in &entries {
            let owner_bare = owner_jid.to_bare();
            let identity = OccupantIdentity {
                bare_jid: &owner_bare,
                real_jid: Some(owner_jid),
                secret: &state.deps.occupant_id_secret,
            };
            let is_self = occupant_jid.to_bare() == owner_bare;
            let mut presence = waddle_xmpp::muc::build_occupant_presence(
                &from_jid,
                occupant_jid,
                outcome.affiliation,
                outcome.role,
                is_self,
                &identity,
            );
            if !muji.is_empty() {
                presence.payloads.push(muji.to_element());
            }
            let _ = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(occupant_jid, Stanza::Presence(presence));
        }
    }
}

/// Clean up MUC room presence when a connection disconnects
/// Public alias for the MUC-presence cleanup used by the SM expired-session
/// janitor in `server::mod`. Thin passthrough so the janitor doesn't need
/// to reimplement the room traversal.
pub async fn cleanup_muc_presence_for_jid(state: &WebSocketState, jid: &FullJid) {
    cleanup_muc_presence(state, jid).await
}

/// If `outcome` represents the final occupant leaving a
/// non-persistent (instant-style) MUC room, dispatch `DestroyRoom`
/// to the room registry so the per-room `RoomActor` is reaped and
/// its entry is removed from `RoomRegistryActor.rooms`.
///
/// Persistent rooms (`RoomConfig.persistent == true`, the default
/// and the shape Waddle channels use) are intentionally left in
/// place: the in-memory actor holds the authoritative caches for
/// affiliations, pin list, and room subject, and a separate
/// re-hydration audit is required before they can be safely
/// evicted. Empty persistent rooms therefore continue to retain
/// their actor across this PR; the residual growth they cause is
/// the next state-inventory-driven follow-up.
///
/// Errors here are logged and swallowed — the leave itself has
/// already succeeded on the wire; failing the response because the
/// eviction round-trip flaked would be a worse user-visible
/// outcome than letting the registry janitor catch this on the
/// next dead-actor sweep.
pub(crate) async fn maybe_evict_empty_room(
    state: &WebSocketState,
    room_jid: &BareJid,
    outcome: &LeaveOutcome,
) {
    if !(outcome.removed_last_session && outcome.occupant_count == 0 && !outcome.is_persistent) {
        return;
    }
    if destroy_room_actor(state, room_jid).await {
        debug!(
            room = %room_jid,
            "Evicted empty non-persistent MUC room from registry"
        );
    } else {
        // Either the room was already absent (race with another leave
        // path) or `destroy_room_actor` logged a registry-ask failure.
        // Either way we don't want to fail the user's leave on this.
        debug!(
            room = %room_jid,
            "Eviction round-trip returned false; registry already cleared or ask failed (logged)"
        );
    }
}

pub(super) async fn cleanup_connection_shutdown(
    state: &WebSocketState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    conn: &mut WsConnState,
    superseded: bool,
) {
    // Short-circuit when this task was superseded: the registry and MUC
    // occupant slots now belong to the newer connection for this FullJid,
    // and any cleanup we do here would clobber the newcomer.
    if superseded {
        return;
    }
    // Note: we deliberately do NOT mirror `conn.phase` Closing into
    // the SM here — the drain loops below need the SM in `Ready`
    // phase so they can run the recipient pass on queued
    // `DeliveryKind::PeerStanza` values. Any explicit Closing
    // transition that needs to lock out late peer dispatches has
    // already happened via the post-`handle_xmpp_frame` mirror in
    // the main loop.

    let Some(jid) = conn.phase.cleanup_jid().cloned() else {
        return;
    };

    let should_detach_for_resume =
        conn.sm_state.is_resumable() && !matches!(conn.phase, ConnectionPhase::Closing { .. });

    if should_detach_for_resume {
        if conn.registry_owner.is_none() {
            if let Some(stream_id) = conn.pending_resume_stream_id.take() {
                if let Err(error) = state
                    .deps
                    .protocol
                    .sm_session_registry
                    .release_claim(&stream_id)
                    .await
                {
                    warn!(stream_id = %stream_id, error = %error, "Failed to release pending SM resume claim during cleanup");
                }
                state.deps.protocol.resumable_sessions.remove(&stream_id);
            }
        }
        let Some(owner) = conn.registry_owner.as_ref() else {
            debug!(jid = %jid, "Skipped SM detach for connection without registry ownership");
            return;
        };
        let presence_state = state
            .deps
            .protocol
            .connection_registry
            .get_presence_state(&jid);
        let Some(entry) = state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&jid, owner)
        else {
            debug!(jid = %jid, "Skipped SM detach for non-owned registry entry");
            return;
        };

        let carbons_enabled = conn.carbons_enabled;
        let presence_available = entry.is_presence_available();
        // First detach drain: snapshot whatever's already in the
        // channel into the unacked queue. No detached_stream_id yet
        // because we haven't decided to store the detached session.
        drain_outbound_into_replay(
            state,
            conn.state_machine.as_mut(),
            &mut conn.sm_state,
            conn.authenticated_session.as_ref(),
            outbound_rx,
            None,
        )
        .await;
        let user_id = conn
            .authenticated_session
            .as_ref()
            .map(|session| session.user_id.to_string())
            .unwrap_or_else(|| jid.to_bare().to_string());
        if let Some(detached) = conn.sm_state.to_detached_session(
            waddle_xmpp::stream_management::DetachedSessionSnapshot {
                user_id,
                jid: jid.clone(),
                carbons_enabled,
                roster_interested: conn.roster_interested,
                presence_available,
                presence_show: presence_state
                    .as_ref()
                    .and_then(|state| state.show.as_deref())
                    .and_then(sm_show_from_name),
                presence_status: presence_state
                    .as_ref()
                    .and_then(|state| state.status.clone()),
                presence_priority: presence_state
                    .as_ref()
                    .map(|state| state.priority)
                    .unwrap_or_else(|| entry.presence_priority()),
            },
        ) {
            let stream_id = detached.stream_id.clone();
            match state
                .deps
                .protocol
                .sm_session_registry
                .store_session(detached)
                .await
            {
                Ok(()) => {
                    if state
                        .deps
                        .protocol
                        .connection_registry
                        .unregister_if_owner(&jid, owner)
                        .is_none()
                    {
                        debug!(jid = %jid, "Removed detached SM session after ownership moved");
                        let _ = state
                            .deps
                            .protocol
                            .sm_session_registry
                            .remove_stored_session_if_unclaimed(&stream_id)
                            .await;
                        return;
                    }
                    outbound_rx.close();
                    // Second detach drain: anything that arrived
                    // between the first drain and the registry
                    // unregister. With the detached session stored,
                    // we record both into the per-connection unacked
                    // queue AND the detached stream's replay buffer.
                    drain_outbound_into_replay(
                        state,
                        conn.state_machine.as_mut(),
                        &mut conn.sm_state,
                        conn.authenticated_session.as_ref(),
                        outbound_rx,
                        Some(&stream_id),
                    )
                    .await;
                    if let Some(session) = conn.authenticated_session.clone() {
                        state
                            .deps
                            .protocol
                            .resumable_sessions
                            .insert(stream_id.clone(), session);
                    }
                    // Remove the routing entry only — the MUC occupant
                    // slot stays. On a successful resume we'll re-register
                    // the same FullJid and presence is preserved.
                    info!(
                        jid = %jid,
                        stream_id = %stream_id,
                        "SM session detached; awaiting resume"
                    );
                }
                Err(err) => {
                    warn!(jid = %jid, error = %err, "Failed to detach SM session; falling back to full cleanup");
                    state.deps.protocol.resumable_sessions.remove(&stream_id);
                    let _ = state
                        .deps
                        .protocol
                        .connection_registry
                        .unregister_if_owner(&jid, owner);
                    // PR #438 review (Copilot): when SM detachment
                    // fails we fall back to a full unregister, so the
                    // caps resource→ver mapping AND any pending
                    // disco#info resolution must be cleared too —
                    // otherwise stale state lingers indefinitely.
                    state.deps.protocol.caps_resolver.drop_resource(&jid);
                    cleanup_muc_presence(state, &jid).await;
                }
            }
            return;
        }
    }

    let removed = conn.registry_owner.as_ref().is_some_and(|owner| {
        state
            .deps
            .protocol
            .connection_registry
            .unregister_if_owner(&jid, owner)
            .is_some()
    });
    if removed {
        // XEP-0115 §6: drop the per-resource caps mapping for this
        // resource. The hash-keyed `CapsCache` itself stays warm so
        // a future session reusing the same `(hash, ver)` short-
        // circuits the disco#info round-trip.
        state.deps.protocol.caps_resolver.drop_resource(&jid);
        info!(jid = %jid, "WebSocket connection unregistered");
        cleanup_muc_presence(state, &jid).await;
    } else {
        debug!(jid = %jid, "Skipped websocket cleanup for non-owned registry entry");
    }
}

async fn cleanup_muc_presence(state: &WebSocketState, jid: &FullJid) {
    for room_jid in list_room_jids(state).await {
        let Some(room_actor) = get_room_actor(state, &room_jid).await else {
            continue;
        };
        let leave_result = room_actor
            .ask(LeaveByRealJid {
                sender_jid: jid.clone(),
            })
            .await;
        // SFU teardown runs *after* the room actor has dropped the
        // session so the MUC's view of the world is the leading edge
        // (the membership gate immediately reports the user as a
        // non-occupant) and the SFU's view is the trailing edge —
        // matches `handle_muc_leave`'s "XMPP says they left, then
        // notify SFU" semantics. Tab close / SM-expiry: the client
        // never sends a graceful `request-leave` on
        // `urn:waddle:muc-call:0`, so without this call the SFU
        // would otherwise hold the participant slot until its own
        // (long) timeout. Idempotent on the SFU side — calling for
        // rooms where the user was never in a call is a no-op.
        super::muc_call_sfu::unregister_participant_from_room(state, &room_jid, jid);
        match leave_result {
            Ok(Some(outcome)) => {
                debug!(
                    room = %room_jid,
                    nick = %outcome.nick,
                    removed_last_session = outcome.removed_last_session,
                    "Removed user from MUC room on disconnect"
                );
                // Tell remaining occupants the user is gone. Without
                // this fan-out, a tab-close / SM-expiry / panic-shed
                // disconnect on a participant who had advertised the
                // `<call xmlns='urn:waddle:muc-call:0'/>` extension
                // leaves the "N in call" chip lit on every other
                // occupant's client. The explicit-leave path
                // (`handle_muc_leave`) calls the same helper, so the
                // wire shape is identical regardless of how the
                // session ended.
                broadcast_muc_leave_to_remaining(state, &room_jid, jid, &outcome);
                broadcast_muc_muji_clear_to_remaining(state, &room_jid, jid, &outcome);
                maybe_evict_empty_room(state, &room_jid, &outcome).await;
            }
            Ok(None) => {}
            Err(error) => {
                warn!(
                    room = %room_jid,
                    jid = %jid,
                    error = ?error,
                    "Failed to remove disconnected user from MUC room"
                );
            }
        }
    }
}

pub(super) async fn cleanup_invalidated_detached_session(
    state: &WebSocketState,
    detached: waddle_xmpp::stream_management::DetachedSession,
    replacement_owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) {
    state
        .deps
        .protocol
        .resumable_sessions
        .remove(&detached.stream_id);
    let replacement_is_current_owner = replacement_owner.is_some_and(|owner| {
        state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&detached.jid, owner)
            .is_some()
    });
    if !replacement_is_current_owner {
        state
            .deps
            .protocol
            .connection_registry
            .unregister(&detached.jid);
        // XEP-0115 §6: clear the resource→ver mapping AND any stuck
        // pending disco#info resolution for this resource so an
        // unresumed detached session doesn't leak indefinitely.
        state
            .deps
            .protocol
            .caps_resolver
            .drop_resource(&detached.jid);
    }
    if detached.presence_available && !replacement_is_current_owner {
        handlers::presence::broadcast_unavailable_for_expired_detached_session(
            state,
            &detached.jid,
        )
        .await;
    }
    cleanup_muc_presence(state, &detached.jid).await;
}

pub(crate) async fn get_room_actor(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Option<ActorRef<RoomActor>> {
    match state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
    {
        Ok(actor) => actor,
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to get room actor");
            None
        }
    }
}

pub(crate) async fn get_or_create_room_actor(
    state: &WebSocketState,
    room_jid: &BareJid,
    config: RoomConfig,
    waddle_id: String,
    channel_id: String,
) -> Option<ActorRef<RoomActor>> {
    match state
        .deps
        .protocol
        .room_registry
        .ask(GetOrCreateRoom {
            room_jid: room_jid.clone(),
            waddle_id,
            channel_id,
            config,
        })
        .await
    {
        Ok(actor) => Some(actor),
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to get or create room actor");
            None
        }
    }
}

pub(crate) async fn list_room_jids(state: &WebSocketState) -> Vec<BareJid> {
    match state.deps.protocol.room_registry.ask(ListRooms).await {
        Ok(rooms) => rooms,
        Err(error) => {
            warn!(error = ?error, "Failed to list room actors");
            Vec::new()
        }
    }
}

pub(crate) async fn is_muc_room_jid(state: &WebSocketState, room_jid: &BareJid) -> bool {
    match state
        .deps
        .protocol
        .room_registry
        .ask(IsMucJid {
            jid: room_jid.clone(),
        })
        .await
    {
        Ok(is_muc_jid) => is_muc_jid,
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to validate MUC JID");
            false
        }
    }
}

pub(crate) async fn destroy_room_actor(state: &WebSocketState, room_jid: &BareJid) -> bool {
    match state
        .deps
        .protocol
        .room_registry
        .ask(DestroyRoom {
            room_jid: room_jid.clone(),
        })
        .await
    {
        Ok(destroyed) => destroyed,
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to destroy room actor");
            false
        }
    }
}

#[cfg(test)]
mod eviction_tests {
    use super::super::tests::create_test_websocket_state;
    use super::*;
    use waddle_xmpp::muc::{
        room_actor::{Join, LeaveByRealJid},
        room_registry_actor::{CreateInstantRoom, CreateRoom, RoomCount},
        RoomConfig,
    };
    use waddle_xmpp_core::{Affiliation, Role};

    fn full_jid(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn room_bare_jid(local: &str) -> BareJid {
        format!("{local}@muc.example.com")
            .parse()
            .expect("bare jid")
    }

    #[tokio::test]
    async fn evicts_empty_instant_room_after_last_leave() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("evict-me");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateInstantRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("create instant room");

        let alice = full_jid("alice@example.com/r1");
        room_actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: alice.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("alice joins");

        let count_before: usize = state
            .deps
            .protocol
            .room_registry
            .ask(RoomCount)
            .await
            .expect("room count");
        assert_eq!(count_before, 1);

        let outcome = room_actor
            .ask(LeaveByRealJid { sender_jid: alice })
            .await
            .expect("leave")
            .expect("outcome present");
        assert!(outcome.removed_last_session);
        assert_eq!(outcome.occupant_count, 0);
        assert!(!outcome.is_persistent);

        maybe_evict_empty_room(&state, &room_jid, &outcome).await;

        let count_after: usize = state
            .deps
            .protocol
            .room_registry
            .ask(RoomCount)
            .await
            .expect("room count");
        assert_eq!(
            count_after, 0,
            "empty non-persistent room must be evicted from the registry"
        );
    }

    #[tokio::test]
    async fn does_not_evict_empty_persistent_room() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("keep-me");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(), // persistent: true
            })
            .await
            .expect("create persistent room");

        let alice = full_jid("alice@example.com/r1");
        room_actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: alice.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("alice joins");

        let outcome = room_actor
            .ask(LeaveByRealJid { sender_jid: alice })
            .await
            .expect("leave")
            .expect("outcome present");
        assert!(outcome.removed_last_session);
        assert_eq!(outcome.occupant_count, 0);
        assert!(
            outcome.is_persistent,
            "default RoomConfig is persistent — outcome must say so"
        );

        maybe_evict_empty_room(&state, &room_jid, &outcome).await;

        let count_after: usize = state
            .deps
            .protocol
            .room_registry
            .ask(RoomCount)
            .await
            .expect("room count");
        assert_eq!(
            count_after, 1,
            "persistent rooms (Waddle channels) must NOT be evicted on empty"
        );
    }

    #[tokio::test]
    async fn does_not_evict_when_other_occupants_remain() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("crowded");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateInstantRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("create instant room");

        let alice = full_jid("alice@example.com/r1");
        let bob = full_jid("bob@example.com/r1");
        for (nick, jid) in [("alice", &alice), ("bob", &bob)] {
            room_actor
                .ask(Join {
                    nick: nick.to_string(),
                    real_jid: jid.clone(),
                    role: Role::Participant,
                    affiliation: Affiliation::Member,
                })
                .await
                .expect("join");
        }

        let outcome = room_actor
            .ask(LeaveByRealJid { sender_jid: alice })
            .await
            .expect("leave")
            .expect("outcome present");
        assert_eq!(outcome.occupant_count, 1);

        maybe_evict_empty_room(&state, &room_jid, &outcome).await;

        let count_after: usize = state
            .deps
            .protocol
            .room_registry
            .ask(RoomCount)
            .await
            .expect("room count");
        assert_eq!(
            count_after, 1,
            "room must remain registered while at least one occupant is present"
        );
    }
}
