//! Stream-management observation helpers for the WebSocket boundary.
//!
//! XEP-0198 `<a h='N'/>` advances the server's handled frontier: it means the
//! server accepted responsibility for those stanzas. It is not message
//! delivery, display, dedupe, or any other higher-level product semantic.
//! `xmpp.sm.handled_progress` therefore tracks only handled-count advancement,
//! while `waddle.messages.delivered` remains the local-queue admission metric.
//!
//! `xmpp.sm.request.latency` measures from the oldest outstanding server
//! `<r/>` to the first client `<a/>` that covers the outbound frontier visible
//! when that request was sent. Later `<r/>` writes coalesce while one request
//! remains outstanding; the metric intentionally observes only the oldest.
//!
//! `xmpp.sm.handled_progress` counts only live `<a/>` advancement. The `h`
//! carried by `<resume/>` re-establishes the frontier on restored state and
//! is deliberately NOT counted as progress: the same logical resume applies
//! its `h` on both the fast path (`handle_sm_resume`) and the deferred
//! claim-finalization path (`registration.rs`), so counting either would
//! double-count against the other. `xmpp.sm.resume.results` records exactly
//! one terminal per resume attempt: failures at `handle_sm_resume` count
//! immediately, but a preliminary `Resumed` is NOT counted there — claim
//! finalization (`registration.rs`) can still flip the attempt to a wire
//! `<failed/>`/stream error, so the success (or the flip's failure outcome)
//! is recorded by [`observe_sm_resume_finalized`] once the transmitted
//! response is decided. An attempt whose connection dies between the two
//! stages records nothing — the wire never carried a terminal either.
//!
//! Timeout mapping for this lane:
//! - `xmpp.sm.drain_timeout` is the graceful-shutdown SM drain deadline.
//! - `xmpp.sm.send_window_pause_timeouts` is the paused high-watermark
//!   recovery deadline.
//!
//! No additional timeout metric family is introduced here.

use jid::FullJid;
use tracing::{info, warn};
use waddle_xmpp::pending_delivery::SmSessionId;

use super::super::{
    frame::{ResponseFrame, StreamErrorFrame},
    transport_xml::websocket_stream_close_element,
};
use waddle_xmpp::{
    stream_management::{SmFailed, SmResumed, StreamManagementState},
    telemetry::{attributes::SmAckOutcome, reliability},
};

pub(super) use waddle_xmpp::telemetry::attributes::SmResumeOutcome;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum SmAckObservation {
    Advanced {
        acked_from_exclusive: u32,
        delta: u64,
        latency_ms: Option<f64>,
    },
    Duplicate {
        latency_ms: Option<f64>,
    },
    Regressed,
    TooHigh {
        acknowledged: u32,
        send_count: u32,
    },
}

impl SmAckObservation {
    pub(super) fn outcome(self) -> SmAckOutcome {
        match self {
            Self::Advanced { .. } => SmAckOutcome::Advanced,
            Self::Duplicate { .. } => SmAckOutcome::Duplicate,
            Self::Regressed => SmAckOutcome::Regressed,
            Self::TooHigh { .. } => SmAckOutcome::TooHigh,
        }
    }
}

pub(super) fn apply_sm_ack_observation(
    sm_state: &mut StreamManagementState,
    h: u32,
) -> SmAckObservation {
    if sm_state.ack_regresses_last_acked(h) {
        return SmAckObservation::Regressed;
    }
    if sm_state.ack_exceeds_outbound(h) {
        return SmAckObservation::TooHigh {
            acknowledged: h,
            send_count: sm_state.outbound_count,
        };
    }
    if h == sm_state.last_acked {
        return SmAckObservation::Duplicate {
            latency_ms: sm_state.fulfill_oldest_ack_request_latency_ms(h),
        };
    }

    let acked_from_exclusive = sm_state.last_acked;
    let latency_ms = sm_state.fulfill_oldest_ack_request_latency_ms(h);
    sm_state.acknowledge(h);
    SmAckObservation::Advanced {
        acked_from_exclusive,
        delta: u64::from(h.wrapping_sub(acked_from_exclusive)),
        latency_ms,
    }
}

pub(super) fn observe_sm_ack(observation: SmAckObservation) {
    reliability::increment_sm_ack(observation.outcome());
    let latency_ms = match observation {
        SmAckObservation::Advanced {
            delta, latency_ms, ..
        } => {
            reliability::add_sm_handled_progress(delta);
            latency_ms
        }
        SmAckObservation::Duplicate { latency_ms } => latency_ms,
        SmAckObservation::Regressed | SmAckObservation::TooHigh { .. } => None,
    };
    if let Some(latency_ms) = latency_ms {
        reliability::record_sm_request_latency_ms(latency_ms);
    }
}

