//! Shared MUC Muji-presence cleanup for SFU-driven and client-driven
//! call departures.
//!
//! Both the LiveKit webhook bridge and the Muji Jingle
//! `session-terminate` path need to clear the room actor's per-session
//! Muji state and broadcast the XEP-0272 leave marker to remaining
//! occupants. Centralising the logic keeps the wire shape identical.

use crate::call_teardown_outbox::CallTeardownIntent;
use crate::call_teardown_outbox::CallTeardownOutboxError;
use crate::call_teardown_outbox::TeardownTarget;
use jid::{BareJid, FullJid};
use tracing::{debug, warn};
use waddle_sfu::{ObservedCallSids, TeardownDisposition};
use waddle_xmpp::muc::build_occupant_presence;
use waddle_xmpp::muc::room_actor::{
    ClearMujiPresence, ClearMujiPresenceOutcome, GetSnapshot, MujiPresenceUpdateOutcome,
};
use waddle_xmpp::telemetry::call::increment_call_teardown_stale_dropped;
use waddle_xmpp::xep::xep0272::Muji;
use waddle_xmpp::xep::xep0421::OccupantIdentity;
use waddle_xmpp_core::Stanza;

use super::websocket::{
    get_room_actor_result, note_participant_left_from_webhook,
    observe_participant_sids_from_webhook, WebSocketState,
};

/// The disposition of a webhook-driven side effect.  A retryable result keeps
/// the delivery in progress so LiveKit redelivers it; permanent input or
/// already-gone-room cases are acknowledged after their warning is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebhookEffectOutcome {
    Completed,
    Stale,
    Retryable(&'static str),
    Permanent(&'static str),
}

/// The SFU ghost sweep has already removed its exact registration. This path
/// only repairs the actor advertisement; repeating SFU departure bookkeeping
/// after an await could revoke a newly published successor's credentials.
pub(crate) async fn clear_muji_presence_only(
    state: &WebSocketState,
    room_jid: &BareJid,
    full_jid: &FullJid,
) -> WebhookEffectOutcome {
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        return WebhookEffectOutcome::Retryable("sfu_unavailable");
    };
    let Ok(call_id) = waddle_sfu::CallId::try_new(room_jid.to_string()) else {
        return WebhookEffectOutcome::Permanent("invalid_call_id");
    };
    let identity = waddle_sfu::Identity::from_jid(full_jid.clone());
    let registration_exists = || sfu.has_call_participant(&call_id, &identity);
    if registration_exists() {
        return WebhookEffectOutcome::Stale;
    }
    let Some(executor) = state.deps.protocol.call_teardown_executor.as_ref() else {
        return WebhookEffectOutcome::Retryable("livekit_executor_unavailable");
    };
    let connected = tokio::time::timeout(
        waddle_xmpp::muc::ROOM_REGISTRY_REPLY_TIMEOUT,
        executor.live_room_occupancy(&call_id),
    )
    .await;
    if registration_exists() {
        return WebhookEffectOutcome::Stale;
    }
    let provider_room_empty = match connected {
        Ok(Ok(occupancy)) => {
            if occupancy
                .waddle
                .iter()
                .any(|participant| participant.identity == identity)
            {
                return WebhookEffectOutcome::Stale;
            }
            // Foreign recorders do not own XMPP call-thread participation.
            // Other live Waddle media does, even when this owner's cache is cold.
            occupancy.waddle.is_empty()
        }
        Ok(Err(_)) | Err(_) => return WebhookEffectOutcome::Retryable("provider_presence_unknown"),
    };
    let actor = match get_room_actor_result(state, room_jid).await {
        Ok(Some(actor)) => actor,
        Ok(None) => return WebhookEffectOutcome::Retryable("room_actor_absent"),
        Err(_) => return WebhookEffectOutcome::Retryable("room_registry_lookup_failed"),
    };
    if registration_exists() {
        return WebhookEffectOutcome::Stale;
    }
    let snapshot = match actor
        .ask(GetSnapshot)
        .reply_timeout(waddle_xmpp::muc::ROOM_REGISTRY_REPLY_TIMEOUT)
        .await
    {
        Ok(snapshot) => snapshot,
        Err(_) => return WebhookEffectOutcome::Retryable("room_actor_snapshot_failed"),
    };
    if registration_exists() {
        return WebhookEffectOutcome::Stale;
    }
    if snapshot.room.find_occupant_by_real_jid(full_jid).is_none() {
        if !provider_room_empty {
            return WebhookEffectOutcome::Completed;
        }
        // Preserve caller responsibility as well as the completion-only retry:
        // enqueue of that retry can itself fail. Repeated actor-only cleanup
        // is idempotent and never repeats any SFU departure bookkeeping.
        return super::call_thread_end::maybe_broadcast_call_thread_ended(state, room_jid).await;
    }
    let Some(occupant) = snapshot.room.session_generation(full_jid) else {
        return WebhookEffectOutcome::Retryable("occupancy_generation_unavailable");
    };
    if registration_exists() {
        return WebhookEffectOutcome::Stale;
    }
    let clear = actor
        .ask(ClearMujiPresence {
            sender_jid: full_jid.clone(),
            occupant: Some(occupant),
        })
        .reply_timeout(waddle_xmpp::muc::ROOM_REGISTRY_REPLY_TIMEOUT)
        .await;
    // An equal-generation publication may race the actor mailbox. Its
    // advertisement can be reasserted by the client, but the obsolete ghost
    // action must never broadcast a leave, end the thread or touch its token.
    if registration_exists() {
        return WebhookEffectOutcome::Stale;
    }
    match clear {
        Ok(Some(ClearMujiPresenceOutcome::Updated(outcome))) => {
            broadcast_muji_clear(state, room_jid, full_jid, &outcome);
        }
        Ok(Some(ClearMujiPresenceOutcome::Superseded)) => return WebhookEffectOutcome::Stale,
        Ok(None) => {}
        Err(_) => return WebhookEffectOutcome::Retryable("room_actor_clear_failed"),
    }
    // The existing completion helper rechecks SFU state under its room lock.
    // Its baseline asynchronous completion window remains unchanged.
    if !provider_room_empty {
        return WebhookEffectOutcome::Completed;
    }
    super::call_thread_end::maybe_broadcast_call_thread_ended(state, room_jid).await
}

