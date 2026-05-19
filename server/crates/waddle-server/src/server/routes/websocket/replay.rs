use super::*;
use super::{
    interpret_loop::build_interpret_deps, stream_management::is_countable_stanza,
    transport_xml::stanza_to_xml,
};

/// Drain `outbound_rx` of all immediately-available
/// [`OutboundStanza`] values and record them into the per-connection
/// XEP-0198 unacked queue (and, when a `detached_stream_id` is
/// supplied, into the detached SM session's stored replay buffer).
///
/// Dispatches on [`DeliveryKind`] so the recipient-pass contract
/// PR11 introduced is preserved through the detach path. Without
/// this dispatch, queued `PeerStanza` values would be serialized
/// raw and replayed bytes would be missing the recipient-side
/// `<stanza-id>` stamp / archive write that the recipient pipeline
/// produces — exactly the bug Qodo flagged on PR269.
///
/// `state_machine` borrows the per-connection SM mutably so it can
/// feed `InboundEvent::StanzaFromPeer` for queued PeerStanza values.
/// When `state_machine` is `None` (pre-bind queue, never reached in
/// practice for a detach drain) PeerStanza values are dropped with
/// a WARN log.
pub(super) async fn drain_outbound_into_replay(
    state: &WebSocketState,
    state_machine: Option<&mut XmppStateMachine>,
    sm_state: &mut StreamManagementState,
    authenticated_session: Option<&crate::auth::Session>,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    detached_stream_id: Option<&str>,
) {
    let deps = build_interpret_deps(state, authenticated_session);
    let mut sm_borrow: Option<&mut XmppStateMachine> = state_machine;
    while let Ok(outbound_stanza) = outbound_rx.try_recv() {
        // Codex P2 review on PR #361: when this is a pending_delivery
        // flush replay, preserve the row's original_receipt_at instead
        // of stamping `Utc::now()` at drain time. Otherwise a flush
        // queued just before the WebSocket dropped would replay later
        // (after Q6 promotion re-creates the pending row) with a
        // wrong XEP-0203 `<delay/>` time.
        let receipt_at = outbound_stanza
            .pending_row_original_receipt_at
            .unwrap_or_else(chrono::Utc::now);
        let pending_row_id = outbound_stanza.pending_row_id.clone();
        match outbound_stanza.kind {
            DeliveryKind::DirectFrame => {
                let xml = stanza_to_xml(&outbound_stanza.stanza);
                record_drained_xml(
                    state,
                    sm_state,
                    detached_stream_id,
                    xml,
                    receipt_at,
                    pending_row_id,
                )
                .await;
            }
            DeliveryKind::PeerStanza => {
                let Some(sm) = sm_borrow.as_deref_mut() else {
                    warn!(
                        "PeerStanza encountered in detach drain without an SM; \
                         dropping. Resumed connection will not see this stanza."
                    );
                    continue;
                };
                let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(
                    outbound_stanza.stanza,
                )));
                let (frames, _close) = drive_interpret_loop(events, sm, &deps).await;
                let mut row_id_for_first = pending_row_id.clone();
                for xml in frames {
                    let row_for_this = row_id_for_first.take();
                    record_drained_xml(
                        state,
                        sm_state,
                        detached_stream_id,
                        xml,
                        receipt_at,
                        row_for_this,
                    )
                    .await;
                }
            }
        }
    }
}

/// Helper: record a single drained XML frame into the per-connection
/// SM unacked queue and, when applicable, into the detached SM
/// session's stored replay buffer. Pulled out so both the
/// `DirectFrame` and per-frame `PeerStanza` arms in
/// [`drain_outbound_into_replay`] can share the same recording
/// contract.
async fn record_drained_xml(
    state: &WebSocketState,
    sm_state: &mut StreamManagementState,
    detached_stream_id: Option<&str>,
    xml: String,
    original_receipt_at: chrono::DateTime<chrono::Utc>,
    pending_row_id: Option<waddle_xmpp::pending_delivery::PendingRowId>,
) {
    if !sm_state.enabled || !is_countable_stanza(&xml) {
        if let Some(row_id) = pending_row_id {
            if let Err(error) = state
                .deps
                .protocol
                .pending_delivery_storage
                .release_row(&row_id)
                .await
            {
                warn!(
                    row_id = %row_id,
                    %error,
                    "pending_delivery release_row (drained non-countable or SM-disabled) failed"
                );
            }
        }
        return;
    }
    // Drain path: we're recording into the unacked queue for replay
    // on the next resume, NOT writing to a live wire. The SM cadence
    // signal is moot — there is no socket to follow up with `<r/>`.
    let _ = sm_state.record_outbound_with_receipt_at(xml.clone(), original_receipt_at);
    let sequence = sm_state.outbound_count;
    if let Some(row_id) = pending_row_id.as_ref() {
        if let Err(error) = state
            .deps
            .protocol
            .pending_delivery_storage
            .record_pushed_at(row_id, sequence)
            .await
        {
            warn!(
                row_id = %row_id,
                sequence,
                %error,
                "pending_delivery record_pushed_at (drain path) failed; deleting row \
                 because SM unacked queue owns recovery"
            );
            if let Err(delete_error) = state
                .deps
                .protocol
                .pending_delivery_storage
                .delete_row(row_id)
                .await
            {
                warn!(
                    row_id = %row_id,
                    error = %delete_error,
                    "pending_delivery delete_row (drain record_pushed_at fallback) failed"
                );
            }
        }
    }
    if let Some(stream_id) = detached_stream_id {
        if let Err(error) = state
            .deps
            .protocol
            .sm_session_registry
            .record_outbound_for_detached_stream_at(stream_id, sequence, xml, original_receipt_at)
            .await
        {
            warn!(stream_id = %stream_id, %error, "Failed to record drained outbound for detached SM session");
        }
    }
}

/// Drive the interpret-loop that resolves an initial batch of
/// [`OutboundEvent`]s and any callback-feedback rounds the dispatcher
/// chain produces (e.g. `LookupArchivedMessage` -> `ArchivedMessageLoaded`
/// -> resumed pipeline events).
///
/// Returns the accumulated wire frames (already serialized via
/// [`crate::server::routes::interpret::interpret`]) and a `close`
/// flag set when any round emitted [`OutboundEvent::CloseTransport`].
/// The state machine `sm` is borrowed mutably so feedback events can
/// be re-fed via `sm.handle(...)` and produce the next-round
/// `OutboundEvent` batch.
pub(super) async fn drive_interpret_loop(
    initial_events: Vec<OutboundEvent>,
    sm: &mut XmppStateMachine,
    deps: &crate::server::routes::interpret::Deps<'_>,
) -> (Vec<String>, bool) {
    let mut all_frames = Vec::new();
    let mut close = false;
    let mut events_to_run = initial_events;
    // Each iteration: resolve the current batch, append its frames,
    // honour `close`, and if the batch produced callback-feedback,
    // feed it back through the SM to get the next batch.
    while !events_to_run.is_empty() {
        let outcome = crate::server::routes::interpret::interpret(events_to_run, deps).await;
        all_frames.extend(outcome.frames);
        if outcome.close {
            close = true;
        }
        if outcome.feedback.is_empty() {
            break;
        }
        let mut next_events = Vec::new();
        for fb in outcome.feedback {
            next_events.extend(sm.handle(fb));
        }
        events_to_run = next_events;
    }
    (all_frames, close)
}