#[derive(Debug)]
pub(super) enum SmResumeTerminal {
    Failed {
        stream_id: SmSessionId,
        outcome: SmResumeOutcome,
        jid: Option<FullJid>,
        client_h: Option<u32>,
        replay_gap_through: Option<u32>,
        handled: Option<u32>,
        send_count: Option<u32>,
    },
    Resumed {
        stream_id: SmSessionId,
        handled: u32,
        jid: FullJid,
        replay: Vec<ResponseFrame>,
    },
}

impl SmResumeTerminal {
    pub(super) fn failed(stream_id: SmSessionId, outcome: SmResumeOutcome) -> Self {
        Self::Failed {
            stream_id,
            outcome,
            jid: None,
            client_h: None,
            replay_gap_through: None,
            handled: None,
            send_count: None,
        }
    }

    pub(super) fn identity_mismatch(stream_id: SmSessionId, resumed_jid: FullJid) -> Self {
        Self::Failed {
            stream_id,
            outcome: SmResumeOutcome::IdentityMismatch,
            jid: Some(resumed_jid),
            client_h: None,
            replay_gap_through: None,
            handled: None,
            send_count: None,
        }
    }

    pub(super) fn replay_gap(
        stream_id: SmSessionId,
        handled: u32,
        jid: FullJid,
        client_h: u32,
        replay_gap_through: Option<u32>,
    ) -> Self {
        Self::Failed {
            stream_id,
            outcome: SmResumeOutcome::ReplayGap,
            jid: Some(jid),
            client_h: Some(client_h),
            replay_gap_through,
            handled: Some(handled),
            send_count: None,
        }
    }

    pub(super) fn handled_too_high(
        stream_id: SmSessionId,
        acknowledged: u32,
        send_count: u32,
    ) -> Self {
        Self::Failed {
            stream_id,
            outcome: SmResumeOutcome::HandledTooHigh,
            jid: None,
            client_h: Some(acknowledged),
            replay_gap_through: None,
            handled: None,
            send_count: Some(send_count),
        }
    }

    pub(super) fn detached_divergence(stream_id: SmSessionId, detached_jid: FullJid) -> Self {
        Self::Failed {
            stream_id,
            outcome: SmResumeOutcome::DetachedDivergence,
            jid: Some(detached_jid),
            client_h: None,
            replay_gap_through: None,
            handled: None,
            send_count: None,
        }
    }

    pub(super) fn resumed(
        stream_id: SmSessionId,
        handled: u32,
        jid: FullJid,
        replay: Vec<ResponseFrame>,
    ) -> Self {
        Self::Resumed {
            stream_id,
            handled,
            jid,
            replay,
        }
    }

    pub(super) fn outcome(&self) -> SmResumeOutcome {
        match self {
            Self::Failed { outcome, .. } => *outcome,
            Self::Resumed { .. } => SmResumeOutcome::Resumed,
        }
    }

