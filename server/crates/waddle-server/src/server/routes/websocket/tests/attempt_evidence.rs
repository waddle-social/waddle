//! Typed attempt evidence for socket writes, the byte bound of the outbound
//! pump queue, and the bounded ordered-relay handoff queue.

use super::super::send::{
    send_ws_message_with_authority, AuthoritySendOutcome, InterruptedWrite, WebSocketWriteError,
};
use super::Arc;
use crate::clustering::NodeLifecycle;
use crate::websocket_transport::{attempt_stage, AdmissionError, OutputError, WriteProgress};
use axum::extract::ws::Message;
use futures::Sink;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::registry::ActorOutboundStage;

#[test]
fn every_write_progress_maps_to_one_custody_stage() {
    for (progress, stage) in [
        (WriteProgress::Admitted, ActorOutboundStage::Queued),
        (WriteProgress::Attempted, ActorOutboundStage::Attempting),
        (WriteProgress::Completed, ActorOutboundStage::Flushed),
        (
            WriteProgress::Uncertain,
            ActorOutboundStage::AttemptedUncertain,
        ),
        (
            WriteProgress::NotAttempted,
            ActorOutboundStage::NotAttempted,
        ),
    ] {
        assert_eq!(attempt_stage(progress), stage);
    }
}

#[test]
fn a_failed_write_reports_not_attempted_only_when_that_is_proven() {
    type Error = OutputError<axum::Error>;
    // Refused before the frame entered the pump queue.
    for refused in [
        Error::Admission(AdmissionError::Overloaded),
        Error::Admission(AdmissionError::Stopped),
        Error::Closing,
        Error::WrongConnection,
        Error::InvalidFrame,
        Error::Interrupted(WriteProgress::NotAttempted),
    ] {
        assert_eq!(refused.attempt_stage(), ActorOutboundStage::NotAttempted);
    }
    // The sink's `start_send` began, or the failure carries no receipt.
    for uncertain in [
        Error::Interrupted(WriteProgress::Uncertain),
        Error::RetirementFailed,
    ] {
        assert_eq!(
            uncertain.attempt_stage(),
            ActorOutboundStage::AttemptedUncertain
        );
    }

    // The evidence survives the wrapper the connection writes through.
    let wrapped: InterruptedWrite<Error> =
        InterruptedWrite::Transport(Error::Interrupted(WriteProgress::Uncertain));
    assert_eq!(
        wrapped.attempt_stage(),
        Some(ActorOutboundStage::AttemptedUncertain)
    );
    let detached: InterruptedWrite<Error> = InterruptedWrite::Detached;
    assert_eq!(detached.attempt_stage(), None);
}

/// A sink that accepts `start_send` and then fails its flush the way a pump
/// reports a frame whose write began but did not complete.
#[derive(Default)]
struct UncertainFlushSink {
    started: Arc<Mutex<Vec<Message>>>,
}

impl Sink<Message> for UncertainFlushSink {
    type Error = OutputError<axum::Error>;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, frame: Message) -> Result<(), Self::Error> {
        self.started.lock().expect("frames").push(frame);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Err(OutputError::Interrupted(WriteProgress::Uncertain)))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

/// Without stream management a completed write is the only acceptance. An
/// uncertain attempt is reported as a closed transport, which the outbound
/// handler answers by leaving the producer its retry: the write acceptance
/// is not acknowledged and the stanza is not treated as delivered.
#[tokio::test]
async fn an_uncertain_attempt_is_not_acceptance() {
    let lifecycle = NodeLifecycle::new();
    let permit = lifecycle.admit().expect("permit");
    let shutdown = CancellationToken::new();
    let mut sink = UncertainFlushSink::default();
    let started = sink.started.clone();
    let stanza =
        "<message xmlns='jabber:client' id='m1'><body>caf\u{e9} &amp; more</body></message>";

    let outcome = send_ws_message_with_authority(
        &mut sink,
        Message::Text(stanza.into()),
        "uncertain write",
        Some((&permit, &shutdown)),
    )
    .await;

    assert!(matches!(outcome, AuthoritySendOutcome::TransportClosed));
    // The frame handed to the sink is the serialized stanza, byte for byte.
    assert_eq!(
        started.lock().expect("frames").as_slice(),
        [Message::Text(stanza.into())]
    );
}

#[cfg(feature = "test-support")]
mod pump {
    use super::*;
    use crate::websocket_transport::{
        start_websocket_transport, IoCustodianInventory, Limits, TestSocketAdmission,
        TransportLimits,
    };
    use futures::{SinkExt, StreamExt};
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn admission(inventory: &IoCustodianInventory) -> TestSocketAdmission {
        TestSocketAdmission::standalone(
            &NodeLifecycle::new(),
            inventory,
            CancellationToken::new(),
            CancellationToken::new(),
        )
        .expect("admission")
    }

