//! Pure, synchronous XMPP state machine.
//!
//! The core method is [`XmppStateMachine::handle`], which is a pure
//! synchronous function from an [`InboundEvent`] + current state to a
//! `Vec<OutboundEvent>`. No async, no I/O, no mocks required in tests.
//!
//! This module owns only *per-connection* state. Cross-connection state
//! (connection registry, MUC room occupancy, MAM archive) is resolved by
//! the interpreter using the events emitted from here.

use super::dispatch::StanzaDispatcher;
use super::event::{CallbackId, InboundEvent, IqContext, OutboundEvent};
use super::frame::InboundFrame;
use super::phase::ConnectionPhase;
use crate::connection::Stanza;
use std::collections::HashMap;
use tracing::Level;

/// An async delegation the state machine is waiting to hear back about.
///
/// Emitted as part of an [`OutboundEvent`] with a [`CallbackId`]; when the
/// interpreter eventually returns an [`InboundEvent`] carrying the same
/// id, the state machine looks up the pending op, dispatches to a
/// completion handler, and drops the entry.
///
/// The variants here mirror the async [`OutboundEvent`] delegations and
/// capture whatever context the completion handler needs. Keep them
/// small — they're held in a `HashMap` on every connection.
#[derive(Debug, Clone)]
pub enum PendingOp {
    /// Awaiting a MAM window query — the original IQ id and the
    /// requester's full JID are needed to build the fin response.
    MamQuery {
        request_id: String,
        requester: jid::FullJid,
    },
    /// Awaiting link-enrichment on an outbound groupchat or direct
    /// message. The pre-enrichment stanza is retained so fallback
    /// behaviour can resend it if the enricher fails.
    Enrichment {
        fallback: Box<xmpp_parsers::message::Message>,
    },
    /// Awaiting the SFU actor's Jingle IQ response.
    Sfu {
        request_id: String,
        reply_from: Option<jid::Jid>,
        reply_to: Option<jid::Jid>,
    },
    /// Awaiting SCRAM credential load from the app's user store.
    ScramCredentials { username: String },
    /// Awaiting OAUTHBEARER token validation against `AppState`.
    OAuthBearer,
}

/// Per-connection XMPP protocol state machine.
pub struct XmppStateMachine {
    phase: ConnectionPhase,
    domain: String,
    dispatcher: StanzaDispatcher,
    /// Monotonically increasing counter feeding [`Self::next_callback_id`].
    next_callback: u64,
    /// In-flight async delegations keyed by the id sent to the interpreter.
    pending_ops: HashMap<CallbackId, PendingOp>,
}

impl XmppStateMachine {
    /// Construct a new machine in the `Unauthenticated` phase.
    pub fn new(domain: impl Into<String>, dispatcher: StanzaDispatcher) -> Self {
        Self {
            phase: ConnectionPhase::new(),
            domain: domain.into(),
            dispatcher,
            next_callback: 0,
            pending_ops: HashMap::new(),
        }
    }

    /// Allocate a fresh [`CallbackId`] for an outbound async delegation.
    ///
    /// Handlers call this before emitting an [`OutboundEvent`] carrying a
    /// callback id (e.g. [`OutboundEvent::QueryMam`]) and then
    /// [`Self::register_pending_op`] to stash the context needed when the
    /// matching [`InboundEvent`] completion arrives.
    pub fn next_callback_id(&mut self) -> CallbackId {
        self.next_callback = self.next_callback.wrapping_add(1);
        CallbackId(self.next_callback)
    }

    /// Consume a previously-registered [`PendingOp`] when its completion
    /// event arrives. Returns `None` for unknown ids (late / duplicate
    /// completions — logged and dropped by the caller).
    ///
    /// A `register_pending_op` counterpart will be reintroduced alongside
    /// the first async handler that needs it. For now the field is
    /// populated directly from tests in the same module.
    fn take_pending_op(&mut self, id: CallbackId) -> Option<PendingOp> {
        self.pending_ops.remove(&id)
    }