pub(crate) async fn enqueue_muji_presence_clear_only(
    state: &WebSocketState,
    room_jid: &BareJid,
    departed: &FullJid,
) -> Result<(), CallTeardownOutboxError> {
    let call_id = waddle_sfu::CallId::try_new(room_jid.to_string())?;
    let intent = CallTeardownIntent {
        call_id,
        target: TeardownTarget::MujiPresenceClearOnly {
            room_jid: room_jid.clone(),
            departed: departed.clone(),
        },
        generation: None,
        occupant: None,
        unbound_occupant: waddle_sfu::UnboundOccupantPolicy::Keep,
        room_sid: None,
        session: None,
    };
    if let Err(error) = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(intent.clone())
        .await
    {
        warn!(room = %room_jid, %error, "failed to persist actor-only Muji clear; retaining the same cleanup responsibility");
        state
            .deps
            .protocol
            .call_teardown_persistence
            .retry_batch(vec![intent]);
        return Err(error);
    }
    Ok(())
}

/// Clear `full_jid`'s Muji advertisement in `room_jid` and broadcast
/// the leave marker to remaining occupants. Idempotent: a participant
/// already cleared via presence-update returns `Ok(None)` and skips
/// the broadcast.
pub(crate) async fn clear_muji_presence_for_departure(
    state: &WebSocketState,
    room_jid: &BareJid,
    full_jid: &FullJid,
    observed_sids: Option<&ObservedCallSids>,
    occupant: Option<waddle_xmpp_core::OccupancySessionGeneration>,
    unbound: waddle_sfu::UnboundOccupantPolicy,
    session: Option<&waddle_sfu::SessionBinding>,
) -> WebhookEffectOutcome {
    debug!(
        room = %room_jid,
        identity = %full_jid,
        "Clearing Muji presence for departed participant"
    );

    if matches!(
        observe_participant_sids_from_webhook(state, room_jid, full_jid, observed_sids),
        Some(waddle_sfu::SidObservationDisposition::StaleSid)
    ) {
        return WebhookEffectOutcome::Stale;
    }

    // #1608 (PR #1626 review round 4): when the departure came from a
    // signaling terminate, a live registration bound to a DIFFERENT
    // session proves this cleanup was superseded by a rejoin — the
    // actor-side advertisement clear, the SFU bookkeeping, and the
    // leave broadcast below all belong to the OLD session and must not
    // touch the new one. Webhook-driven callers pass no session and
    // keep their membership-scoped semantics.
    if session_superseded(state, room_jid, full_jid, session) {
        // No metric here: callers already account for `Stale` outcomes
        // (the outbox drain counts stale_dropped itself), and counting
        // at both layers double-counts one skipped cleanup.
        return WebhookEffectOutcome::Stale;
    }

    let actor = match get_room_actor_result(state, room_jid).await {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            warn!(
                room = %room_jid,
                identity = %full_jid,
                "MUC room actor is absent during LiveKit departure cleanup; queueing owner-gated Muji clear"
            );
            if enqueue_muji_presence_clear(
                state,
                room_jid,
                full_jid,
                observed_sids,
                occupant,
                unbound,
                session,
            )
            .await
            .is_err()
            {
                return WebhookEffectOutcome::Retryable("teardown_outbox_enqueue_failed");
            }
            record_participant_left(
                state,
                room_jid,
                full_jid,
                observed_sids,
                occupant,
                unbound,
                session,
            );
            return WebhookEffectOutcome::Completed;
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                identity = %full_jid,
                error = %error,
                "failed to resolve MUC room actor during LiveKit departure cleanup"
            );
            return WebhookEffectOutcome::Retryable("room_registry_lookup_failed");
        }
    };
    // Re-checked after the awaited actor lookup, immediately before
    // the destructive ask (#1608): the actor mailbox itself is the
    // remaining residue — closing it would require the room actor to
    // learn Jingle session bindings; the authoritative SFU registry
    // leg below is fully atomic instead, and a wrongly-cleared
    // advertisement is re-asserted by the client's next Muji presence
    // update.
    if session_superseded(state, room_jid, full_jid, session) {
        return WebhookEffectOutcome::Stale;
    }
    let outcome = match actor
        .ask(ClearMujiPresence {
            sender_jid: full_jid.clone(),
            occupant,
        })
        .await
    {
        Ok(Some(ClearMujiPresenceOutcome::Updated(outcome))) => *outcome,
        Ok(Some(ClearMujiPresenceOutcome::Superseded)) => {
            return WebhookEffectOutcome::Stale;
        }
        Ok(None) => {
            debug!(
                room = %room_jid,
                identity = %full_jid,
                "Participant not in MUC actor; SFU registry cleanup only"
            );
            record_participant_left(
                state,
                room_jid,
                full_jid,
                observed_sids,
                occupant,
                unbound,
                session,
            );
            return super::call_thread_end::maybe_broadcast_call_thread_ended(state, room_jid)
                .await;
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                identity = %full_jid,
                error = ?error,
                "room actor rejected Muji clear; asking LiveKit to retry"
            );
            return WebhookEffectOutcome::Retryable("room_actor_ask_failed");
        }
    };

    broadcast_muji_clear(state, room_jid, full_jid, &outcome);
    record_participant_left(
        state,
        room_jid,
        full_jid,
        observed_sids,
        occupant,
        unbound,
        session,
    );
    super::call_thread_end::maybe_broadcast_call_thread_ended(state, room_jid).await
}

