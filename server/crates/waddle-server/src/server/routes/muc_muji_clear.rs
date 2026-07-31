//! Shared MUC Muji-presence cleanup for SFU-driven and client-driven
//! call departures.
//!
//! Both the LiveKit webhook bridge and the Muji Jingle
//! `session-terminate` path need to clear the room actor's per-session
//! Muji state and broadcast the XEP-0272 leave marker to remaining
//! occupants. Centralising the logic keeps the wire shape identical.

use std::sync::{Arc, LazyLock};

use jid::{BareJid, FullJid};
use minidom::Element;
use tracing::{debug, warn};
use waddle_sfu::{ObservedCallSids, TeardownDisposition};
use waddle_xmpp::muc::build_occupant_presence;
use waddle_xmpp::muc::room_actor::{ClearMujiPresence, MujiPresenceUpdateOutcome};
use waddle_xmpp::telemetry::call::increment_call_teardown_stale_dropped;
use waddle_xmpp::xep::xep0272::Muji;
use waddle_xmpp::xep::xep0421::OccupantIdentity;
use waddle_xmpp::xep::{
    build_call_thread_ended, build_hint_element, CallThreadDuration, CallThreadEnded, Hint,
    NS_FASTEN,
};
use waddle_xmpp_core::Stanza;
use xmpp_parsers::message::{Message, MessageType};

use super::websocket::{
    get_room_actor_result, interpret_loop::build_interpret_deps,
    note_participant_left_from_webhook, observe_participant_sids_from_webhook, WebSocketState,
};

static CALL_THREAD_END_LOCKS: LazyLock<dashmap::DashMap<BareJid, Arc<tokio::sync::Mutex<()>>>> =
    LazyLock::new(dashmap::DashMap::new);

