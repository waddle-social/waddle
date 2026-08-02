//! Durable MUC call-thread completion: the "ended" summary broadcast,
//! its per-room serialization, and the outbox-backed retry that
//! survives replacement calls and process restarts (#1612 review
//! rounds 8-12). Split out of `muc_muji_clear` so protocol handling
//! and persistence/retry concerns evolve independently.

use std::sync::Arc;

use jid::BareJid;
use minidom::Element;
use tracing::warn;
use waddle_xmpp::xep::{
    build_call_thread_ended, build_hint_element, CallThreadDuration, CallThreadEnded, Hint,
    NS_FASTEN,
};
use xmpp_parsers::message::{Message, MessageType};

use super::muc_muji_clear::WebhookEffectOutcome;
use super::websocket::{
    get_room_actor_result, interpret_loop::build_interpret_deps, WebSocketState,
};

fn call_thread_end_lock(state: &WebSocketState, room_jid: &BareJid) -> Arc<tokio::sync::Mutex<()>> {
    state
        .deps
        .protocol
        .call_thread_end_locks
        .entry(room_jid.clone())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Persist a completion-only retry of the call-thread "ended" broadcast.
/// Deliberately NOT a `MujiPresenceClear`: the presence clear already
/// succeeded on the caller's path, and replaying it from the outbox could
/// clobber a quick rejoin's fresh advertisement (#1612 review round 8).
pub(crate) async fn enqueue_call_thread_end_retry(
    state: &WebSocketState,
    room_jid: &BareJid,
    thread_id: waddle_xmpp_core::mam::ThreadId,
    anchor_origin_id: waddle_xmpp_core::xep0359::OriginId,
    started: chrono::DateTime<chrono::Utc>,
    ended: chrono::DateTime<chrono::Utc>,
) -> Result<(), crate::call_teardown_outbox::CallTeardownOutboxError> {
    let call_id = match waddle_sfu::CallId::new(room_jid.to_string()) {
        Ok(call_id) => call_id,
        Err(error) => {
            warn!(
                room = %room_jid,
                %error,
                "could not model call-thread completion retry as a teardown intent"
            );
            return Ok(());
        }
    };
    let intent = crate::call_teardown_outbox::CallTeardownIntent {
        call_id,
        target: crate::call_teardown_outbox::TeardownTarget::CallThreadEndRetry {
            room_jid: room_jid.clone(),
            thread_id,
            anchor_origin_id,
            started,
            ended,
        },
        generation: None,
        room_sid: None,
    };
    let store = &state.deps.protocol.call_teardown_outbox;
    let persistence = &state.deps.protocol.call_teardown_persistence;
    if let Err(error) = store.enqueue(intent.clone()).await {
        warn!(
            room = %room_jid,
            %error,
            "failed to persist call-thread completion retry; retrying asynchronously"
        );
        persistence.retry_batch(vec![intent]);
        return Err(error);
    }
    Ok(())
}

pub(crate) async fn maybe_broadcast_call_thread_ended(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> WebhookEffectOutcome {
    maybe_broadcast_call_thread_ended_for(state, room_jid, None).await
}

/// The durable completion retry's persisted view of the FAILED thread
/// (#1612 review rounds 10-11): identity plus everything needed to
/// reconstruct the ended summary when the in-memory `ActiveCallThread`
/// is gone (process restart) or was replaced by a newer call.
pub(crate) struct CallThreadCompletionFence<'a> {
    pub thread_id: &'a waddle_xmpp_core::mam::ThreadId,
    pub anchor_origin_id: &'a waddle_xmpp_core::xep0359::OriginId,
    pub started: chrono::DateTime<chrono::Utc>,
    /// The instant the call actually ended, captured when the first
    /// completion attempt failed. Reused verbatim by every retry so
    /// outage time never inflates the reported duration (#1612 review
    /// round 12).
    pub ended: chrono::DateTime<chrono::Utc>,
}

/// Like [`maybe_broadcast_call_thread_ended`], but fenced to a specific
/// thread: a durable completion retry must only ever finish THE thread
/// whose completion failed. When the live entry still matches, the
/// normal completion flow runs; when it is gone or replaced, the ended
/// summary is reconstructed from the fence's persisted payload without
/// touching the live thread (#1612 review rounds 10-11).
pub(crate) async fn maybe_broadcast_call_thread_ended_for(
    state: &WebSocketState,
    room_jid: &BareJid,
    expected_thread: Option<CallThreadCompletionFence<'_>>,
) -> WebhookEffectOutcome {
    // A processing delivery is deliberately re-executed. Serialize this
    // final call-thread effect so overlapping attempts cannot both clone the
    // active entry and emit duplicate ended messages. On a retryable persist
    // failure the entry remains in the map for the next waiter to retry.
    let room_lock = call_thread_end_lock(state, room_jid);
    let end_guard = room_lock.lock().await;
    let outcome = async {
        let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
            // A restarted process without a working SFU service must
            // still honor a persisted completion fence — the payload
            // path needs only inbox storage and room delivery (#1612
            // review round 12).
            if let Some(fence) = &expected_thread {
                return complete_call_thread_from_fence(state, room_jid, fence).await;
            }
            return WebhookEffectOutcome::Completed;
        };
        let call_id = match waddle_sfu::CallId::new(room_jid.to_string()) {
            Ok(call_id) => call_id,
            Err(error) => {
                warn!(room = %room_jid, %error, "cannot derive call id while ending MUC call thread");
                return WebhookEffectOutcome::Permanent("invalid_call_id");
            }
        };
        let live_active = state
            .deps
            .protocol
            .call_threads
            .get(room_jid)
            .map(|active| active.clone());
        if let Some(fence) = &expected_thread {
            let live_matches = live_active
                .as_ref()
                .is_some_and(|active| active.thread_id == fence.thread_id.as_str());
            if !live_matches {
                // Restart lost the in-memory entry, or a newer call
                // replaced it: the persisted payload is the only
                // remaining record of the failed thread. Complete from
                // it — never acknowledge an absent entry, never touch
                // the replacement (#1612 review round 11).
                return complete_call_thread_from_fence(state, room_jid, fence).await;
            }
        }
        if !sfu.participants_for_call(&call_id).is_empty() {
            return WebhookEffectOutcome::Completed;
        }
        let Some(active) = live_active else {
            return WebhookEffectOutcome::Completed;
        };

        // A fenced retry reuses the persisted end instant; only the
        // first-attempt webhook/presence path stamps "now".
        let ended = expected_thread
            .as_ref()
            .map(|fence| fence.ended)
            .unwrap_or_else(chrono::Utc::now);
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
        if super::interpret::broadcast_room_system_message(
            &deps,
            room_jid.clone(),
            Box::new(message),
        )
        .await
        .is_none()
        {
            // `mark_call_thread_ended` has upsert-style overwrite semantics,
            // so retaining the active entry and re-running persistence on a
            // LiveKit redelivery is safe.
            return WebhookEffectOutcome::Retryable("call_thread_end_broadcast_failed");
        }
        remove_completed_call_thread(state, room_jid, &active);
        WebhookEffectOutcome::Completed
    }
    .await;
    drop(end_guard);
    state
        .deps
        .protocol
        .call_thread_end_locks
        .remove_if(room_jid, |_, current| {
            Arc::ptr_eq(current, &room_lock) && Arc::strong_count(current) == 2
        });
    outcome
}