    /// Inspect the current phase. Useful for transport adapters that need to
    /// know whether the connection is registered, and for tests.
    pub fn phase(&self) -> &ConnectionPhase {
        &self.phase
    }

    /// The pure event → events transition.
    ///
    /// This is the only public entry point during normal operation.
    pub fn handle(&mut self, event: InboundEvent) -> Vec<OutboundEvent> {
        match event {
            InboundEvent::FrameReceived(frame) => self.on_frame(frame),
            InboundEvent::StanzaFromPeer(stanza) => self.on_peer_stanza(*stanza),
            InboundEvent::TransportClosed => self.on_closed(),
            InboundEvent::EnrichmentComplete { id, message } => {
                self.on_enrichment_complete(id, *message)
            }
            InboundEvent::SfuResponse { id, result } => self.on_sfu_response(id, result),
            InboundEvent::MamQueryComplete { id, result } => self.on_mam_complete(id, result),
            InboundEvent::ScramCredentialsLoaded { id, result } => {
                self.on_scram_credentials(id, result)
            }
            InboundEvent::OAuthBearerValidated { id, result } => {
                self.on_oauth_bearer_validated(id, result)
            }
        }
    }

    // ----- Async callback completions ---------------------------------------
    //
    // Step 1 of the sans-I/O migration wires up the plumbing: we look up the
    // pending op, log the completion, and drop the entry. Concrete handler
    // dispatch lands alongside each async-dependent handler (MAM query, SFU
    // ask, OAUTHBEARER validation, …) in later migration steps.

    fn on_enrichment_complete(
        &mut self,
        id: CallbackId,
        _message: xmpp_parsers::message::Message,
    ) -> Vec<OutboundEvent> {
        self.log_completion(id, "EnrichmentComplete")
    }

    fn on_sfu_response(
        &mut self,
        id: CallbackId,
        _result: super::event::CallbackResult,
    ) -> Vec<OutboundEvent> {
        self.log_completion(id, "SfuResponse")
    }

    fn on_mam_complete(
        &mut self,
        id: CallbackId,
        _result: super::event::CallbackResult,
    ) -> Vec<OutboundEvent> {
        self.log_completion(id, "MamQueryComplete")
    }

    fn on_scram_credentials(
        &mut self,
        id: CallbackId,
        _result: super::event::CallbackResult,
    ) -> Vec<OutboundEvent> {
        self.log_completion(id, "ScramCredentialsLoaded")
    }

    fn on_oauth_bearer_validated(
        &mut self,
        id: CallbackId,
        _result: super::event::CallbackResult,
    ) -> Vec<OutboundEvent> {
        self.log_completion(id, "OAuthBearerValidated")
    }

    /// Shared completion stub: take the pending op, log whether a match
    /// was found, and emit nothing else. Replaced by handler-specific
    /// dispatch as migration steps 4-6 land.
    fn log_completion(&mut self, id: CallbackId, kind: &str) -> Vec<OutboundEvent> {
        let op = self.take_pending_op(id);
        let matched = op.is_some();
        vec![OutboundEvent::Log {
            level: if matched { Level::DEBUG } else { Level::WARN },
            message: format!(
                "{kind} completion for {id:?} (matched pending op: {matched}); dispatch TBD"
            ),
        }]
    }

    fn on_frame(&mut self, frame: InboundFrame) -> Vec<OutboundEvent> {
        // Stream-framing acknowledgements (Open/Close) become the transport
        // adapter's responsibility in later migration steps. For now we
        // treat them as no-ops so the machine can be driven end-to-end in
        // tests without auth/bind wiring.
        //
        // SASL Auth/SaslResponse are recognised by the frame parser but
        // their handling (SCRAM challenge/response, OAUTHBEARER validation)
        // lands in step 6 of the migration plan. Until then they are logged
        // and dropped so the existing WebSocket auth code keeps owning the
        // flow end-to-end.
        match frame {
            InboundFrame::Open | InboundFrame::Close => Vec::new(),
            InboundFrame::Auth { mechanism, .. } => vec![OutboundEvent::Log {
                level: Level::DEBUG,
                message: format!(
                    "SASL <auth mechanism=\"{mechanism}\"> received; handled by legacy path"
                ),
            }],
            InboundFrame::SaslResponse(_) => vec![OutboundEvent::Log {
                level: Level::DEBUG,
                message: "SASL <response> received; handled by legacy path".to_string(),
            }],
            InboundFrame::Stanza(stanza) => self.on_stanza(*stanza),
        }
    }

