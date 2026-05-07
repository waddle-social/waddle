use super::*;
use super::{
    interpret_loop::build_interpret_deps, replay::drive_interpret_loop, send::send_ws_message,
    state::WsConnState, stream_management::is_countable_stanza, transport_xml::stanza_to_xml,
};

pub(super) async fn handle_outbound_stanza<S, E>(
    sender: &mut S,
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    outbound_stanza: OutboundStanza,
) -> bool
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    debug!(kind = ?outbound_stanza.kind, "Received outbound stanza from registry");
    match outbound_stanza.kind {
        DeliveryKind::DirectFrame => {
            // Server-generated frame (carbon, IQ reply, SM ack, ...). Bypass
            // the recipient-pass pipeline and write directly to the wire.
            let xml = stanza_to_xml(&outbound_stanza.stanza);
            let pending_row_id = outbound_stanza.pending_row_id.clone();
            let pending_row_receipt_at = outbound_stanza.pending_row_original_receipt_at;
            if conn.sm_state.enabled && is_countable_stanza(&xml) {
                match pending_row_receipt_at {
                    Some(receipt_at) => conn
                        .sm_state
                        .record_outbound_with_receipt_at(xml.clone(), receipt_at),
                    None => conn.sm_state.record_outbound(xml.clone()),
                }
                // Locked Q7b SM-ack lifecycle: bind the just-assigned outbound
                // counter back onto pending_delivery flush rows before the next
                // queued SM ack can range-delete them.
                if let Some(row_id) = pending_row_id {
                    let sequence = conn.sm_state.outbound_count;
                    if let Err(error) = state
                        .deps
                        .protocol
                        .pending_delivery_storage
                        .record_pushed_at(&row_id, sequence)
                        .await
                    {
                        if conn.sm_state.is_resumable() {
                            warn!(
                                row_id = %row_id,
                                sequence,
                                error = %error,
                                "pending_delivery record_pushed_at failed; deleting row \
                                 because resumable SM unacked queue owns recovery"
                            );
                            if let Err(delete_error) = state
                                .deps
                                .protocol
                                .pending_delivery_storage
                                .delete_row(&row_id)
                                .await
                            {
                                warn!(
                                    row_id = %row_id,
                                    error = %delete_error,
                                    "pending_delivery delete_row (record_pushed_at fallback) failed"
                                );
                            }
                        } else {
                            warn!(
                                row_id = %row_id,
                                sequence,
                                error = %error,
                                "pending_delivery record_pushed_at failed; releasing row \
                                 because non-resumable SM has no detached replay owner"
                            );
                            if let Err(release_error) = state
                                .deps
                                .protocol
                                .pending_delivery_storage
                                .release_row(&row_id)
                                .await
                            {
                                warn!(
                                    row_id = %row_id,
                                    error = %release_error,
                                    "pending_delivery release_row (record_pushed_at fallback) failed"
                                );
                            }
                        }
                    }
                }
            }
            send_ws_message(sender, Message::Text(xml), "Failed to send outbound stanza").await
        }
        DeliveryKind::PeerStanza => {
            // #229 PR11: peer-routed stanza. Run the recipient pass before
            // writing so XEP-0191, XEP-0359, MAM, carbons, and inbox side
            // effects stay identical to the in-loop path.
            let Some(sm) = conn.state_machine.as_mut() else {
                warn!(
                    "PeerStanza arrived before per-connection state machine was initialized; \
                     dropping. This indicates a routing peer targeted us before bind completed."
                );
                return true;
            };
            let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(
                outbound_stanza.stanza,
            )));
            let interpret_deps =
                build_interpret_deps(state.as_ref(), conn.authenticated_session.as_ref());
            let (frames, close) = drive_interpret_loop(events, sm, &interpret_deps).await;
            // Always best-effort flush the accumulated frames first, even if
            // `close=true`, so a final error stanza or stream-close frame is
            // visible before transport teardown.
            for xml in frames {
                if conn.sm_state.enabled && is_countable_stanza(&xml) {
                    conn.sm_state.record_outbound(xml.clone());
                }
                if !send_ws_message(
                    sender,
                    Message::Text(xml),
                    "Failed to send recipient-pass frame",
                )
                .await
                {
                    return false;
                }
            }
            if close {
                info!("PeerStanza dispatch requested transport close");
                return false;
            }
            true
        }
    }
}
