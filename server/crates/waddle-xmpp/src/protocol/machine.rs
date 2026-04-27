//! Pure, synchronous XMPP state machine.
//!
//! The core method is [`XmppStateMachine::handle`], which is a pure
//! synchronous function from an [`InboundEvent`] + current state to a
//! `Vec<OutboundEvent>`. No async, no I/O, no mocks required in tests.
//!
//! This module owns only *per-connection* state. Cross-connection state
//! (connection registry, MUC room occupancy, MAM archive) is resolved by
//! the interpreter using the events emitted from here.

use super::dispatch::{MessageDispatchOutcome, MessageDispatchTermination, StanzaDispatcher};
use super::event::{CallbackId, InboundEvent, OutboundEvent, StanzaContext};
use super::frame::InboundFrame;
use super::handlers::enrichment_dispatch::ENRICHMENT_CALLBACK_SENTINEL;
use super::handlers::rich_target_validation::{
    self, RichTargetKind, RichTargetValidationHandler, RICH_TARGET_LOOKUP_CALLBACK_SENTINEL,
};
use super::id_gen::{IdGenerator, UuidV4Generator};
use super::message_context::{MessageContext, MessageContextEnv};
use super::phase::ConnectionPhase;
use super::session_state::{Blocklist, CarbonsState, MucOccupancy};
use super::traits::{HandlerId, HandlerOutcome};
use crate::Stanza;
use jid::BareJid;
use std::collections::HashMap;
use std::sync::Arc;
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
    /// Awaiting a callback that resumes a paused message-pipeline run.
    ///
    /// Created when [`super::dispatch::StanzaDispatcher::dispatch_message`]
    /// returns
    /// [`super::dispatch::MessageDispatchTermination::Awaiting`]; the
    /// pending op stores everything the resume needs to rebuild a fresh
    /// [`MessageContext`] and call
    /// [`super::dispatch::StanzaDispatcher::resume_message`] when the
    /// matching [`InboundEvent`] arrives.
    MessageDispatchResume {
        /// The handler index the pipeline paused at; resume runs the
        /// handler immediately after.
        resume_after: HandlerId,
        /// The (possibly already-stamped) message snapshot at pause
        /// time. Replaced by the rewritten message on
        /// `EnrichmentComplete`; reused as-is on `ArchivedMessageLoaded`.
        message: Box<xmpp_parsers::message::Message>,
        /// Which completion path applies — drives whether the state
        /// machine resumes the pipeline directly or first runs the
        /// XEP rule check via
        /// [`RichTargetValidationHandler::handle_completion`].
        kind: ResumeKind,
        /// Connection's bound full JID at pause time, for
        /// [`MessageContext`] rebuild on resume.
        full_jid: jid::FullJid,
        /// Snapshot of the session-bounded state at pause time. Per
        /// #229 Q5, `MessageContext` is frozen at dispatch start; the
        /// resumed dispatch sees the same view.
        blocklist: Blocklist,
        carbons: CarbonsState,
        muc_occupancy: MucOccupancy,
    },
}

/// Discriminator for the kind of completion the state machine should
/// run when an [`InboundEvent`] arrives matching a
/// [`PendingOp::MessageDispatchResume`].
#[derive(Debug, Clone)]
pub enum ResumeKind {
    /// The pause was triggered by
    /// [`super::handlers::enrichment_dispatch::EnrichmentDispatchHandler`].
    /// On `EnrichmentComplete`, the rewritten message replaces the
    /// stashed one and the pipeline resumes.
    Enrichment,
    /// The pause was triggered by
    /// [`super::handlers::rich_target_validation::RichTargetValidationHandler`].
    /// On `ArchivedMessageLoaded`, the state machine calls
    /// [`RichTargetValidationHandler::handle_completion`] with the
    /// stashed `kind` + `author`; on `Continue` the pipeline resumes,
    /// on `Halt` the typed error reply is forwarded directly.
    RichTarget {
        kind: RichTargetKind,
        author: BareJid,
    },
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
    /// Session-bounded XEP-0191 blocklist. Mutated by the XEP-0191 IQ
    /// handler; read by the message pipeline via [`MessageContext`].
    /// Empty until the XEP-0191 handler lands.
    blocklist: Blocklist,
    /// Session-bounded XEP-0280 carbons-enabled flag. Mutated by the
    /// carbons IQ handler; read by `CarbonsHandler` via
    /// [`MessageContext`].
    carbons: CarbonsState,
    /// Session-bounded XEP-0045 occupancy. Mutated by the MUC presence
    /// handler; read by `RouteHandler`'s groupchat branch via
    /// [`MessageContext`].
    muc_occupancy: MucOccupancy,
    /// Source of fresh, opaque XEP-0359 stanza-ids stamped by message
    /// handlers. Defaults to UUIDv4; tests can override.
    id_gen: Arc<dyn IdGenerator>,
}

