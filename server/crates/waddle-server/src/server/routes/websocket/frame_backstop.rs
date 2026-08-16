//! Per-connection stanza-handler **wedge backstop** (#808).
//!
//! Fix-4 of the #757 production incident. The per-connection frame loop awaits
//! stanza dispatch inline; before this, a single handler that blocked forever
//! (a wedged actor, a stuck lock) froze the whole connection — the #757
//! symptom. This module wraps stanza dispatch in a bounded
//! [`tokio::time::timeout`] so no single stanza can stall the connection
//! indefinitely. Processing stays strictly serial, so RFC 6120 §10.1 in-order
//! processing is preserved (no spawn-based concurrency — see
//! `docs/adr/008-stanza-handler-wedge-backstop.md`).
//!
//! On elapse the response is **conformant** (RFC 6120 §8.2.3 / §8.3): a timed-out
//! IQ `get`/`set` still owes exactly one reply, so we synthesize
//! `<iq type='error'><resource-constraint/></iq>` with error type `wait` (a
//! temporary, retryable condition). Message/Presence owe no reply; when stream
//! management is active they remain explicitly unhandled and the transport is
//! ended for replay/resumption instead of falsely advancing XEP-0198 `h`.
//!
//! Observability (all OTEL-native via the existing providers): a per-dispatch
//! `info_span!` (→ OTEL span), a `warn!` with stable fields (→ OTLP log), and
//! the [`metrics::record_stanza_handler_timeout`] counter (→ OTLP metric).

use std::future::Future;
use std::time::Duration;

use tracing::{info_span, warn, Instrument, Span};
use waddle_xmpp::metrics;
use waddle_xmpp::Stanza;

use super::handlers::iq::errors::resource_constraint_iq_error;

/// Whether dispatch accepted XEP-0198 responsibility for the inbound stanza.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InboundDisposition {
    Handled,
    Unhandled,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum StanzaTimeout {
    /// The retryable IQ error accepts responsibility for the request. Keep the
    /// response typed until `frame.rs` reaches the existing transport XML
    /// boundary.
    HandledIq(xmpp_parsers::minidom::Element),
    /// Dispatch was cancelled without accepting responsibility or owing a
    /// protocol response.
    Unhandled,
    /// The exact node serving generation was revoked while dispatch was
    /// suspended. No timeout reply is owed and XEP-0198 responsibility stays
    /// with the sender.
    AdmissionRevoked,
}

/// Completion from an admission-fenced dispatch.
///
/// Once admission has allowed a handler to start, it may commit durable or
/// external effects before its next suspension point. A later lifecycle
/// transition therefore fences the transport response, but cannot turn the
/// dispatch result back into an unhandled stanza.
pub(super) struct AdmissionDispatchResult<T> {
    pub(super) result: Result<T, StanzaTimeout>,
    pub(super) authority_revoked_after_start: bool,
}

/// Maximum wall-clock a single stanza's dispatch may take before the backstop
/// fires. This is a coarse *wedge* backstop, not a latency SLO: it sits above
/// the slowest legitimate handler's own internal budget (the 10s
/// `profile::fetch::TOTAL_TIMEOUT`) with margin, and below the wasm client's own
/// ~30s IQ timeout so the synthesized `wait` error is actionable. Complements
/// the tighter per-`.ask()` reply timeout in `waddle_xmpp::muc::RoomRegistry`
/// (#807), which is the fast, specific fail-path for the known actor wedge.
pub(super) const STANZA_HANDLER_WEDGE_TIMEOUT: Duration = Duration::from_secs(15);

/// The conformant reply addressing for a timed-out IQ `get`/`set`. The response
/// echoes the request `id` and swaps `from`/`to` (RFC 6120 §8.2.3).
struct IqReply {
    id: String,
    /// Response `from` = the request's `to`.
    from: Option<String>,
    /// Response `to` = the request's `from`.
    to: Option<String>,
}

/// Captured, owned metadata for one inbound stanza: enough to build the
/// conformant timeout reply and the diagnostic span/log without holding a borrow
/// on the stanza (which is moved into the dispatch future).
pub(super) struct StanzaBackstop {
    /// `"iq"` | `"message"` | `"presence"` — the stanza kind label.
    kind: &'static str,
    /// The request payload namespace (empty when absent), for the metric axis.
    payload_ns: String,
    /// `Some` only for an IQ `get`/`set`, which owes exactly one response.
    iq_reply: Option<IqReply>,
    /// Per-dispatch diagnostic span; becomes an OTEL span via the global bridge.
    span: Span,
}