async fn enqueue_muji_presence_clear(
    state: &WebSocketState,
    room_jid: &BareJid,
    full_jid: &FullJid,
    observed_sids: Option<&ObservedCallSids>,
    occupant: Option<waddle_xmpp_core::OccupancySessionGeneration>,
    unbound: waddle_sfu::UnboundOccupantPolicy,
    session: Option<&waddle_sfu::SessionBinding>,
) -> Result<(), CallTeardownOutboxError> {
    let call_id = match waddle_sfu::CallId::try_new(room_jid.to_string()) {
        Ok(call_id) => call_id,
        Err(error) => {
            warn!(
                room = %room_jid,
                identity = %full_jid,
                %error,
                "could not model absent-room Muji cleanup as a teardown intent"
            );
            return Ok(());
        }
    };
    let intent = CallTeardownIntent {
        call_id,
        target: TeardownTarget::MujiPresenceClear {
            room_jid: room_jid.clone(),
            departed: full_jid.clone(),
            participant_sid: observed_sids.and_then(|sids| sids.participant_sid.clone()),
        },
        generation: None,
        unbound_occupant: unbound,
        room_sid: observed_sids.and_then(|sids| sids.room_sid.clone()),
        occupant,
        // Carried through from the producer when the departure came
        // from a signaling terminate (#1608); webhook-driven callers
        // have no session evidence and pass None.
        session: session.cloned(),
    };
    let store = &state.deps.protocol.call_teardown_outbox;
    let persistence = &state.deps.protocol.call_teardown_persistence;
    if let Err(error) = store.enqueue(intent.clone()).await {
        warn!(
            room = %room_jid,
            identity = %full_jid,
            %error,
            "failed to persist absent-room Muji clear; keeping webhook retryable and retrying asynchronously"
        );
        persistence.retry_batch(vec![intent]);
        return Err(error);
    }
    Ok(())
}

