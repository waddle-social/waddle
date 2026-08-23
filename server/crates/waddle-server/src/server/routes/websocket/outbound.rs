use super::*;
use super::{
    batch_write::{
        write_response_batch_with_admission, BatchAuthority, BatchSmPolicy, BatchWriteOutcome,
    },
    frame::ordered_relay_origin_from_sm,
    interpret_loop::build_interpret_deps,
    replay::drive_interpret_loop,
    send::{send_ws_message_with_authority, AuthoritySendOutcome},
    state::WsConnState,
    stream_management::is_countable_stanza,
    timers::TransportTimers,
    transport_xml::stanza_to_xml,
};
use waddle_xmpp::stream_management::SmRequest;
use waddle_xmpp::telemetry::attributes::SmEvictionPath;

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
            let mut resumable_recovery_owned = false;
            if conn.sm_state.enabled && is_countable_stanza(&xml) {
                let record_result = match pending_row_receipt_at {
                    Some(receipt_at) => conn.sm_state.record_outbound_with_receipt_at(
                        xml.clone(),
                        receipt_at,
                        SmEvictionPath::DirectOutbound,
                    ),
                    None => conn
                        .sm_state
                        .record_outbound(xml.clone(), SmEvictionPath::DirectOutbound),
                };
                request_ack_after = record_result.request_ack;
                resumable_recovery_owned =
                    conn.sm_state.is_resumable() && conn.sm_state.replay_gap_through().is_none();
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
            let sent = match send_ws_message_with_authority(
                sender,
                Message::Text(xml.into()),
                "Failed to send outbound stanza",
                Some((permit, shutdown)),
            )
            .await
            {
                AuthoritySendOutcome::Sent => true,
                AuthoritySendOutcome::TransportClosed => {
                    if resumable_recovery_owned {
                        if let Some(acceptance) = outbound_stanza.write_acceptance.as_ref() {
                            acceptance.acknowledge();
                        }
                    }
                    return false;
                }
                AuthoritySendOutcome::AuthorityRevoked => {
                    // The frame may already sit in the live SM queue, but the
                    // old generation has not yet detached and persisted that
                    // queue. Mirror the force-detach/pending contract here:
                    // in-memory recording alone is not enough to settle the
                    // producer. Dropping the unacknowledged token makes the
                    // producer retain the row for lease-expiry retry.
                    return false;
                }
            };
            // With stream management enabled, `record_outbound` above placed
            // the exact XML in the resumable recovery queue before this sink
            // write.  Without SM, a successful sink write is the only
            // available acceptance point.  Either way, registry enqueue alone
            // is never enough to resolve this notification.
            if sent {
                if let Some(acceptance) = outbound_stanza.write_acceptance.as_ref() {
                    acceptance.acknowledge();
                }
            }
            // SM cadence: when `record_outbound` flagged the threshold,
            // follow the just-written stanza with an `<r/>` so the
            // client knows to send `<a h='N'/>`. The wasm client never
            // acks proactively, so without this nudge the unacked queue
            // grows unbounded until eviction permanently breaks resume.
            if sent && request_ack_after {
                if !authoritative() {
                    return false;
                }
                if !matches!(
                    send_ws_message_with_authority(
                        sender,
                        Message::Text(SmRequest::to_xml().into()),
                        "Failed to send SM <r/> request",
                        Some((permit, shutdown)),
                    )
                    .await,
                    AuthoritySendOutcome::Sent
                ) {
                    return false;
                }
                conn.sm_state.note_ack_request_sent();
            }
            sent
        }
        DeliveryKind::PeerStanza => {
            // #229 PR11: peer-routed stanza. Run the recipient pass before
            // writing so XEP-0191, XEP-0359, MAM, carbons, and inbox side
            // effects stay identical to the in-loop path.
            let ordered_relay_origin =
                ordered_relay_origin_from_sm(&conn.sm_state, conn.phase.bound_jid());
            let session = conn.authenticated_session.clone();
            let principal = session
                .as_ref()
                .map(super::ResolvedPrincipal::from_authenticated_session);
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
            let interpret_deps = build_interpret_deps(state.as_ref(), principal)
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
                if !matches!(
                    send_ws_message_with_authority(
                        sender,
                        Message::Ping(axum::body::Bytes::new()),
                        "Failed to send keepalive ping",
                        Some((permit, shutdown)),
                    )
                    .await,
                    AuthoritySendOutcome::Sent
                ) {
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
                BatchWriteOutcome::TransportClosed | BatchWriteOutcome::DeferredCapExhausted => {
                    return false;
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use waddle_xmpp::{
        registry::{OutboundStanza, OutboundWriteAcceptance},
        Stanza,
    };

    struct RecordingSink {
        sent: Vec<Message>,
        acceptance: Option<tokio::sync::oneshot::Receiver<()>>,
        acceptance_pending_on_send: bool,
    }

    impl futures::Sink<Message> for RecordingSink {
        type Error = Infallible;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            self.acceptance_pending_on_send = matches!(
                self.acceptance
                    .as_mut()
                    .expect("writer acceptance receiver")
                    .try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            );
            self.sent.push(item);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    struct TransportClosedSink {
        acceptance: Option<tokio::sync::oneshot::Receiver<()>>,
        acceptance_pending_on_send: bool,
        send_attempts: usize,
    }

    impl futures::Sink<Message> for TransportClosedSink {
        type Error = io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            self.acceptance_pending_on_send = matches!(
                self.acceptance
                    .as_mut()
                    .expect("writer acceptance receiver")
                    .try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            );
            self.send_attempts += 1;
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "simulated transport close",
            ))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    struct RevokeDuringReadySink {
        lifecycle: crate::clustering::NodeLifecycle,
        ready_polls: usize,
    }

    impl futures::Sink<Message> for RevokeDuringReadySink {
        type Error = Infallible;

        fn poll_ready(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            self.ready_polls += 1;
            if self.ready_polls == 1 {
                self.lifecycle.begin_drain();
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            panic!("authority revocation must suppress start_send");
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn direct_frame_write_acceptance_follows_sm_backed_writer_handoff() {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.sm_state
            .enable("outbound-write-acceptance".to_owned(), true, Some(300));
        let (acceptance, accepted) = OutboundWriteAcceptance::new();
        let mut sink = RecordingSink {
            sent: Vec::new(),
            acceptance: Some(accepted),
            acceptance_pending_on_send: false,
        };
        let mut reader = futures::stream::pending::<Result<Message, Infallible>>();
        let mut timers = TransportTimers::new();
        let stanza = Stanza::Message(xmpp_parsers::message::Message::new(Some(
            "alice@example.test".parse().expect("recipient JID"),
        )));

        assert!(
            handle_outbound_stanza(
                &mut sink,
                &mut reader,
                &state,
                &mut conn,
                &mut timers,
                OutboundStanza::with_write_acceptance(stanza, acceptance),
                OutboundAuthority {
                    permit: &permit,
                    shutdown: &shutdown,
                },
            )
            .await
        );
        assert_eq!(conn.sm_state.queue_len(), 1, "SM owns recovery before ack");
        assert_eq!(sink.sent.len(), 1, "writer accepted the direct frame");
        assert!(
            sink.acceptance_pending_on_send,
            "registry enqueue must not acknowledge before the writer accepts the frame"
        );
        assert!(
            sink.acceptance
                .take()
                .expect("writer acceptance receiver")
                .await
                .is_ok(),
            "SM-backed writer resolves acceptance"
        );
    }

    #[tokio::test]
    async fn resumable_sm_direct_frame_acknowledges_write_acceptance_on_transport_close() {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.sm_state.enable(
            "outbound-write-acceptance-close".to_owned(),
            true,
            Some(300),
        );
        let (acceptance, accepted) = OutboundWriteAcceptance::new();
        let mut sink = TransportClosedSink {
            acceptance: Some(accepted),
            acceptance_pending_on_send: false,
            send_attempts: 0,
        };
        let mut reader = futures::stream::pending::<Result<Message, Infallible>>();
        let mut timers = TransportTimers::new();
        let stanza = Stanza::Message(xmpp_parsers::message::Message::new(Some(
            "alice@example.test".parse().expect("recipient JID"),
        )));

        assert!(
            !handle_outbound_stanza(
                &mut sink,
                &mut reader,
                &state,
                &mut conn,
                &mut timers,
                OutboundStanza::with_write_acceptance(stanza, acceptance),
                OutboundAuthority {
                    permit: &permit,
                    shutdown: &shutdown,
                },
            )
            .await
        );
        assert_eq!(sink.send_attempts, 1, "writer attempted the direct send");
        assert_eq!(
            conn.sm_state.queue_len(),
            1,
            "resumable SM retained recovery ownership"
        );
        assert!(
            sink.acceptance_pending_on_send,
            "registry enqueue must not acknowledge before the writer attempts the frame"
        );
        assert!(
            sink.acceptance
                .take()
                .expect("writer acceptance receiver")
                .await
                .is_ok(),
            "resumable SM ownership must resolve acceptance even after transport close"
        );
    }

    #[tokio::test]
    async fn replay_gapped_sm_direct_frame_keeps_write_acceptance_pending_on_transport_close() {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.sm_state.enable(
            "outbound-write-acceptance-replay-gap".to_owned(),
            true,
            Some(300),
        );
        for sequence in 0..waddle_xmpp::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE {
            let _ = conn.sm_state.record_outbound(
                format!("<message id='{sequence}'/>"),
                SmEvictionPath::DirectOutbound,
            );
        }
        let (acceptance, accepted) = OutboundWriteAcceptance::new();
        let mut sink = TransportClosedSink {
            acceptance: Some(accepted),
            acceptance_pending_on_send: false,
            send_attempts: 0,
        };
        let mut reader = futures::stream::pending::<Result<Message, Infallible>>();
        let mut timers = TransportTimers::new();
        let stanza = Stanza::Message(xmpp_parsers::message::Message::new(Some(
            "alice@example.test".parse().expect("recipient JID"),
        )));

        assert!(
            !handle_outbound_stanza(
                &mut sink,
                &mut reader,
                &state,
                &mut conn,
                &mut timers,
                OutboundStanza::with_write_acceptance(stanza, acceptance),
                OutboundAuthority {
                    permit: &permit,
                    shutdown: &shutdown,
                },
            )
            .await
        );
        assert!(
            conn.sm_state.replay_gap_through().is_some(),
            "the overflowed frame cannot be recovered by SM resumption"
        );
        assert!(
            !matches!(
                sink.acceptance
                    .as_mut()
                    .expect("writer acceptance receiver")
                    .try_recv(),
                Ok(())
            ),
            "an unrecoverable replay-gapped frame must not settle its producer"
        );
    }

    #[tokio::test]
    async fn authority_revoked_gap_free_resumable_direct_frame_keeps_write_acceptance_pending() {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut conn = WsConnState::new();
        conn.sm_state.enable(
            "outbound-write-acceptance-authority-revoked".to_owned(),
            true,
            Some(300),
        );
        let (acceptance, mut accepted) = OutboundWriteAcceptance::new();
        let mut sink = RevokeDuringReadySink {
            lifecycle: lifecycle.clone(),
            ready_polls: 0,
        };
        let mut reader = futures::stream::pending::<Result<Message, Infallible>>();
        let mut timers = TransportTimers::new();
        let stanza = Stanza::Message(xmpp_parsers::message::Message::new(Some(
            "alice@example.test".parse().expect("recipient JID"),
        )));

        assert!(
            !handle_outbound_stanza(
                &mut sink,
                &mut reader,
                &state,
                &mut conn,
                &mut timers,
                OutboundStanza::with_write_acceptance(stanza, acceptance),
                OutboundAuthority {
                    permit: &permit,
                    shutdown: &shutdown,
                },
            )
            .await
        );
        assert_eq!(
            conn.sm_state.queue_len(),
            1,
            "the live SM queue recorded the frame"
        );
        assert!(
            conn.sm_state.replay_gap_through().is_none(),
            "the resumable queue stayed gap-free before detach"
        );
        assert_eq!(sink.ready_polls, 1, "revocation interrupted the ready wait");
        assert!(
            matches!(
                accepted.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ),
            "authority revocation must drop unacknowledged acceptance so the producer retains the row for retry"
        );
    }
}