    fn on_stanza(&self, stanza: Stanza) -> Vec<OutboundEvent> {
        // Extract the full JID from the current phase. Stanzas are only
        // dispatched in `Ready`; in every other phase they are protocol
        // violations and get logged.
        let full_jid = match &self.phase {
            ConnectionPhase::Ready { full_jid, .. } => full_jid,
            ConnectionPhase::Unauthenticated => {
                return vec![OutboundEvent::Log {
                    level: Level::WARN,
                    message: format!(
                        "Received {} stanza before authentication; ignoring",
                        stanza.name()
                    ),
                }];
            }
        };

        let ctx = IqContext {
            domain: &self.domain,
            full_jid,
        };

        match stanza {
            Stanza::Iq(iq) => self.dispatcher.dispatch_iq(&iq, &ctx),
            Stanza::Message(message) => self.dispatcher.dispatch_message(&message, &ctx),
            Stanza::Presence(presence) => self.dispatcher.dispatch_presence(&presence, &ctx),
        }
    }

    fn on_peer_stanza(&mut self, stanza: Stanza) -> Vec<OutboundEvent> {
        // Stanzas delivered from other connections via the registry are
        // forwarded unchanged to the client. Serialization of the stanza
        // into an XML frame is wired in step 2 of the migration.
        vec![OutboundEvent::Log {
            level: Level::DEBUG,
            message: format!("Peer-routed {} stanza (forwarding TBD)", stanza.name()),
        }]
    }

    fn on_closed(&mut self) -> Vec<OutboundEvent> {
        // Minimal cleanup for step 1. Full flow (unregister connection,
        // broadcast leave presences for joined_rooms, cancel timers) arrives
        // with the MUC migration in step 5.
        vec![OutboundEvent::Log {
            level: Level::INFO,
            message: "Transport closed".to_string(),
        }]
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Internal test helpers — not exported, used by the in-tree tests in
    //! this module and by the top-level `tests/protocol_state_machine.rs`
    //! integration test file.

    use super::*;
    use jid::FullJid;
    use std::collections::HashSet;

    /// Construct a machine already in the `Ready` phase, for unit tests that
    /// want to exercise stanza dispatch without going through the full auth
    /// + bind flow.
    pub fn ready_machine(
        domain: impl Into<String>,
        full_jid: FullJid,
        dispatcher: StanzaDispatcher,
    ) -> XmppStateMachine {
        XmppStateMachine {
            phase: ConnectionPhase::Ready {
                full_jid,
                joined_rooms: HashSet::new(),
            },
            domain: domain.into(),
            dispatcher,
            next_callback: 0,
            pending_ops: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::handlers::ping::PingHandler;
    use minidom::Element;
    use std::sync::Arc;
    use xmpp_parsers::iq::{Iq, IqType};

    fn make_ping_iq(id: &str) -> Iq {
        let ping_elem = Element::builder("ping", crate::xep::xep0199::NS_PING).build();
        Iq {
            from: None,
            to: None,
            id: id.to_string(),
            payload: IqType::Get(ping_elem),
        }
    }

    fn test_jid() -> jid::FullJid {
        "alice@waddle.social/web"
            .parse()
            .expect("test JID is valid")
    }

    #[test]
    fn ping_iq_in_ready_phase_emits_send_stanza() {
        let mut dispatcher = StanzaDispatcher::new();
        dispatcher.register_iq(Arc::new(PingHandler));
        let mut sm = test_support::ready_machine("waddle.social", test_jid(), dispatcher);

        let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
            Stanza::Iq(make_ping_iq("ping-42")),
        ))));