impl StanzaBackstop {
    /// Capture backstop metadata from a borrowed stanza before it is moved into
    /// the dispatch future. `bound_jid` (the session's bound full JID, once
    /// binding happened) stamps the correlation attributes from #1326: the
    /// XMPP resource plus the bare user JID, so one attribute search joins a
    /// browser session's Faro beacons to its server-side spans.
    pub(super) fn capture(stanza: &Stanza, bound_jid: Option<&jid::FullJid>) -> Self {
        let correlation = MessageCorrelation::capture(stanza);
        match stanza {
            // Only IQ `get`/`set` carry a request payload AND owe a response, so
            // a single match keeps the namespace and the reply addressing in
            // lockstep — result/error IQs fall through to "no reply owed".
            Stanza::Iq(iq) => match &**iq {
                xmpp_parsers::iq::Iq::Get { payload, .. }
                | xmpp_parsers::iq::Iq::Set { payload, .. } => Self::build(
                    "iq",
                    payload.ns(),
                    Some(IqReply {
                        id: iq.id().to_string(),
                        from: iq.to().map(|jid| jid.to_string()),
                        to: iq.from().map(|jid| jid.to_string()),
                    }),
                    correlation,
                    bound_jid,
                ),
                _ => Self::build("iq", String::new(), None, correlation, bound_jid),
            },
            Stanza::Presence(_) => {
                Self::build("presence", String::new(), None, correlation, bound_jid)
            }
            Stanza::Message(_) => {
                Self::build("message", String::new(), None, correlation, bound_jid)
            }
        }
    }

    fn build(
        kind: &'static str,
        payload_ns: String,
        iq_reply: Option<IqReply>,
        correlation: MessageCorrelation,
        bound_jid: Option<&jid::FullJid>,
    ) -> Self {
        // `otel.status_code` is declared Empty and recorded as ERROR on elapse;
        // `tracing-opentelemetry` maps that field onto the OTEL span status.
        // The correlation fields (#1321/#1326) are declared Empty and recorded
        // only when known, so absent values don't serialize at all.
        let span = info_span!(
            "xmpp.stanza.dispatch",
            stanza_kind = kind,
            payload_ns = %payload_ns,
            otel.status_code = tracing::field::Empty,
            condition = tracing::field::Empty,
            message_id = tracing::field::Empty,
            room = tracing::field::Empty,
            xmpp.resource = tracing::field::Empty,
            user = tracing::field::Empty,
        );
        if let Some(message_id) = &correlation.message_id {
            span.record("message_id", message_id.as_str());
        }
        if let Some(room) = &correlation.room {
            span.record("room", tracing::field::display(room));
        }
        if let Some(jid) = bound_jid {
            span.record("xmpp.resource", jid.resource().as_str());
            span.record("user", tracing::field::display(jid.to_bare()));
        }
        Self {
            kind,
            payload_ns,
            iq_reply,
            span,
        }
    }

    /// Build the conformant response set for a dispatch that exceeded
    /// [`STANZA_HANDLER_WEDGE_TIMEOUT`]: a `resource-constraint`/`wait` IQ error
    /// for get/set, nothing for everything else. The typed disposition keeps an
    /// empty successful response distinct from a cancelled, unhandled stanza.
    /// Records the timeout metric and a `warn!`, and marks the span errored.
    fn on_timeout(self) -> StanzaTimeout {
        metrics::record_stanza_handler_timeout(self.kind, &self.payload_ns);
        self.span.record("otel.status_code", "ERROR");
        let _enter = self.span.enter();
        match &self.iq_reply {
            Some(reply) => {
                // The synthesized IQ rejection is typed as
                // `resource-constraint`; expose that bounded condition on the
                // same failed dispatch span as every handler-level rejection.
                self.span.record(
                    "condition",
                    waddle_xmpp::StanzaErrorCondition::ResourceConstraint.as_str(),
                );
                warn!(
                    stanza_kind = self.kind,
                    id = %reply.id,
                    // ADR-008 §5 stable field set; from/to identify the affected
                    // peer JID for grep/OTLP triage — the primary #757 gap.
                    // (Response addressing: from = request `to`, to = request `from`.)
                    from = ?reply.from,
                    to = ?reply.to,
                    payload_ns = %self.payload_ns,
                    timeout_secs = STANZA_HANDLER_WEDGE_TIMEOUT.as_secs(),
                    "stanza handler exceeded wedge backstop; returning resource-constraint"
                );
                StanzaTimeout::HandledIq(super::transport_xml::build_iq_error_element_typed(
                    &reply.id,
                    reply.from.as_deref(),
                    reply.to.as_deref(),
                    resource_constraint_iq_error(
                        "The server could not process this request in time; please retry.",
                    ),
                ))
            }
            None => {
                warn!(
                    stanza_kind = self.kind,
                    payload_ns = %self.payload_ns,
                    timeout_secs = STANZA_HANDLER_WEDGE_TIMEOUT.as_secs(),
                    "stanza handler exceeded wedge backstop; leaving stanza unhandled (no reply owed)"
                );
                // Dispatch was cancelled without a protocol response. XEP-0198
                // leaves responsibility with the sender.
                StanzaTimeout::Unhandled
            }
        }
    }
}