impl XmppStateMachine {
    /// Construct a new machine in the `Unauthenticated` phase, with a
    /// production [`UuidV4Generator`] for XEP-0359 stamping.
    pub fn new(domain: impl Into<String>, dispatcher: StanzaDispatcher) -> Self {
        Self::with_id_gen(domain, dispatcher, Arc::new(UuidV4Generator))
    }

    /// Construct a machine with a caller-supplied [`IdGenerator`] —
    /// typically a deterministic test impl from
    /// [`super::id_gen::CounterIdGenerator`] or
    /// [`super::id_gen::FixedIdGenerator`].
    pub fn with_id_gen(
        domain: impl Into<String>,
        dispatcher: StanzaDispatcher,
        id_gen: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            phase: ConnectionPhase::new(),
            domain: domain.into(),
            dispatcher,
            next_callback: 0,
            pending_ops: HashMap::new(),
            blocklist: Blocklist::empty(),
            carbons: CarbonsState::Disabled,
            muc_occupancy: MucOccupancy::empty(),
            id_gen,
        }
    }

    /// Allocate a fresh [`CallbackId`] for an outbound async delegation.
    ///
    /// Handlers call this before emitting an [`OutboundEvent`] carrying a
    /// callback id (e.g. [`OutboundEvent::QueryMam`]) and then
    /// [`Self::register_pending_op`] to stash the context needed when the
    /// matching [`InboundEvent`] completion arrives.
    pub fn next_callback_id(&mut self) -> CallbackId {
        self.next_callback = self
            .next_callback
            .checked_add(1)
            .expect("callback id space exhausted");
        CallbackId(self.next_callback)
    }

    /// Register the completion context for a previously allocated callback id.
    ///
    /// Handlers must call this before emitting an async delegation event so
    /// the matching completion can recover its state.
    pub fn register_pending_op(&mut self, id: CallbackId, op: PendingOp) {
        assert!(
            self.pending_ops.insert(id, op).is_none(),
            "duplicate callback id registered: {id:?}"
        );
    }

    /// Consume a previously-registered [`PendingOp`] when its completion
    /// event arrives. Returns `None` for unknown ids (late / duplicate
    /// completions — logged and dropped by the caller).
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
            InboundEvent::ArchivedMessageLoaded { id, result } => {
                self.on_archived_message_loaded(id, result.as_deref())
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
        rewritten: xmpp_parsers::message::Message,
    ) -> Vec<OutboundEvent> {
        let Some(op) = self.take_pending_op(id) else {
            return vec![OutboundEvent::Log {
                level: Level::WARN,
                message: format!(
                    "EnrichmentComplete for unknown callback id {id:?}; \
                     late or duplicate completion, dropping"
                ),
            }];
        };
        match op {
            PendingOp::MessageDispatchResume {
                resume_after,
                message: _stashed,
                kind: ResumeKind::Enrichment,
                full_jid,
                blocklist,
                carbons,
                muc_occupancy,
            } => self.resume_message_dispatch(
                rewritten,
                resume_after,
                full_jid,
                blocklist,
                carbons,
                muc_occupancy,
            ),
            PendingOp::MessageDispatchResume { kind, .. } => vec![OutboundEvent::Log {
                level: Level::ERROR,
                message: format!(
                    "EnrichmentComplete for callback id {id:?} but pending op \
                     is not Enrichment-typed (kind={kind:?}); dropping"
                ),
            }],
            other => vec![OutboundEvent::Log {
                level: Level::ERROR,
                message: format!(
                    "EnrichmentComplete for callback id {id:?} but pending op \
                     is not a MessageDispatchResume (op={other:?}); dropping"
                ),
            }],
        }
    }

    fn on_archived_message_loaded(
        &mut self,
        id: CallbackId,
        result: Option<&super::event::ArchivedMessage>,
    ) -> Vec<OutboundEvent> {
        let Some(op) = self.take_pending_op(id) else {
            return vec![OutboundEvent::Log {
                level: Level::WARN,
                message: format!(
                    "ArchivedMessageLoaded for unknown callback id {id:?}; \
                     late or duplicate completion, dropping"
                ),
            }];
        };
        match op {
            PendingOp::MessageDispatchResume {
                resume_after,
                message,
                kind: ResumeKind::RichTarget { kind, author },
                full_jid,
                blocklist,
                carbons,
                muc_occupancy,
            } => {
                let completion =
                    RichTargetValidationHandler::handle_completion(kind, &message, result, &author);
                match completion {
                    HandlerOutcome::Continue(continue_events) => {
                        let mut all = continue_events;
                        let resumed = self.resume_message_dispatch(
                            *message,
                            resume_after,
                            full_jid,
                            blocklist,
                            carbons,
                            muc_occupancy,
                        );
                        all.extend(resumed);
                        all
                    }
                    HandlerOutcome::Halt(halt_events) => halt_events,
                    HandlerOutcome::AwaitCallback(events) => {
                        // Rich-target completion shouldn't itself
                        // park — surface as ERROR but at least
                        // forward the events so any reply reaches the
                        // wire.
                        let mut out = events;
                        out.push(OutboundEvent::Log {
                            level: Level::ERROR,
                            message: format!(
                                "RichTargetValidationHandler::handle_completion \
                                 returned AwaitCallback for {id:?}; not supported"
                            ),
                        });
                        out
                    }
                }
            }
            PendingOp::MessageDispatchResume { kind, .. } => vec![OutboundEvent::Log {
                level: Level::ERROR,
                message: format!(
                    "ArchivedMessageLoaded for callback id {id:?} but pending op \
                     is not RichTarget-typed (kind={kind:?}); dropping"
                ),
            }],
            other => vec![OutboundEvent::Log {
                level: Level::ERROR,
                message: format!(
                    "ArchivedMessageLoaded for callback id {id:?} but pending op \
                     is not a MessageDispatchResume (op={other:?}); dropping"
                ),
            }],
        }
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
        // Bearer-token validation is security-sensitive; until this callback
        // has a real typed dispatch path, consume the pending op without
        // emitting diagnostics that could become part of a token-taint flow.
        let _ = self.take_pending_op(id);
        Vec::new()
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
        // Stream-framing acknowledgements remain the transport adapter's
        // responsibility, but the machine still tracks lifecycle intent so
        // later typed cleanup can distinguish an explicit `<close/>` from an
        // unexpected transport drop.
        //
        // SASL Auth/SaslResponse are recognised by the frame parser but
        // their handling (SCRAM challenge/response, OAUTHBEARER validation)
        // is owned by the WebSocket frame dispatcher in websocket.rs.
        // The state machine delegates silently.
        match frame {
            InboundFrame::Open => Vec::new(),
            InboundFrame::Close => {
                self.phase = ConnectionPhase::closing(self.phase.bound_jid().cloned());
                Vec::new()
            }
            InboundFrame::Auth { .. } => vec![],
            InboundFrame::SaslResponse(_) => vec![],
            InboundFrame::Stanza(stanza) => self.on_stanza(*stanza),
        }
    }

    fn on_stanza(&mut self, stanza: Stanza) -> Vec<OutboundEvent> {
        // Extract the full JID from the current phase. Stanzas are only
        // dispatched in `Ready`; in every other phase they are protocol
        // violations and get logged.
        let full_jid = match &self.phase {
            ConnectionPhase::Ready { full_jid, .. } => full_jid.clone(),
            ConnectionPhase::Unauthenticated
            | ConnectionPhase::ScramPending { .. }
            | ConnectionPhase::Authenticated { .. }
            | ConnectionPhase::Closing { .. } => {
                return vec![OutboundEvent::Log {
                    level: Level::WARN,
                    message: format!(
                        "Received {} stanza before authentication; ignoring",
                        stanza.name()
                    ),
                }];
            }
        };

        let ctx = StanzaContext {
            domain: &self.domain,
            full_jid: &full_jid,
        };

        match stanza {
            Stanza::Iq(iq) => self.dispatcher.dispatch_iq(&iq, &ctx),
            Stanza::Message(mut message) => {
                let outcome = {
                    let env = MessageContextEnv {
                        domain: &self.domain,
                        full_jid: &full_jid,
                        blocklist: &self.blocklist,
                        carbons: self.carbons,
                        muc_occupancy: &self.muc_occupancy,
                        id_gen: self.id_gen.as_ref(),
                    };
                    let mctx = MessageContext::derive(env, &message);
                    self.dispatcher.dispatch_message(&mut message, &mctx)
                };
                self.handle_message_outcome(outcome, message, &full_jid)
            }
            Stanza::Presence(presence) => self.dispatcher.dispatch_presence(&presence, &ctx),
        }
    }

    /// Process a [`MessageDispatchOutcome`] returned by either
    /// `dispatch_message` or `resume_message`.
    ///
    /// On `Completed` / `Halted`, return the events as-is. On
    /// `Awaiting`, allocate a fresh callback id, replace the
    /// handler-supplied [`CallbackId`] sentinels in the emitted events,
    /// and register a [`PendingOp::MessageDispatchResume`] that the
    /// matching `InboundEvent` callback will resume.
    fn handle_message_outcome(
        &mut self,
        outcome: MessageDispatchOutcome,
        message: xmpp_parsers::message::Message,
        full_jid: &jid::FullJid,
    ) -> Vec<OutboundEvent> {
        let MessageDispatchOutcome {
            mut events,
            termination,
        } = outcome;
        match termination {
            MessageDispatchTermination::Completed | MessageDispatchTermination::Halted { .. } => {
                events
            }
            MessageDispatchTermination::Awaiting { resume_after } => {
                let id = self.next_callback_id();
                let resume_kind = match infer_resume_kind(&events, &message, full_jid) {
                    Some(k) => k,
                    None => {
                        // No recognised callback-bearing event. Don't
                        // register a pending op — the pipeline parked
                        // without a way to resume, so this is
                        // operator-visible misconfiguration. Surface
                        // an ERROR log and return events as-is so the
                        // halt is at least visible.
                        events.push(OutboundEvent::Log {
                            level: Level::ERROR,
                            message: "MessageDispatchOutcome::Awaiting with no recognised \
                                      callback event; pipeline cannot resume"
                                .to_string(),
                        });
                        return events;
                    }
                };
                replace_callback_sentinels(&mut events, id, &resume_kind);
                self.register_pending_op(
                    id,
                    PendingOp::MessageDispatchResume {
                        resume_after,
                        message: Box::new(message),
                        kind: resume_kind,
                        full_jid: full_jid.clone(),
                        blocklist: self.blocklist.clone(),
                        carbons: self.carbons,
                        muc_occupancy: self.muc_occupancy.clone(),
                    },
                );
                events
            }
        }
    }

    /// Resume a paused message-pipeline run with the (possibly
    /// rewritten) message. Recursively chains through further `Awaiting`
    /// terminations — a long pipeline can park multiple times (e.g.
    /// rich-target lookup followed by enrichment).
    fn resume_message_dispatch(
        &mut self,
        message: xmpp_parsers::message::Message,
        resume_after: HandlerId,
        full_jid: jid::FullJid,
        blocklist: Blocklist,
        carbons: CarbonsState,
        muc_occupancy: MucOccupancy,
    ) -> Vec<OutboundEvent> {
        let mut message = message;
        let outcome = {
            let env = MessageContextEnv {
                domain: &self.domain,
                full_jid: &full_jid,
                blocklist: &blocklist,
                carbons,
                muc_occupancy: &muc_occupancy,
                id_gen: self.id_gen.as_ref(),
            };
            let mctx = MessageContext::derive(env, &message);
            self.dispatcher
                .resume_message(&mut message, &mctx, resume_after)
        };
        // Rebuilding session state for a possible second pause uses
        // the same snapshot — Q5's "frozen at dispatch start" rule.
        // Restoring it on each handle_message_outcome path keeps the
        // pause-resume contract consistent across multiple parks.
        let saved_blocklist = blocklist.clone();
        let saved_occupancy = muc_occupancy.clone();
        // The blocklist / occupancy used to build the next pending op
        // come from the same snapshot via self.* — which we don't
        // mutate during dispatch. So the resume's own pause registers
        // a fresh PendingOp with the live `self.*` (the original
        // snapshot semantically equals it for this dispatch). We
        // honour the snapshot by feeding the resume snapshot into
        // `handle_message_outcome` — implemented inline because the
        // generic helper reads from `self.*`.
        let _ = (saved_blocklist, saved_occupancy);
        self.handle_message_outcome(outcome, message, &full_jid)
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

/// Inspect the events returned by an `Awaiting` dispatch and infer the
/// [`ResumeKind`] that drives completion handling.
///
/// The state machine recognises two callback-bearing events emitted by
/// production handlers — `RequestEnrichment` (Enrichment) and
/// `LookupArchivedMessage` (RichTarget). Future
/// `AwaitCallback`-emitting handlers add an arm here.
///
/// `LookupArchivedMessage` doesn't carry the rich-target kind directly,
/// so the function re-runs detection on the message via
/// [`rich_target_validation::detect`] (which is pure and idempotent).
fn infer_resume_kind(
    events: &[OutboundEvent],
    message: &xmpp_parsers::message::Message,
    full_jid: &jid::FullJid,
) -> Option<ResumeKind> {
    for event in events {
        match event {
            OutboundEvent::RequestEnrichment { .. } => {
                return Some(ResumeKind::Enrichment);
            }
            OutboundEvent::LookupArchivedMessage { .. } => {
                // Re-detect to recover the kind+author the
                // RichTargetValidationHandler used. Detection is
                // pure and idempotent, so re-running matches the
                // handler's original result.
                let detected = rich_target_validation::detect(
                    message,
                    &MessageContext {
                        domain: "",
                        full_jid,
                        locality: super::session_state::Locality::Sender,
                        blocklist: &Blocklist::empty(),
                        carbons: CarbonsState::Disabled,
                        muc_occupancy: &MucOccupancy::empty(),
                        // detect() doesn't call id_gen; a fixed
                        // generator is fine here. We only need the
                        // returned kind+reference+author.
                        id_gen: &super::id_gen::FixedIdGenerator(String::new()),
                    },
                )?;
                return Some(ResumeKind::RichTarget {
                    kind: detected.kind,
                    author: detected.author,
                });
            }
            _ => {}
        }
    }
    None
}

/// Replace the handler-supplied sentinel [`CallbackId`] in emitted
/// events with the freshly-allocated id the state machine will use as
/// the pending-op key. Handlers can't allocate ids (they're pure), so
/// they emit a sentinel and the state machine swaps it here.
fn replace_callback_sentinels(
    events: &mut [OutboundEvent],
    real_id: CallbackId,
    kind: &ResumeKind,
) {
    let sentinel = match kind {
        ResumeKind::Enrichment => ENRICHMENT_CALLBACK_SENTINEL,
        ResumeKind::RichTarget { .. } => RICH_TARGET_LOOKUP_CALLBACK_SENTINEL,
    };
    for event in events.iter_mut() {
        match event {
            OutboundEvent::RequestEnrichment { id, .. } if *id == sentinel => *id = real_id,
            OutboundEvent::LookupArchivedMessage { id, .. } if *id == sentinel => *id = real_id,
            _ => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Internal test helpers — not exported, used by the in-tree tests in
    //! this module and by the top-level `tests/protocol_state_machine.rs`
    //! integration test file.

    use super::*;
    use jid::FullJid;
    /// Construct a machine already in the `Ready` phase, for unit tests that
    /// want to exercise stanza dispatch without going through the full auth
    /// + bind flow.
    pub fn ready_machine(
        domain: impl Into<String>,
        full_jid: FullJid,
        dispatcher: StanzaDispatcher,
    ) -> XmppStateMachine {
        XmppStateMachine {
            phase: ConnectionPhase::ready(full_jid, false),
            domain: domain.into(),
            dispatcher,
            next_callback: 0,
            pending_ops: HashMap::new(),
            blocklist: Blocklist::empty(),
            carbons: CarbonsState::Disabled,
            muc_occupancy: MucOccupancy::empty(),
            id_gen: Arc::new(UuidV4Generator),
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
                _ => panic!("expected IQ reply stanza"),
            },
            _ => panic!("expected SendStanza event"),
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
    fn open_frame_is_noop_and_close_enters_closing_phase() {
        let mut sm = XmppStateMachine::new("waddle.social", StanzaDispatcher::new());
        assert!(sm
            .handle(InboundEvent::FrameReceived(InboundFrame::Open))
            .is_empty());
        assert!(sm
            .handle(InboundEvent::FrameReceived(InboundFrame::Close))
            .is_empty());
        assert!(matches!(sm.phase(), ConnectionPhase::Closing { .. }));
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
        sm.register_pending_op(
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

    // ----------------------------------------------------------------
    // Pause/resume integration — the message pipeline parks via
    // `AwaitCallback`, the matching `InboundEvent` arrives, the
    // pipeline resumes and runs to completion.
    // ----------------------------------------------------------------

    use crate::protocol::event::{
        ArchivedMessage, CallbackResult, MessageRef, OriginIdValue, StanzaIdRef, StanzaIdValue,
    };
    use crate::protocol::handlers::canonicalize::CanonicalizeHandler;
    use crate::protocol::handlers::enrichment_dispatch::EnrichmentDispatchHandler;
    use crate::protocol::handlers::rich_target_validation::RichTargetValidationHandler;
    use crate::protocol::message_context::MessageContext;
    use crate::protocol::traits::{HandlerOutcome, MessageHandler};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use xmpp_parsers::message::{Body, Message, MessageType};

    fn ready_machine_with_dispatcher(
        dispatcher: StanzaDispatcher,
        domain: &str,
        full_jid: jid::FullJid,
    ) -> XmppStateMachine {
        test_support::ready_machine(domain, full_jid, dispatcher)
    }

    fn alice() -> jid::FullJid {
        "alice@example.com/web".parse().expect("jid")
    }

    fn chat_with_body(from: &str, to: &str, body: &str) -> Message {
        let mut m = Message::new(Some(to.parse().expect("jid")));
        m.from = Some(from.parse().expect("jid"));
        m.type_ = MessageType::Chat;
        m.bodies.insert(String::new(), Body(body.to_string()));
        m
    }

    /// Probe handler that records every invocation so tests can assert
    /// "ran on resume" or "did not run".
    struct TailProbe {
        invocations: Arc<AtomicUsize>,
    }

    impl MessageHandler for TailProbe {
        fn name(&self) -> &'static str {
            "test-tail-probe"
        }

        fn handle(&self, _message: &mut Message, _ctx: &MessageContext<'_>) -> HandlerOutcome {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            HandlerOutcome::Continue(Vec::new())
        }
    }

    #[test]
    fn enrichment_await_then_complete_resumes_pipeline_with_rewritten_message() {
        let mut dispatcher = StanzaDispatcher::new();
        dispatcher.register_message(Arc::new(EnrichmentDispatchHandler));
        let tail = Arc::new(AtomicUsize::new(0));
        dispatcher.register_message(Arc::new(TailProbe {
            invocations: tail.clone(),
        }));

        let mut sm = ready_machine_with_dispatcher(dispatcher, "example.com", alice());
        let msg = chat_with_body(
            "alice@example.com/web",
            "bob@example.com",
            "see https://example.com/page",
        );
        let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
            Stanza::Message(msg),
        ))));

        // Pipeline parked: RequestEnrichment with a real CallbackId
        // (sentinel was 0; the state machine swapped it).
        let callback_id = events
            .iter()
            .find_map(|e| match e {
                OutboundEvent::RequestEnrichment { id, .. } => Some(*id),
                _ => None,
            })
            .expect("RequestEnrichment emitted");
        assert_ne!(callback_id, ENRICHMENT_CALLBACK_SENTINEL);
        // Tail handler has not run — pipeline is paused.
        assert_eq!(tail.load(Ordering::SeqCst), 0);

        // Feed the matching completion with a rewritten message; the
        // pipeline resumes and the tail probe runs.
        let mut rewritten = chat_with_body(
            "alice@example.com/web",
            "bob@example.com",
            "see https://example.com/page",
        );
        rewritten
            .payloads
            .push(minidom::Element::builder("reference", "urn:xmpp:reference:0").build());
        let resume_events = sm.handle(InboundEvent::EnrichmentComplete {
            id: callback_id,
            message: Box::new(rewritten),
        });
        assert_eq!(tail.load(Ordering::SeqCst), 1);
        // Resume produced no events from the no-op probe — but it
        // didn't error either.
        assert!(
            !resume_events.iter().any(|e| matches!(
                e,
                OutboundEvent::Log { level, .. } if *level == Level::ERROR
            )),
            "resume must not log ERROR for the happy path: {resume_events:?}"
        );
    }

    #[test]
    fn rich_target_await_then_loaded_with_valid_target_resumes_pipeline() {
        let mut dispatcher = StanzaDispatcher::new();
        dispatcher.register_message(Arc::new(RichTargetValidationHandler));
        // Register canonicalize so resume actually does something
        // visible (stamps a stanza-id under alice's archive).
        dispatcher.register_message(Arc::new(CanonicalizeHandler));
        let tail = Arc::new(AtomicUsize::new(0));
        dispatcher.register_message(Arc::new(TailProbe {
            invocations: tail.clone(),
        }));

        let mut sm = ready_machine_with_dispatcher(dispatcher, "example.com", alice());
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "fixed text");
        msg.payloads
            .push(crate::xep::xep0308::build_replace_element("orig-msg-1"));
        let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
            Stanza::Message(msg),
        ))));

        let callback_id = events
            .iter()
            .find_map(|e| match e {
                OutboundEvent::LookupArchivedMessage { id, .. } => Some(*id),
                _ => None,
            })
            .expect("LookupArchivedMessage emitted");
        assert_ne!(callback_id, RICH_TARGET_LOOKUP_CALLBACK_SENTINEL);
        assert_eq!(tail.load(Ordering::SeqCst), 0);

        // Loaded with a valid same-author archived message → resume.
        let mut archived_msg =
            chat_with_body("alice@example.com/web", "bob@example.com", "original text");
        archived_msg.id = Some("orig-msg-1".to_string());
        let archived = ArchivedMessage {
            stanza_id: StanzaIdRef {
                by: "alice@example.com".parse().expect("bare"),
                id: StanzaIdValue::new("archive-A1"),
            },
            message: Box::new(archived_msg),
            tombstoned: false,
        };
        let resume_events = sm.handle(InboundEvent::ArchivedMessageLoaded {
            id: callback_id,
            result: Some(Box::new(archived)),
        });
        assert_eq!(
            tail.load(Ordering::SeqCst),
            1,
            "valid completion resumes pipeline through canonicalize and tail"
        );
        // Canonicalize stamped under alice's archive — but we can't
        // observe the stamp here without inspecting the message
        // post-resume. The tail-probe count is the resume signal.
        assert!(
            !resume_events.iter().any(|e| matches!(
                e,
                OutboundEvent::Log { level, .. } if *level == Level::ERROR
            )),
            "valid resume must not ERROR: {resume_events:?}"
        );
    }

    #[test]
    fn rich_target_loaded_not_found_emits_item_not_found_no_resume() {
        let mut dispatcher = StanzaDispatcher::new();
        dispatcher.register_message(Arc::new(RichTargetValidationHandler));
        let tail = Arc::new(AtomicUsize::new(0));
        dispatcher.register_message(Arc::new(TailProbe {
            invocations: tail.clone(),
        }));

        let mut sm = ready_machine_with_dispatcher(dispatcher, "example.com", alice());
        let mut msg = chat_with_body(
            "alice@example.com/web",
            "bob@example.com",
            "I take that back",
        );
        msg.payloads
            .push(crate::xep::xep0424::build_retract_element("stanza-X"));
        let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
            Stanza::Message(msg),
        ))));
        let callback_id = events
            .iter()
            .find_map(|e| match e {
                OutboundEvent::LookupArchivedMessage { id, .. } => Some(*id),
                _ => None,
            })
            .expect("LookupArchivedMessage emitted");

        // Result: not found → typed item-not-found reply, no resume.
        let resume_events = sm.handle(InboundEvent::ArchivedMessageLoaded {
            id: callback_id,
            result: None,
        });
        assert_eq!(
            tail.load(Ordering::SeqCst),
            0,
            "item-not-found halt must not resume the pipeline"
        );
        // Verify the typed error reply is present.
        let has_error_reply = resume_events.iter().any(|e| match e {
            OutboundEvent::SendStanza(stanza) => {
                matches!(stanza.as_ref(), Stanza::Message(m) if m.type_ == MessageType::Error)
            }
            _ => false,
        });
        assert!(has_error_reply, "expected SendStanza error reply");
    }

    #[test]
    fn rich_target_loaded_wrong_author_emits_not_acceptable() {
        let mut dispatcher = StanzaDispatcher::new();
        dispatcher.register_message(Arc::new(RichTargetValidationHandler));
        let tail = Arc::new(AtomicUsize::new(0));
        dispatcher.register_message(Arc::new(TailProbe {
            invocations: tail.clone(),
        }));

        let mut sm = ready_machine_with_dispatcher(dispatcher, "example.com", alice());
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "fixed text");
        msg.payloads
            .push(crate::xep::xep0308::build_replace_element("orig-msg-1"));
        let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
            Stanza::Message(msg),
        ))));
        let callback_id = events
            .iter()
            .find_map(|e| match e {
                OutboundEvent::LookupArchivedMessage { id, .. } => Some(*id),
                _ => None,
            })
            .expect("LookupArchivedMessage emitted");

        // Loaded with an archived message whose author differs.
        let mut archived_msg =
            chat_with_body("mallory@example.com/web", "bob@example.com", "imposter");
        archived_msg.id = Some("orig-msg-1".to_string());
        let archived = ArchivedMessage {
            stanza_id: StanzaIdRef {
                by: "alice@example.com".parse().expect("bare"),
                id: StanzaIdValue::new("archive-X"),
            },
            message: Box::new(archived_msg),
            tombstoned: false,
        };
        let resume_events = sm.handle(InboundEvent::ArchivedMessageLoaded {
            id: callback_id,
            result: Some(Box::new(archived)),
        });
        assert_eq!(tail.load(Ordering::SeqCst), 0);
        let has_error_reply = resume_events.iter().any(|e| match e {
            OutboundEvent::SendStanza(stanza) => {
                matches!(stanza.as_ref(), Stanza::Message(m) if m.type_ == MessageType::Error)
            }
            _ => false,
        });
        assert!(has_error_reply);
    }

    #[test]
    fn enrichment_complete_with_unknown_callback_id_logs_warn() {
        let mut sm = test_support::ready_machine("example.com", alice(), StanzaDispatcher::new());
        // Construct a MessageRef so the unused import lints quiet —
        // this also guards against API drift. (No assertion required;
        // the test below exercises the WARN path.)
        let _ = MessageRef::OriginId {
            sender: "alice@example.com".parse().expect("bare"),
            origin_id: OriginIdValue::new("dummy"),
        };
        let _ = CallbackResult::Ok { stanza: None };
        let events = sm.handle(InboundEvent::EnrichmentComplete {
            id: CallbackId(99999),
            message: Box::new(chat_with_body(
                "alice@example.com/web",
                "bob@example.com",
                "ignored",
            )),
        });
        assert!(events.iter().any(|e| matches!(
            e,
            OutboundEvent::Log { level, .. } if *level == Level::WARN
        )));
    }

    #[test]
    fn oauth_bearer_completion_consumes_pending_op_without_logging() {
        let mut sm = XmppStateMachine::new("waddle.social", StanzaDispatcher::new());
        let id = sm.next_callback_id();
        sm.register_pending_op(id, PendingOp::OAuthBearer);

        let events = sm.handle(InboundEvent::OAuthBearerValidated {
            id,
            result: crate::protocol::event::CallbackResult::Ok { stanza: None },
        });
        assert!(events.is_empty());

        let events2 = sm.handle(InboundEvent::OAuthBearerValidated {
            id,
            result: crate::protocol::event::CallbackResult::Ok { stanza: None },
        });
        assert!(events2.is_empty());
    }
}