    fn condition(&self) -> Option<&'static str> {
        match self.outcome() {
            SmResumeOutcome::UnexpectedRequest => Some("unexpected-request"),
            SmResumeOutcome::NotFound => Some("item-not-found"),
            SmResumeOutcome::OwnerUnreachable | SmResumeOutcome::ReplayGap => {
                Some("resource-constraint")
            }
            SmResumeOutcome::IdentityMismatch
            | SmResumeOutcome::PrincipalUnavailable
            | SmResumeOutcome::DetachedDivergence => Some("not-authorized"),
            SmResumeOutcome::Storage | SmResumeOutcome::Internal => Some("internal-server-error"),
            SmResumeOutcome::ShutdownAbandoned
            | SmResumeOutcome::HandledTooHigh
            | SmResumeOutcome::Resumed => None,
        }
    }

    pub(super) fn into_frames(self) -> Vec<ResponseFrame> {
        match self {
            Self::Failed {
                outcome: SmResumeOutcome::ShutdownAbandoned,
                ..
            } => Vec::new(),
            Self::Failed {
                outcome: SmResumeOutcome::HandledTooHigh,
                client_h: Some(acknowledged),
                send_count: Some(send_count),
                ..
            } => vec![
                ResponseFrame::from(StreamErrorFrame::HandledCountTooHigh {
                    acknowledged,
                    send_count,
                }),
                ResponseFrame::from(websocket_stream_close_element()),
            ],
            Self::Failed {
                outcome: SmResumeOutcome::ReplayGap,
                handled: Some(handled),
                ..
            } => vec![ResponseFrame::from(
                SmFailed::resume_failed("resource-constraint", handled).to_element(),
            )],
            // `condition()` is `None` only for `ShutdownAbandoned` (matched
            // above), `Resumed` (the other variant), and `HandledTooHigh` —
            // whose dedicated arm above requires the fields its constructor
            // always sets. A field-less `HandledTooHigh` therefore cannot
            // carry a truthful frame; degrade to the generic internal error
            // instead of panicking on a proof the type system doesn't hold.
            Self::Failed { .. } => vec![ResponseFrame::from(
                SmFailed::with_condition(self.condition().unwrap_or("internal-server-error"))
                    .to_element(),
            )],
            Self::Resumed {
                stream_id,
                handled,
                replay,
                ..
            } => {
                let mut responses = Vec::with_capacity(replay.len() + 1);
                responses.push(ResponseFrame::from(
                    SmResumed::new(stream_id.as_str().to_owned(), handled).to_element(),
                ));
                responses.extend(replay);
                responses
            }
        }
    }
}

/// Record the terminal resume result once the transmitted response is
/// decided (claim finalization) — the preliminary `Resumed` from
/// `handle_sm_resume` is provisional until then. See the module doc.
pub(super) fn observe_sm_resume_finalized(outcome: SmResumeOutcome) {
    reliability::increment_sm_resume_result(outcome);
}

