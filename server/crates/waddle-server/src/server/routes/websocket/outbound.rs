use super::*;
use super::{
    batch_write::{
        write_response_batch_with_admission, BatchAuthority, BatchSmPolicy, BatchWriteOutcome,
    },
    frame::ordered_relay_origin_from_sm,
    interpret_loop::build_interpret_deps,
    replay::drive_interpret_loop,
    send::send_ws_message,
    state::WsConnState,
    stream_management::is_countable_stanza,
    timers::TransportTimers,
    transport_xml::stanza_to_xml,
};
use waddle_xmpp::stream_management::SmRequest;

#[derive(Clone, Copy)]
pub(super) struct OutboundAuthority<'a> {
    pub(super) permit: &'a crate::clustering::NodeAdmissionPermit,
    pub(super) shutdown: &'a tokio_util::sync::CancellationToken,
}

pub(super) async fn handle_outbound_stanza<S, SE, R, RE>(
    sender: &mut S,
    reader: &mut R,
    state: &Arc<WebSocketState>,
    conn: &mut WsConnState,
    timers: &mut TransportTimers,
    outbound_stanza: OutboundStanza,
    authority: OutboundAuthority<'_>,
) -> bool
where
    S: Sink<Message, Error = SE> + Unpin,
    SE: std::fmt::Display,
    R: futures::Stream<Item = Result<Message, RE>> + Unpin,
    RE: std::fmt::Display,
{
    let OutboundAuthority { permit, shutdown } = authority;
    let authoritative = || !shutdown.is_cancelled() && permit.revalidate().is_ok();
    if !authoritative() {
        return false;
    }
    debug!(kind = ?outbound_stanza.kind, "Received outbound stanza from registry");
    match outbound_stanza.kind {
        DeliveryKind::DirectFrame => {
            // Server-generated frame (carbon, IQ reply, SM ack, ...). Bypass
            // the recipient-pass pipeline and write directly to the wire.
            let xml = stanza_to_xml(&outbound_stanza.stanza);
            let pending_row_id = outbound_stanza.pending_row_id.clone();
            let pending_row_receipt_at = outbound_stanza.pending_row_original_receipt_at;
            let mut request_ack_after = false;
            if conn.sm_state.enabled && is_countable_stanza(&xml) {
                let record_result = match pending_row_receipt_at {
                    Some(receipt_at) => conn
                        .sm_state
                        .record_outbound_with_receipt_at(xml.clone(), receipt_at),
                    None => conn.sm_state.record_outbound(xml.clone()),
                };
                request_ack_after = record_result.request_ack;
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
            if !authoritative() {
                return false;
            }
            let sent = send_ws_message(
                sender,
                Message::Text(xml.into()),
                "Failed to send outbound stanza",
            )
            .await;
            // SM cadence: when `record_outbound` flagged the threshold,
            // follow the just-written stanza with an `<r/>` so the
            // client knows to send `<a h='N'/>`. The wasm client never
            // acks proactively, so without this nudge the unacked queue
            // grows unbounded until eviction permanently breaks resume.
            if sent && request_ack_after {
                if !authoritative() {
                    return false;
                }
                if !send_ws_message(
                    sender,
                    Message::Text(SmRequest::to_xml().into()),
                    "Failed to send SM <r/> request",
                )
                .await
                {
                    return false;
                }
            }
            sent
        }
        DeliveryKind::PeerStanza => {
            // #229 PR11: peer-routed stanza. Run the recipient pass before
            // writing so XEP-0191, XEP-0359, MAM, carbons, and inbox side
            // effects stay identical to the in-loop path.
            let ordered_relay_origin =
                ordered_relay_origin_from_sm(&conn.sm_state, conn.phase.bound_jid());
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
                build_interpret_deps(state.as_ref(), conn.authenticated_session.as_ref())
                    .with_ordered_relay_origin(ordered_relay_origin);
            let drive = drive_interpret_loop(events, sm, &interpret_deps).await;
            if !authoritative() {
                return false;
            }
            // Timer/keepalive effects can't arise from a StanzaFromPeer
            // dispatch today (only TransportReady/Tick produce them),
            // but honour them anyway so a future policy change can't
            // silently drop effects on this path.
            timers.apply(drive.timer_commands);
            for _ in 0..drive.keepalive_probes {
                if !authoritative() {
                    return false;
                }
                if !send_ws_message(
                    sender,
                    Message::Ping(axum::body::Bytes::new()),
                    "Failed to send keepalive ping",
                )
                .await
                {
                    return false;
                }
            }
            let close = drive.close;
            // Always best-effort flush the accumulated frames first, even if
            // `close=true`, so a final error stanza or stream-close frame is
            // visible before transport teardown.
            //
            // The chunked writer (issue #1089) records each countable
            // frame just before its own write, follows every
            // `ack_threshold`th one with an `<r/>`, and drains
            // already-arrived inbound `<a/>` acks after each `<r/>` so
            // a large outbound frame batch can't pin the unacked queue
            // at capacity.
            match write_response_batch_with_admission(
                sender,
                reader,
                state.as_ref(),
                conn,
                drive.frames,
                BatchSmPolicy::Record,
                BatchAuthority { permit, shutdown },
            )
            .await
            {
                BatchWriteOutcome::Continue => {}
                BatchWriteOutcome::TransportClosed => return false,
                BatchWriteOutcome::AuthorityRevoked => return false,
            }
            if close {
                info!("PeerStanza dispatch requested transport close");
                return false;
            }
            true
        }
    }
}