fn record_participant_left(
    state: &WebSocketState,
    room_jid: &BareJid,
    full_jid: &FullJid,
    observed_sids: Option<&ObservedCallSids>,
    occupant: Option<waddle_xmpp_core::OccupancySessionGeneration>,
    unbound: waddle_sfu::UnboundOccupantPolicy,
    session: Option<&waddle_sfu::SessionBinding>,
) {
    // #1703: a connection-originated clear names its occupant generation and
    // the registry decides ATOMICALLY under its call-entry guard: a
    // registration bound to a DIFFERENT generation (a replacement connection,
    // same sid or not) or to a different sid (the same connection re-initiated
    // while this clear was in flight, #1608) is untouched; a matching one is
    // cleared; an unbound
    // (restored) one follows `unbound` — the live connection's own clear
    // tears it down, a durable redrive keeps it. A silent mismatch is
    // correct here for the same reason as the sid path below: the
    // presence-side outcome was already decided by the actor.
    if let Some(occupant) = occupant {
        let _ = super::websocket::muc_call_sfu::note_participant_left_for_occupant(
            state,
            room_jid,
            full_jid,
            observed_sids,
            occupant,
            unbound,
            session,
        );
        return;
    }
    match session {
        // #1608: the signaling-driven cleanup removes the registration
        // only when its binding still accepts the producing session —
        // check and removal are one atomic registry operation, so a
        // rebind racing the awaits above cannot lose the NEW session's
        // bookkeeping. A mismatch is silent here: the presence-side
        // outcome was already decided by the actor, and the metric
        // accounting belongs to the callers.
        Some(_) => {
            let _ = super::websocket::muc_call_sfu::note_participant_left_for_session(
                state,
                room_jid,
                full_jid,
                observed_sids,
                session,
            );
        }
        None => {
            if matches!(
                note_participant_left_from_webhook(state, room_jid, full_jid, observed_sids),
                Some(TeardownDisposition::StaleSid)
            ) {
                increment_call_teardown_stale_dropped();
            }
        }
    }
}

/// `true` when a signaling session was supplied and `full_jid`'s live
/// registration is bound to a DIFFERENT session — the cleanup was
/// superseded by a rejoin (#1608).
fn session_superseded(
    state: &WebSocketState,
    room_jid: &BareJid,
    full_jid: &FullJid,
    session: Option<&waddle_sfu::SessionBinding>,
) -> bool {
    let (Some(session), Some(sfu)) = (session, state.deps.protocol.sfu.as_ref()) else {
        return false;
    };
    let Ok(call_id) = waddle_sfu::CallId::try_new(room_jid.to_string()) else {
        return false;
    };
    let identity = waddle_sfu::Identity::from_jid(full_jid.clone());
    sfu.participant_session_binding(&call_id, &identity)
        .is_some_and(|bound| &bound != session)
}