    /// The outbound queue admits a frame only within its byte bound. A frame
    /// larger than the whole bound is refused before it is queued, with a
    /// typed refusal whose evidence is `NotAttempted`, and the refusal
    /// neither drops a frame already admitted nor stops later frames.
    #[tokio::test]
    async fn an_oversized_frame_is_refused_without_losing_admitted_frames() {
        let inventory = IoCustodianInventory::new(1).expect("inventory");
        let limits = TransportLimits::new(
            Limits::new(1, 64).expect("inbound limits"),
            Limits::new(4, 16).expect("outbound limits"),
        );
        let (wire, mut written) = futures::channel::mpsc::unbounded::<Message>();
        let (_custodian, _input, output) = start_websocket_transport(
            admission(&inventory),
            limits,
            TIMEOUT,
            |_: &futures::channel::mpsc::SendError| false,
            move |_| {
                std::future::ready((
                    futures::stream::pending::<Result<Message, std::io::Error>>(),
                    wire,
                ))
            },
        )
        .await
        .expect("transport starts");
        let mut sink = output.into_message_sink();

        sink.send(Message::Text("0123456789".into()))
            .await
            .expect("a frame within the bound is written");

        let refused = sink
            .send(Message::Text("x".repeat(17).into()))
            .await
            .expect_err("a frame above the whole bound is refused");
        assert!(matches!(
            refused,
            OutputError::Admission(AdmissionError::Overloaded)
        ));
        assert_eq!(refused.attempt_stage(), ActorOutboundStage::NotAttempted);

        sink.send(Message::Text("after".into()))
            .await
            .expect("the transport keeps writing after a refusal");

        let first = tokio::time::timeout(TIMEOUT, written.next())
            .await
            .expect("first frame");
        let second = tokio::time::timeout(TIMEOUT, written.next())
            .await
            .expect("second frame");
        assert_eq!(first, Some(Message::Text("0123456789".into())));
        assert_eq!(second, Some(Message::Text("after".into())));
    }

    /// A receipt follows its frame from admission to a terminal stage, and a
    /// flushed frame reached the socket as the exact bytes submitted.
    #[tokio::test]
    async fn a_receipt_reports_flushed_for_the_exact_bytes_written() {
        let inventory = IoCustodianInventory::new(1).expect("inventory");
        let limits = TransportLimits::new(
            Limits::new(1, 64).expect("inbound limits"),
            Limits::new(4, 1024).expect("outbound limits"),
        );
        let (wire, mut written) = futures::channel::mpsc::unbounded::<Message>();
        let (_custodian, _input, mut output) = start_websocket_transport(
            admission(&inventory),
            limits,
            TIMEOUT,
            |_: &futures::channel::mpsc::SendError| false,
            move |_| {
                std::future::ready((
                    futures::stream::pending::<Result<Message, std::io::Error>>(),
                    wire,
                ))
            },
        )
        .await
        .expect("transport starts");
        let stanza = "<iq xmlns='jabber:client' type='result' id='q&apos;1'/>";

        let frame = output
            .data_frame(Message::Text(stanza.into()))
            .expect("data frame");
        let receipt = output.submit(frame).await.expect("admitted");
        assert_eq!(
            tokio::time::timeout(TIMEOUT, receipt.wait_for_terminal())
                .await
                .expect("terminal progress"),
            WriteProgress::Completed
        );
        assert_eq!(receipt.attempt_stage(), ActorOutboundStage::Flushed);
        assert_eq!(
            tokio::time::timeout(TIMEOUT, written.next())
                .await
                .expect("written frame"),
            Some(Message::Text(stanza.into()))
        );
    }
}

#[cfg(feature = "clustering")]
mod handoff {
    use crate::server::routes::interpret::{
        ordered_relay_handoff_channel, OrderedRelayHandoffHandle, OrderedRelayHandoffRefusal,
        OrderedRelayInboundSequence, ORDERED_RELAY_HANDOFF_CAPACITY,
    };

    /// The completion queue is bounded by outstanding handoffs. At the bound
    /// a new handoff is refused before anything is routed; every handoff
    /// already granted still delivers its completion, and room returns only
    /// when the connection has taken one.
    #[tokio::test]
    async fn a_full_handoff_queue_refuses_new_handoffs_and_keeps_granted_ones() {
        let (tx, mut rx) = ordered_relay_handoff_channel();
        let granted: Vec<_> = (0..ORDERED_RELAY_HANDOFF_CAPACITY as u32)
            .map(|sequence| {
                OrderedRelayHandoffHandle::reserve(OrderedRelayInboundSequence(sequence), &tx)
                    .expect("within the bound")
            })
            .collect();
        let overflow = OrderedRelayInboundSequence(ORDERED_RELAY_HANDOFF_CAPACITY as u32);
        assert_eq!(
            OrderedRelayHandoffHandle::reserve(overflow, &tx).err(),
            Some(OrderedRelayHandoffRefusal::Full)
        );

        for handoff in &granted {
            handoff.complete(Vec::new());
            // A second completion of the same handoff delivers nothing more.
            handoff.complete(Vec::new());
        }
        // Completed but not yet taken: the slots are still in use.
        assert_eq!(
            OrderedRelayHandoffHandle::reserve(overflow, &tx).err(),
            Some(OrderedRelayHandoffRefusal::Full)
        );

        for sequence in 0..ORDERED_RELAY_HANDOFF_CAPACITY as u32 {
            let completion = rx.try_recv().expect("every granted handoff completes");
            assert_eq!(
                completion.inbound_sequence,
                OrderedRelayInboundSequence(sequence)
            );
        }
        assert!(rx.try_recv().is_err(), "each handoff completed once");
        assert!(OrderedRelayHandoffHandle::reserve(overflow, &tx).is_ok());
    }

    #[tokio::test]
    async fn an_unused_handoff_returns_its_slot_and_a_closed_queue_refuses() {
        let (tx, rx) = ordered_relay_handoff_channel();
        let granted: Vec<_> = (0..ORDERED_RELAY_HANDOFF_CAPACITY as u32)
            .map(|sequence| {
                OrderedRelayHandoffHandle::reserve(OrderedRelayInboundSequence(sequence), &tx)
                    .expect("within the bound")
            })
            .collect();
        let next = OrderedRelayInboundSequence(u32::MAX);
        assert!(OrderedRelayHandoffHandle::reserve(next, &tx).is_err());
        drop(granted);
        assert!(OrderedRelayHandoffHandle::reserve(next, &tx).is_ok());

        drop(rx);
        assert_eq!(
            OrderedRelayHandoffHandle::reserve(next, &tx).err(),
            Some(OrderedRelayHandoffRefusal::Closed)
        );
    }
}