pub(super) fn observe_sm_resume(terminal: &SmResumeTerminal) {
    if !matches!(terminal.outcome(), SmResumeOutcome::Resumed) {
        reliability::increment_sm_resume_result(terminal.outcome());
    }

    match terminal {
        SmResumeTerminal::Failed {
            stream_id,
            outcome,
            jid,
            client_h,
            replay_gap_through,
            send_count,
            ..
        } => match outcome {
            SmResumeOutcome::UnexpectedRequest => {
                info!(stream_id = %stream_id, outcome = ?outcome, "SM resume rejected: unexpected request");
            }
            SmResumeOutcome::NotFound => {
                info!(stream_id = %stream_id, outcome = ?outcome, "SM resume rejected: session not found or expired");
            }
            SmResumeOutcome::OwnerUnreachable
            | SmResumeOutcome::Storage
            | SmResumeOutcome::Internal
            | SmResumeOutcome::PrincipalUnavailable => {
                warn!(stream_id = %stream_id, outcome = ?outcome, "SM resume failed");
            }
            SmResumeOutcome::ShutdownAbandoned => {
                info!(stream_id = %stream_id, outcome = ?outcome, "SM resume abandoned: graceful shutdown in progress");
            }
            SmResumeOutcome::IdentityMismatch | SmResumeOutcome::DetachedDivergence => {
                warn!(stream_id = %stream_id, outcome = ?outcome, resumed_jid = ?jid, "SM resume rejected: identity mismatch");
            }
            SmResumeOutcome::ReplayGap => {
                warn!(
                    stream_id = %stream_id,
                    outcome = ?outcome,
                    jid = ?jid,
                    client_h = ?client_h,
                    replay_gap_through = ?replay_gap_through,
                    "SM resume rejected: replay window no longer contains every stanza required by client h"
                );
            }
            SmResumeOutcome::HandledTooHigh => {
                info!(
                    stream_id = %stream_id,
                    outcome = ?outcome,
                    client_h = ?client_h,
                    send_count = ?send_count,
                    "SM resume rejected: handled count too high"
                );
            }
            SmResumeOutcome::Resumed => {}
        },
        SmResumeTerminal::Resumed {
            stream_id,
            jid,
            replay,
            ..
        } => {
            info!(stream_id = %stream_id, outcome = ?SmResumeOutcome::Resumed, jid = %jid, replay = replay.len(), "SM resume accepted; awaiting claim finalization");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::{Arc, Mutex};

    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::{InMemorySpanExporterBuilder, SdkTracerProvider};
    use tracing_subscriber::prelude::*;
    use waddle_xmpp::telemetry::{
        attributes::MessageKind, messages::record_delivered_message, test_support,
    };

    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("capture buffer lock")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sm_ack_metrics_classify_outcomes_without_touching_delivered_counter() {
        let metrics = test_support::acquire().await;
        record_delivered_message(MessageKind::Dm);

        let mut sm_state = StreamManagementState::new();
        sm_state.enable("ack-metrics".to_string(), true, Some(300));
        sm_state.last_acked = 3;
        sm_state.outbound_count = 5;

        observe_sm_ack(apply_sm_ack_observation(&mut sm_state, 5));
        observe_sm_ack(apply_sm_ack_observation(&mut sm_state, 5));
        observe_sm_ack(apply_sm_ack_observation(&mut sm_state, 4));
        observe_sm_ack(apply_sm_ack_observation(&mut sm_state, 6));

        assert_eq!(
            metrics.counter_sum("xmpp.sm.acks", &[("outcome", "advanced")]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum("xmpp.sm.acks", &[("outcome", "duplicate")]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum("xmpp.sm.acks", &[("outcome", "regressed")]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum("xmpp.sm.acks", &[("outcome", "too_high")]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum("xmpp.sm.handled_progress", &[]),
            Some(2)
        );
        assert_eq!(
            metrics.counter_sum("waddle.messages.delivered", &[("kind", "dm")]),
            Some(1),
            "processing <a/> must not increment the delivered-message counter"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sm_ack_wraparound_delta_records_progress_and_request_latency() {
        let metrics = test_support::acquire().await;

        let mut sm_state = StreamManagementState::new();
        sm_state.enable("ack-wrap".to_string(), true, Some(300));
        sm_state.last_acked = u32::MAX - 1;
        sm_state.outbound_count = 1;
        sm_state.note_ack_request_sent();

        observe_sm_ack(apply_sm_ack_observation(&mut sm_state, 1));

        assert_eq!(
            metrics.counter_sum("xmpp.sm.acks", &[("outcome", "advanced")]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum("xmpp.sm.handled_progress", &[]),
            Some(3)
        );
        assert_eq!(
            metrics.histogram_count("xmpp.sm.request.latency", &[]),
            Some(1)
        );
        assert_eq!(sm_state.last_acked, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sm_resume_observation_keeps_origin_id_out_of_metrics_spans_and_logs() {
        let metrics = test_support::acquire().await;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let exporter = InMemorySpanExporterBuilder::new().build();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(CaptureWriter(buffer.clone())),
            )
            .with(
                tracing_opentelemetry::layer()
                    .with_tracer(provider.tracer("sm-observe-test"))
                    .with_error_events_to_status(true)
                    .with_error_records_to_exceptions(true),
            );
        let _subscriber = tracing::subscriber::set_default(subscriber);

        let sentinel_origin_id = "sentinel-origin-id-sm-observe";
        let resumed_jid: FullJid = "alice@example.com/web".parse().expect("jid");
        let replay = vec![ResponseFrame::from(format!(
            "<message xmlns='jabber:client'><origin-id xmlns='urn:xmpp:sid:0' id='{sentinel_origin_id}'/></message>"
        ))];

        observe_sm_resume(&SmResumeTerminal::resumed(
            SmSessionId::new("stream-resumed"),
            7,
            resumed_jid,
            replay,
        ));
        assert_eq!(
            metrics.counter_sum("xmpp.sm.resume.results", &[("outcome", "resumed")]),
            None,
            "preliminary resumed terminal must not count before claim finalization"
        );
        observe_sm_resume_finalized(SmResumeOutcome::Resumed);

        provider
            .force_flush()
            .expect("in-memory tracer provider must flush");
        assert_eq!(
            metrics.counter_sum("xmpp.sm.resume.results", &[("outcome", "resumed")]),
            Some(1)
        );
        for (_, attributes) in metrics
            .counter_samples("xmpp.sm.resume.results")
            .expect("resume result samples")
        {
            for (key, value) in attributes {
                assert_ne!(key, sentinel_origin_id);
                assert_ne!(value, sentinel_origin_id);
            }
        }
        for span in exporter
            .get_finished_spans()
            .expect("in-memory exporter must yield finished spans")
        {
            for attribute in span.attributes {
                assert_ne!(attribute.key.as_str(), sentinel_origin_id);
                assert_ne!(attribute.value.to_string(), sentinel_origin_id);
            }
        }
        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        assert!(logs.contains("SM resume accepted"), "{logs}");
        assert!(
            !logs.contains(sentinel_origin_id),
            "origin-id must stay out of telemetry logs: {logs}"
        );
    }
}