/// Broadcast a server-originated Muji-presence clear to every remaining
/// occupant of the room.
pub(crate) fn broadcast_muji_clear(
    state: &WebSocketState,
    room_jid: &BareJid,
    leaving_real_jid: &FullJid,
    outcome: &MujiPresenceUpdateOutcome,
) {
    let from_room_jid = room_jid
        .clone()
        .with_resource_str(&outcome.update.sender_nick)
        .unwrap_or_else(|_| leaving_real_jid.clone());

    let mut entries: Vec<(FullJid, Option<Muji>)> =
        Vec::with_capacity(outcome.session_mujis.len() + 1);
    entries.push((leaving_real_jid.clone(), None));
    for (owner, muji) in &outcome.session_mujis {
        if owner == leaving_real_jid {
            continue;
        }
        entries.push((owner.clone(), Some(muji.clone())));
    }

    for recipient in &outcome.update.recipients {
        for (owner_jid, muji) in &entries {
            let owner_bare = owner_jid.to_bare();
            let identity = OccupantIdentity {
                bare_jid: &owner_bare,
                real_jid: Some(owner_jid),
                secret: &state.deps.occupant_id_secret,
                hats: &state.deps.app_state.server_hats,
            };
            let is_self = recipient.to_bare() == owner_bare;
            let mut presence = build_occupant_presence(
                &from_room_jid,
                recipient,
                outcome.update.sender_affiliation,
                outcome.update.sender_role,
                waddle_xmpp::muc::MucPresenceStatus::new(is_self, false),
                &identity,
            );
            if let Some(muji_ref) = muji {
                if !muji_ref.is_empty() {
                    presence.payloads.push(muji_ref.to_element());
                }
            }
            let _ = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(recipient, Stanza::Presence(presence));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{clear_muji_presence_for_departure, WebhookEffectOutcome};
    use crate::server::routes::call_thread_end::{
        maybe_broadcast_call_thread_ended, remove_completed_call_thread,
    };
    use crate::server::routes::websocket::handlers::presence::handle_muc_join;
    use crate::server::routes::websocket::tests::{
        create_test_server_owner_session, create_test_websocket_state_with_calls,
        create_test_websocket_state_with_sfu, snapshot_room, EmptyCallAdmin, RecordingSfu,
    };
    use crate::server::routes::websocket::ActiveCallThread;
    use jid::{BareJid, FullJid};

    fn empty_ghost_probe_executor() -> waddle_sfu::LiveKitTeardownExecutor {
        let config = waddle_sfu::SfuConfig {
            api_key: waddle_sfu::ApiKey::new("APItestkey"),
            api_secret: waddle_sfu::ApiSecret::from_text("super-secret-secret-32-bytes-min")
                .expect("API secret"),
            webhook_secret: waddle_sfu::ApiSecret::from_text("super-secret-secret-32-bytes-min")
                .expect("webhook secret"),
            ws_url: waddle_sfu::WebsocketUrl::try_new("wss://livekit.test/".parse().expect("URL"))
                .expect("WebSocket URL"),
            turn_host: waddle_sfu::TurnHost::new("turn.test"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: waddle_sfu::TurnSharedSecret::from_text("turn-secret"),
            token_ttl: chrono::Duration::seconds(3600),
            turn_ttl: chrono::Duration::seconds(3600),
        };
        waddle_sfu::LiveKitSfu::with_admin(config, Arc::new(EmptyCallAdmin)).teardown_executor()
    }

    struct PauseGhostLookup {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl kameo::message::Message<PauseGhostLookup>
        for waddle_xmpp::muc::room_registry_actor::RoomRegistryActor
    {
        type Reply = ();

        async fn handle(
            &mut self,
            message: PauseGhostLookup,
            _context: &mut kameo::message::Context<Self, Self::Reply>,
        ) -> Self::Reply {
            message.entered.notify_one();
            message.release.notified().await;
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ghost_actor_cleanup_preserves_publication_during_a_paused_actor_lookup() {
        let mut state = create_test_websocket_state_with_calls().await;
        Arc::get_mut(&mut state)
            .expect("unique state")
            .deps
            .protocol
            .call_teardown_executor = Some(empty_ghost_probe_executor());
        let session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "ghost-lookup-race@muc.example.com".parse().expect("room");
        let alice: FullJid = "alice@example.com/web".parse().expect("JID");
        let identity = waddle_sfu::Identity::from_jid(alice.clone());
        let call = waddle_sfu::CallId::try_new(room_jid.to_string()).expect("call");
        let sfu = state.deps.protocol.sfu.as_ref().expect("real SFU");
        let publish = |generation, name: &str| {
            sfu.issue_join_token_with_session(
                &call,
                &identity,
                waddle_sfu::MediaCapabilities::direct_call_peer(),
                &waddle_sfu::SessionBinding::try_new(name).expect("session"),
                generation,
                sfu.participant_registration_expectation(&call, &identity)
                    .expect("expectation"),
            )
            .expect("mint")
            .expect("publish")
        };
        let old_token = publish(
            waddle_xmpp_core::OccupancySessionGeneration::mint(),
            "ghost-session",
        );
        sfu.note_participant_left(&call, &identity, None);
        assert!(sfu.is_revoked(&old_token.jti));
        assert!(
            !sfu.has_call_participant(&call, &identity),
            "SFU cleanup precedes actor repair"
        );
        let successor = waddle_xmpp_core::OccupancySessionGeneration::mint();
        handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            crate::server::routes::websocket::handlers::presence::MucJoinConnectionContext {
                occupancy_session: successor,
                authenticated_session: Some(&session),
                registry_owner: None,
            },
        )
        .await;
        super::get_room_actor_result(state.as_ref(), &room_jid)
            .await
            .expect("lookup")
            .expect("actor")
            .ask(waddle_xmpp::muc::room_actor::UpsertMujiPresence {
                sender_jid: alice.clone(),
                occupant: None,
                muji: waddle_xmpp::xep::xep0272::Muji::with_contents(vec![
                    waddle_xmpp::xep::xep0272::MujiContent::new(
                        "audio",
                        waddle_xmpp::xep::xep0272::Creator::Initiator,
                        waddle_xmpp::xep::xep0167::MediaKind::Audio,
                    ),
                ]),
            })
            .await
            .expect("Muji update")
            .expect("occupant update");
        state
            .deps
            .protocol
            .call_threads
            .insert(room_jid.clone(), active_call_thread(alice.to_bare()));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let registry = state.deps.protocol.room_registry.clone();
        let pause = tokio::spawn({
            let entered = entered.clone();
            let release = release.clone();
            async move {
                registry
                    .ask(PauseGhostLookup { entered, release })
                    .await
                    .expect("paused registry")
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("registry paused");
        let mut cleanup = Box::pin(super::clear_muji_presence_only(
            state.as_ref(),
            &room_jid,
            &alice,
        ));
        assert!(
            futures::poll!(cleanup.as_mut()).is_pending(),
            "cleanup passes its absent-SFU check and waits for the registry"
        );
        let token = publish(successor, "successor-session");
        release.notify_one();
        assert_eq!(cleanup.await, super::WebhookEffectOutcome::Stale);
        pause.await.expect("registry resumed");
        assert!(!sfu.is_revoked(&token.jti));
        assert_eq!(
            sfu.participant_occupant_session(&call, &identity),
            Some(successor)
        );
        assert!(snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .muji_for_session("alice", &alice)
            .is_some());
        assert!(state.deps.protocol.call_threads.contains_key(&room_jid));
    }
    fn active_call_thread(initiator: BareJid) -> ActiveCallThread {
        ActiveCallThread {
            anchor_origin_id: "anchor-origin-id".to_owned(),
            initiator,
            media: waddle_xmpp::xep::CallThreadMedia::audio_only(),
            started: chrono::Utc::now() - chrono::Duration::minutes(5),
            thread_id: "call-thread-id".to_owned(),
        }
    }

    #[tokio::test]
    async fn absent_room_preserves_confirmed_departure_policy() {
        let state = create_test_websocket_state_with_sfu(Arc::new(RecordingSfu::default())).await;
        let room_jid: BareJid = "confirmed-departure@muc.example.com"
            .parse()
            .expect("room jid");
        let full_jid: FullJid = "alice@example.com/web".parse().expect("full jid");
        let occupant = waddle_xmpp_core::OccupancySessionGeneration::mint();
        let session = waddle_sfu::SessionBinding::try_new("departed-session").expect("session");

        let outcome = clear_muji_presence_for_departure(
            state.as_ref(),
            &room_jid,
            &full_jid,
            None,
            Some(occupant),
            waddle_sfu::UnboundOccupantPolicy::TearDown,
            Some(&session),
        )
        .await;

        assert_eq!(outcome, WebhookEffectOutcome::Completed);
        let jobs = state
            .deps
            .protocol
            .call_teardown_outbox
            .claim_due(8)
            .await
            .expect("queued clear");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].intent.occupant, Some(occupant));
        assert_eq!(jobs[0].intent.session.as_ref(), Some(&session));
        assert_eq!(
            jobs[0].intent.unbound_occupant,
            waddle_sfu::UnboundOccupantPolicy::TearDown
        );
    }

    #[tokio::test]
    async fn absent_room_outbox_enqueue_failure_is_retryable() {
        let state = create_test_websocket_state_with_sfu(Arc::new(RecordingSfu::default())).await;
        let db = state.deps.app_state.db_pool.global();
        let connection = db.guard().await.expect("db");
        connection
            .execute("DROP TABLE call_teardown_outbox", ())
            .await
            .expect("drop outbox table");

        let room_jid: BareJid = "enqueue-failure@muc.example.com".parse().expect("room jid");
        let full_jid: FullJid = "alice@example.com/web".parse().expect("full jid");

        let outcome = clear_muji_presence_for_departure(
            state.as_ref(),
            &room_jid,
            &full_jid,
            None,
            None,
            waddle_sfu::UnboundOccupantPolicy::TearDown,
            None,
        )
        .await;

        assert_eq!(
            outcome,
            WebhookEffectOutcome::Retryable("teardown_outbox_enqueue_failed")
        );
    }

    #[tokio::test]
    async fn failed_call_thread_end_broadcast_is_retryable_and_retains_entry() {
        let state = create_test_websocket_state_with_sfu(Arc::new(RecordingSfu::default())).await;
        let room_jid: BareJid = "missing-call-room@muc.example.com"
            .parse()
            .expect("room jid");
        let initiator: BareJid = "alice@example.com".parse().expect("initiator jid");
        state
            .deps
            .protocol
            .call_threads
            .insert(room_jid.clone(), active_call_thread(initiator));

        let outcome = maybe_broadcast_call_thread_ended(state.as_ref(), &room_jid).await;

        assert_eq!(
            outcome,
            WebhookEffectOutcome::Retryable("call_thread_end_broadcast_failed")
        );
        assert!(
            state.deps.protocol.call_threads.contains_key(&room_jid),
            "a failed ended fastening must retain the active entry for redelivery"
        );
    }

    #[tokio::test]
    async fn successful_call_thread_end_broadcast_removes_entry() {
        let state = create_test_websocket_state_with_sfu(Arc::new(RecordingSfu::default())).await;
        let room_jid: BareJid = "live-call-room@muc.example.com".parse().expect("room jid");
        let initiator: FullJid = "alice@example.com/web".parse().expect("initiator jid");
        let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let initiator_occupancy_session = waddle_xmpp_core::OccupancySessionGeneration::mint();
        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &initiator,
            "alice",
            None,
            crate::server::routes::websocket::handlers::presence::MucJoinConnectionContext {
                occupancy_session: initiator_occupancy_session,
                authenticated_session: Some(&owner_session),
                registry_owner: None,
            },
        )
        .await;
        state
            .deps
            .protocol
            .call_threads
            .insert(room_jid.clone(), active_call_thread(initiator.to_bare()));

        let outcome = maybe_broadcast_call_thread_ended(state.as_ref(), &room_jid).await;

        assert_eq!(outcome, WebhookEffectOutcome::Completed);
        assert!(
            !state.deps.protocol.call_threads.contains_key(&room_jid),
            "a successfully broadcast ended fastening must consume the active entry"
        );
    }

    #[tokio::test]
    async fn completed_old_call_thread_never_removes_a_same_room_replacement() {
        let state = create_test_websocket_state_with_sfu(Arc::new(RecordingSfu::default())).await;
        let room_jid: BareJid = "reused-call-room@muc.example.com"
            .parse()
            .expect("room jid");
        let initiator: BareJid = "alice@example.com".parse().expect("initiator jid");
        let completed = active_call_thread(initiator.clone());
        let mut replacement = active_call_thread(initiator);
        replacement.anchor_origin_id = "replacement-anchor".to_owned();
        replacement.thread_id = "replacement-thread".to_owned();
        state
            .deps
            .protocol
            .call_threads
            .insert(room_jid.clone(), replacement.clone());

        remove_completed_call_thread(state.as_ref(), &room_jid, &completed);

        let retained = state
            .deps
            .protocol
            .call_threads
            .get(&room_jid)
            .expect("replacement remains");
        assert_eq!(retained.thread_id, replacement.thread_id);
        assert_eq!(retained.anchor_origin_id, replacement.anchor_origin_id);
    }
}