/// Message-correlation fields for the dispatch span (#1321): the stanza's
/// message id, and the room bare JID for groupchat traffic. Captured from the
/// borrowed stanza before it moves into the dispatch future.
struct MessageCorrelation {
    message_id: Option<String>,
    /// Typed until the span-record boundary, per the typed-payloads rule.
    room: Option<jid::BareJid>,
}

impl MessageCorrelation {
    fn capture(stanza: &Stanza) -> Self {
        match stanza {
            Stanza::Message(message) => Self {
                message_id: message.id.as_ref().map(|id| id.0.clone()),
                room: (message.type_ == xmpp_parsers::message::MessageType::Groupchat)
                    .then(|| message.to.as_ref().map(|to| to.to_bare()))
                    .flatten(),
            },
            _ => Self {
                message_id: None,
                room: None,
            },
        }
    }
}

/// Drive a stanza's dispatch under the wedge backstop: run `dispatch` within the
/// backstop span and the [`STANZA_HANDLER_WEDGE_TIMEOUT`]; on elapse, return the
/// conformant timeout response instead of letting the connection hang.
///
/// Completed dispatches feed the `xmpp.stanzas.processed` counter and the
/// `xmpp.stanza.latency` histogram (#1320 wire-up / #1321 dispatch seam);
/// timeouts keep their dedicated counter and are not double-counted as
/// processed.
pub(super) async fn run_with_backstop<F, T>(
    backstop: StanzaBackstop,
    dispatch: F,
) -> Result<T, StanzaTimeout>
where
    F: Future<Output = T> + Send,
    T: Default + Send,
{
    run_with_backstop_impl(backstop, dispatch, None)
        .await
        .result
}

pub(super) async fn run_with_backstop_and_admission<F, T>(
    backstop: StanzaBackstop,
    dispatch: F,
    permit: &crate::clustering::NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> AdmissionDispatchResult<T>
where
    F: Future<Output = T> + Send,
    T: Default + Send,
{
    run_with_backstop_impl(backstop, dispatch, Some((permit, shutdown))).await
}

async fn run_with_backstop_impl<F, T>(
    backstop: StanzaBackstop,
    dispatch: F,
    admission: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
) -> AdmissionDispatchResult<T>
where
    F: Future<Output = T> + Send,
    T: Default + Send,
{
    // This check is the responsibility boundary. It happens immediately
    // before dispatch: a revoked generation never starts a new handler,
    // while an admitted handler runs to the existing bounded backstop.
    // Cancelling an admitted handler on revocation can replay a stanza whose
    // durable/external effect already committed.
    if admission
        .is_some_and(|(permit, shutdown)| shutdown.is_cancelled() || permit.revalidate().is_err())
    {
        return AdmissionDispatchResult {
            result: Err(StanzaTimeout::AdmissionRevoked),
            authority_revoked_after_start: false,
        };
    }
    let span = backstop.span.clone();
    let kind = backstop.kind;
    let started = std::time::Instant::now();
    let dispatch = tokio::time::timeout(STANZA_HANDLER_WEDGE_TIMEOUT, dispatch.instrument(span));
    let result = dispatch.await;
    let authority_revoked_after_start = admission
        .is_some_and(|(permit, shutdown)| shutdown.is_cancelled() || permit.revalidate().is_err());
    let result = match result {
        Ok(responses) => {
            metrics::record_stanza(kind, "inbound");
            metrics::record_stanza_latency(started.elapsed().as_secs_f64() * 1000.0, kind);
            Ok(responses)
        }
        Err(_elapsed) => Err(backstop.on_timeout()),
    };
    let result = if authority_revoked_after_start {
        match result {
            Ok(_) | Err(StanzaTimeout::HandledIq(_)) => Ok(T::default()),
            Err(timeout) => Err(timeout),
        }
    } else {
        result
    };
    AdmissionDispatchResult {
        result,
        authority_revoked_after_start,
    }
}

#[cfg(test)]
mod tests;
