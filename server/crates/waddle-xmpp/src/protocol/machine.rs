//! Pure, synchronous XMPP state machine.
//!
//! The core method is [`XmppStateMachine::handle`], which is a pure
//! synchronous function from an [`InboundEvent`] + current state to a
//! `Vec<OutboundEvent>`. No async, no I/O, no mocks required in tests.
//!
//! This module owns only *per-connection* state. Cross-connection state
//! (connection registry, MUC room occupancy, MAM archive) is resolved by
//! the interpreter using the events emitted from here.

mod completion;
mod message_pipeline;
mod pending_op;

use super::dispatch::StanzaDispatcher;
use super::event::{CallbackId, InboundEvent, OutboundEvent, StanzaContext};
use super::frame::InboundFrame;
use super::id_gen::{IdGenerator, UuidV4Generator};
use super::message_context::{MessageContext, MessageContextEnv};
use super::phase::ConnectionPhase;
use super::session_state::{Blocklist, CarbonsState, MucOccupancy};
use crate::Stanza;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::Level;

pub use pending_op::{PendingOp, ResumeKind};

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
    /// Whether this state machine currently represents a live client
    /// transport. Headless offline-recipient passes flip this to `false`
    /// so delivery-only XEPs do not fire.
    has_live_transport: bool,
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
            has_live_transport: true,
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

    /// Transition this state machine into [`ConnectionPhase::Ready`]
    /// for `full_jid`. Used by the WebSocket transport adapter
    /// (#229 PR11) to mirror the per-connection
    /// [`crate::registry::ConnectionRegistry`] bind transition into
    /// the state machine so subsequent
    /// [`InboundEvent::StanzaFromPeer`] / [`InboundEvent::FrameReceived`]
    /// events dispatch through the message-pipeline chain instead of
    /// being rejected with a "before authentication" warning.
    ///
    /// `resumed` should be `true` when the bind happens via XEP-0198
    /// resume, `false` for a fresh bind. This matches the
    /// `ConnectionPhase::ready` constructor's signature and lets the
    /// SM observe the same metadata the legacy phase tracker uses.
    pub fn transition_to_ready(&mut self, full_jid: jid::FullJid, resumed: bool) {
        self.phase = ConnectionPhase::ready(full_jid, resumed);
    }

    /// Transition this state machine into [`ConnectionPhase::Closing`].
    /// Used by the transport adapter on connection teardown so the SM
    /// rejects late stanzas with the same diagnostic the legacy path
    /// produces.
    pub fn transition_to_closing(&mut self) {
        let bound = self.phase.bound_jid().cloned();
        self.phase = ConnectionPhase::closing(bound);
    }

    /// Mark whether this machine currently represents a live client
    /// transport.
    pub fn set_has_live_transport(&mut self, has_live_transport: bool) {
        self.has_live_transport = has_live_transport;
    }

    /// Replace the per-connection XEP-0191 blocklist snapshot.
    ///
    /// Called by the transport adapter immediately after
    /// [`Self::transition_to_ready`] to seed the SM with the bound
    /// user's persisted blocklist, loaded once from
    /// [`crate::storage`]-equivalent persistence. Per #229 Q5, the
    /// blocklist is **frozen** as a session-state snapshot for the
    /// duration of this session: every
    /// [`super::message_context::MessageContext`] derived during a
    /// dispatch (including across pause/resume hops) reads from this
    /// snapshot, never from the live database. Mutations driven by
    /// XEP-0191 IQ handlers during the session update the SM's
    /// internal blocklist; persistence-layer mutations from other
    /// resources of the same user reflect on the *next* bind, not
    /// mid-session.
    pub fn set_blocklist(&mut self, blocklist: Blocklist) {
        self.blocklist = blocklist;
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
                // Server-stamp the authenticated sender on every
                // client-originated message stanza (RFC 6120 §8.1.2.1
                // / RFC 6121). Clients SHOULD NOT set `from`, and even
                // when they do the server is free to override it with
                // the resource-bound full JID. This guarantees
                // [`Locality::derive`]'s `from`-match resolves to
                // `Sender` for outgoing messages and the dispatcher
                // chain (blocking, archive, route, …) sees a
                // canonical sender.
                message.from = Some(jid::Jid::from(full_jid.clone()));
                // Hot path: borrow `self.*` directly during the
                // synchronous dispatch. The session-state snapshot is
                // only materialized (cloned) when the pipeline parks
                // and we need to stash it in `PendingOp`; for stanzas
                // that complete synchronously (the common case), no
                // clone happens at all.
                let outcome = {
                    let env = MessageContextEnv {
                        domain: &self.domain,
                        full_jid: &full_jid,
                        blocklist: &self.blocklist,
                        carbons: self.carbons,
                        muc_occupancy: &self.muc_occupancy,
                        has_live_transport: self.has_live_transport,
                        id_gen: self.id_gen.as_ref(),
                    };
                    let mctx = MessageContext::derive(env, &message);
                    self.dispatcher.dispatch_message(&mut message, &mctx)
                };
                self.handle_message_outcome(outcome, message, &full_jid, None)
            }
            Stanza::Presence(presence) => self.dispatcher.dispatch_presence(&presence, &ctx),
        }
    }

    fn on_peer_stanza(&mut self, stanza: Stanza) -> Vec<OutboundEvent> {
        // Stanzas delivered from other connections via the registry
        // run the **recipient-pass** of the message pipeline. The
        // [`MessageContext::derive`] locality computation handles the
        // sender/recipient asymmetry — for a peer stanza arriving on
        // bob's machine, `(full_jid=bob, from=alice, to=bob)` derives
        // `Locality::Recipient` and the chain emits the recipient-side
        // archive write, recipient-side `<stanza-id by=bob/>` stamp,
        // received-carbon fan-out, recipient-side inbox row, and a
        // final wire write.
        //
        // IQ and presence peer routing arrive in later migration steps
        // (issue #229 PR10 for groupchat, follow-ups for IQ).
        let full_jid = match &self.phase {
            ConnectionPhase::Ready { full_jid, .. } => full_jid.clone(),
            ConnectionPhase::Unauthenticated
            | ConnectionPhase::ScramPending { .. }
            | ConnectionPhase::Authenticated { .. }
            | ConnectionPhase::Closing { .. } => {
                return vec![OutboundEvent::Log {
                    level: Level::WARN,
                    message: format!(
                        "Peer-routed {} stanza arrived before this connection \
                         reached Ready; dropping",
                        stanza.name()
                    ),
                }];
            }
        };

        match stanza {
            Stanza::Message(mut message) => {
                let outcome = {
                    let env = MessageContextEnv {
                        domain: &self.domain,
                        full_jid: &full_jid,
                        blocklist: &self.blocklist,
                        carbons: self.carbons,
                        muc_occupancy: &self.muc_occupancy,
                        has_live_transport: self.has_live_transport,
                        id_gen: self.id_gen.as_ref(),
                    };
                    // Peer stanzas are by definition recipient-pass —
                    // the sender pass already ran on the originating
                    // connection and the routing layer produced this
                    // delivery. Override the derived locality so the
                    // self-loop edge case (alice/web → alice/web,
                    // which `Locality::derive` classifies as `Both`)
                    // can't trip [`RouteHandler`]'s `Both` branch into
                    // re-emitting `RouteToConnection` and looping back
                    // through routing. The recipient pass terminates
                    // with `SendStanza` to bob's wire, full stop.
                    let mut mctx = MessageContext::derive(env, &message);
                    mctx.locality = super::session_state::Locality::Recipient;
                    self.dispatcher.dispatch_message(&mut message, &mctx)
                };
                self.handle_message_outcome(outcome, message, &full_jid, None)
            }
            Stanza::Iq(iq) => {
                // IQs routed peer-to-peer (e.g. Jingle session-initiate
                // after the server's namespace handler has rewritten the
                // Waddle transport) bypass the recipient-pass message
                // pipeline — they're application-specific and have no
                // archive/inbox/incoming-block semantics. Write straight
                // to the responder's wire so they can emit the
                // XEP-0166 §6.4 IQ-result themselves.
                vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(iq)))]
            }
            other => vec![OutboundEvent::Log {
                level: Level::DEBUG,
                message: format!(
                    "Peer-routed {} stanza (only message + iq pipelines wired so far)",
                    other.name()
                ),
            }],
        }
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
pub(crate) mod test_support;

#[cfg(test)]
mod tests;