        assert_eq!(events.len(), 1, "expected one SendStanza event");
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    assert_eq!(reply.id, "ping-42");
                    assert!(matches!(reply.payload, IqType::Result(_)));
                }
                other => panic!("expected Iq, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn ping_iq_before_auth_is_logged_and_dropped() {
        let mut dispatcher = StanzaDispatcher::new();
        dispatcher.register_iq(Arc::new(PingHandler));
        let mut sm = XmppStateMachine::new("waddle.social", dispatcher);

        let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
            Stanza::Iq(make_ping_iq("ping-early")),
        ))));

        assert!(
            events
                .iter()
                .all(|e| !matches!(e, OutboundEvent::SendStanza(_))),
            "pre-auth stanzas must never produce a reply stanza"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, OutboundEvent::Log { .. })),
            "pre-auth stanzas should be logged for diagnostics"
        );
    }

    #[test]
    fn unknown_iq_namespace_emits_log_warning() {
        let dispatcher = StanzaDispatcher::new(); // no handlers registered
        let mut sm = test_support::ready_machine("waddle.social", test_jid(), dispatcher);

        let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
            Stanza::Iq(make_ping_iq("ping-unhandled")),
        ))));

        assert!(
            events.iter().any(|e| matches!(
                e,
                OutboundEvent::Log { level, .. } if *level == Level::WARN
            )),
            "unhandled namespaces should emit a WARN log"
        );
    }

    #[test]
    fn transport_closed_emits_info_log() {
        let mut sm = XmppStateMachine::new("waddle.social", StanzaDispatcher::new());
        let events = sm.handle(InboundEvent::TransportClosed);
        assert!(events.iter().any(|e| matches!(
            e,
            OutboundEvent::Log { level, .. } if *level == Level::INFO
        )));
    }

    #[test]
    fn open_and_close_frames_are_noops() {
        let mut sm = XmppStateMachine::new("waddle.social", StanzaDispatcher::new());
        assert!(sm
            .handle(InboundEvent::FrameReceived(InboundFrame::Open))
            .is_empty());
        assert!(sm
            .handle(InboundEvent::FrameReceived(InboundFrame::Close))
            .is_empty());
    }

    #[test]
    fn callback_ids_are_monotonic_and_unique() {
        let mut sm = XmppStateMachine::new("waddle.social", StanzaDispatcher::new());
        let a = sm.next_callback_id();
        let b = sm.next_callback_id();
        let c = sm.next_callback_id();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_eq!(a, CallbackId(1));
        assert_eq!(b, CallbackId(2));
        assert_eq!(c, CallbackId(3));
    }

    #[test]
    fn pending_op_round_trip_is_matched_by_completion_event() {
        // Allocate a callback, register a pending op, then feed the
        // matching completion InboundEvent. The machine must look up
        // the op, emit a DEBUG log (matched=true) and drop the entry.
        let mut sm = XmppStateMachine::new("waddle.social", StanzaDispatcher::new());
        let id = sm.next_callback_id();
        sm.pending_ops.insert(
            id,
            PendingOp::MamQuery {
                request_id: "mam-7".to_string(),
                requester: test_jid(),
            },
        );

        let events = sm.handle(InboundEvent::MamQueryComplete {
            id,
            result: crate::protocol::event::CallbackResult::Ok { stanza: None },
        });

        assert!(events.iter().any(|e| matches!(
            e,
            OutboundEvent::Log { level, .. } if *level == Level::DEBUG
        )));
        // Second completion with the same id must now miss the pending
        // map (op consumed) and log at WARN — this is the late/duplicate
        // completion diagnostic path.
        let events2 = sm.handle(InboundEvent::MamQueryComplete {
            id,
            result: crate::protocol::event::CallbackResult::Ok { stanza: None },
        });
        assert!(events2.iter().any(|e| matches!(
            e,
            OutboundEvent::Log { level, .. } if *level == Level::WARN
        )));
    }
}