fn call_thread_end_lock(room_jid: &BareJid) -> Arc<tokio::sync::Mutex<()>> {
    CALL_THREAD_END_LOCKS
        .entry(room_jid.clone())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// The disposition of a webhook-driven side effect.  A retryable result keeps
/// the delivery in progress so LiveKit redelivers it; permanent input or
/// already-gone-room cases are acknowledged after their warning is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebhookEffectOutcome {
    Completed,
    Retryable(&'static str),
    Permanent(&'static str),
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
        increment_call_teardown_stale_dropped();
        return WebhookEffectOutcome::Completed;
    }

    let actor = match get_room_actor_result(state, room_jid).await {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            warn!(
                room = %room_jid,
                identity = %full_jid,
                "MUC room actor is absent during LiveKit departure cleanup; queueing owner-gated Muji clear"
            );
            if enqueue_muji_presence_clear(state, room_jid, full_jid, observed_sids)
                .await
                .is_err()
            {
                return WebhookEffectOutcome::Retryable("teardown_outbox_enqueue_failed");
            }
            record_participant_left(state, room_jid, full_jid, observed_sids);
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
    let outcome = match actor
        .ask(ClearMujiPresence {
            sender_jid: full_jid.clone(),
        })
        .await
    {
        Ok(Some(outcome)) => outcome,
        Ok(None) => {
            debug!(
                room = %room_jid,
                identity = %full_jid,
                "Participant not in MUC actor; SFU registry cleanup only"
            );
            record_participant_left(state, room_jid, full_jid, observed_sids);
            return maybe_broadcast_call_thread_ended(state, room_jid).await;
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
    record_participant_left(state, room_jid, full_jid, observed_sids);
    maybe_broadcast_call_thread_ended(state, room_jid).await
}

async fn enqueue_muji_presence_clear(
    state: &WebSocketState,
    room_jid: &BareJid,
    full_jid: &FullJid,
    observed_sids: Option<&ObservedCallSids>,
) -> Result<(), crate::call_teardown_outbox::CallTeardownOutboxError> {
    let call_id = match waddle_sfu::CallId::new(room_jid.to_string()) {
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
    let intent = crate::call_teardown_outbox::CallTeardownIntent {
        call_id,
        target: crate::call_teardown_outbox::TeardownTarget::MujiPresenceClear {
            room_jid: room_jid.clone(),
            departed: full_jid.clone(),
        },
        generation: None,
        room_sid: observed_sids.and_then(|sids| sids.room_sid.clone()),
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
) {
    if matches!(
        note_participant_left_from_webhook(state, room_jid, full_jid, observed_sids),
        Some(TeardownDisposition::StaleSid)
    ) {
        increment_call_teardown_stale_dropped();
    }
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

pub(crate) async fn maybe_broadcast_call_thread_ended(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> WebhookEffectOutcome {
    // A processing delivery is deliberately re-executed. Serialize this
    // final call-thread effect so overlapping attempts cannot both clone the
    // active entry and emit duplicate ended messages. On a retryable persist
    // failure the entry remains in the map for the next waiter to retry.
    let room_lock = call_thread_end_lock(room_jid);
    let end_guard = room_lock.lock().await;
    let outcome = async {
        let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
            return WebhookEffectOutcome::Completed;
        };
        let call_id = match waddle_sfu::CallId::new(room_jid.to_string()) {
            Ok(call_id) => call_id,
            Err(error) => {
                warn!(room = %room_jid, %error, "cannot derive call id while ending MUC call thread");
                return WebhookEffectOutcome::Permanent("invalid_call_id");
            }
        };
        if !sfu.participants_for_call(&call_id).is_empty() {
            return WebhookEffectOutcome::Completed;
        }
        let Some(active) = state
            .deps
            .protocol
            .call_threads
            .get(room_jid)
            .map(|active| active.clone())
        else {
            return WebhookEffectOutcome::Completed;
        };

        let ended = chrono::Utc::now();
        let duration = ended.signed_duration_since(active.started);
        let duration = CallThreadDuration::parse(&format_call_thread_duration(duration))
            .expect("formatted call-thread duration is valid");
        let message = build_call_thread_ended_message(
            room_jid,
            &active.anchor_origin_id,
            &CallThreadEnded {
                ended,
                duration: duration.clone(),
            },
        );
        // Stamp the ended summary onto every subscriber's inbox/threads
        // projection of this thread. The fastening below is the wire record;
        // this persists the same `ended` + `duration` onto the durable rows
        // keyed by the anchor's `urn:waddle:threads:0` thread id so the
        // threads view can surface the ended summary without replaying MAM.
        if let Err(error) = state
            .deps
            .protocol
            .inbox_storage
            .mark_call_thread_ended(room_jid, &active.thread_id, ended, &duration)
            .await
        {
            warn!(
                room = %room_jid,
                %error,
                "failed to persist call-thread ended summary to inbox"
            );
            return WebhookEffectOutcome::Retryable("inbox_call_thread_end_persist_failed");
        }

        let deps = build_interpret_deps(state, None);
        let _ = super::interpret::broadcast_room_system_message(
            &deps,
            room_jid.clone(),
            Box::new(message),
        )
        .await;
        state.deps.protocol.call_threads.remove(room_jid);
        WebhookEffectOutcome::Completed
    }
    .await;
    drop(end_guard);
    CALL_THREAD_END_LOCKS.remove_if(room_jid, |_, current| {
        Arc::ptr_eq(current, &room_lock) && Arc::strong_count(current) == 2
    });
    outcome
}

fn build_call_thread_ended_message(
    room_jid: &BareJid,
    anchor_origin_id: &str,
    ended: &CallThreadEnded,
) -> Message {
    let apply_to = Element::builder("apply-to", NS_FASTEN)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            anchor_origin_id,
        )
        .append(build_call_thread_ended(ended))
        .build();
    let mut message = Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.from = Some(jid::Jid::from(room_jid.clone()));
    message.type_ = MessageType::Groupchat;
    message.payloads.push(apply_to);
    message.payloads.push(build_hint_element(Hint::Store));
    message
}

fn format_call_thread_duration(duration: chrono::Duration) -> String {
    let seconds = duration.num_seconds().max(0);
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("PT{hours}H{minutes}M{seconds}S")
    } else if minutes > 0 {
        format!("PT{minutes}M{seconds}S")
    } else {
        format!("PT{seconds}S")
    }
}

#[cfg(test)]
mod tests {
    use super::{clear_muji_presence_for_departure, WebhookEffectOutcome};
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state_with_sfu, RecordingSfu,
    };
    use jid::{BareJid, FullJid};
    use std::sync::Arc;

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

        let outcome =
            clear_muji_presence_for_departure(state.as_ref(), &room_jid, &full_jid, None).await;

        assert_eq!(
            outcome,
            WebhookEffectOutcome::Retryable("teardown_outbox_enqueue_failed")
        );
    }
}