pub(crate) fn remove_completed_call_thread(
    state: &WebSocketState,
    room_jid: &BareJid,
    completed: &crate::server::routes::websocket::ActiveCallThread,
) {
    state
        .deps
        .protocol
        .call_threads
        .remove_if(room_jid, |_, current| {
            current.thread_id == completed.thread_id
                && current.anchor_origin_id == completed.anchor_origin_id
        });
}

/// Complete a failed call-thread end from its persisted payload. This
/// is at-least-once by design: if a concurrent path already finished
/// the thread, `mark_call_thread_ended` is an idempotent upsert and the
/// extra ended broadcast is benign — losing the summary forever (the
/// alternative) is not (#1612 review round 11).
async fn complete_call_thread_from_fence(
    state: &WebSocketState,
    room_jid: &BareJid,
    fence: &CallThreadCompletionFence<'_>,
) -> WebhookEffectOutcome {
    let ended = fence.ended;
    let duration = ended.signed_duration_since(fence.started);
    let duration = CallThreadDuration::parse(&format_call_thread_duration(duration))
        .expect("formatted call-thread duration is valid");
    let message = build_call_thread_ended_message(
        room_jid,
        fence.anchor_origin_id.as_str(),
        &CallThreadEnded {
            ended,
            duration: duration.clone(),
        },
    );
    if let Err(error) = state
        .deps
        .protocol
        .inbox_storage
        .mark_call_thread_ended(room_jid, fence.thread_id.as_str(), ended, &duration)
        .await
    {
        warn!(
            room = %room_jid,
            thread = %fence.thread_id.as_str(),
            %error,
            "failed to persist reconstructed call-thread ended summary"
        );
        return WebhookEffectOutcome::Retryable("inbox_call_thread_end_persist_failed");
    }
    // Broadcast is best-effort when no room actor exists anywhere
    // (post-restart dynamic room nobody rejoined): the durable value of
    // this row is the persisted inbox summary above, and there is no
    // occupant to deliver the fastening to. A room WITH an actor still
    // requires the broadcast to succeed (#1612 review round 12).
    let actor_present = matches!(get_room_actor_result(state, room_jid).await, Ok(Some(_)));
    let deps = build_interpret_deps(state, None);
    if super::interpret::broadcast_room_system_message(&deps, room_jid.clone(), Box::new(message))
        .await
        .is_none()
    {
        if actor_present {
            return WebhookEffectOutcome::Retryable("call_thread_end_broadcast_failed");
        }
        warn!(
            room = %room_jid,
            thread = %fence.thread_id.as_str(),
            "call-thread ended summary persisted, but no room actor exists to \
             receive the broadcast; completing from the durable payload"
        );
    }
    WebhookEffectOutcome::Completed
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
