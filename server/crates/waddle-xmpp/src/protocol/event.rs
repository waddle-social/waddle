//! Events flowing in and out of the XMPP state machine.
//!
//! The state machine consumes [`InboundEvent`] and emits
//! [`OutboundEvent`]. Side effects are performed by a transport-specific
//! interpreter, never inside the state machine itself.
//!
//! # Typed payloads
//!
//! Per the *Typed-payloads hard rule* in `CLAUDE.md`, protocol data on
//! every event variant is a typed Rust value — [`Stanza`],
//! [`xmpp_parsers::iq::Iq`], [`xmpp_parsers::message::Message`], [`FullJid`],
//! etc. — never a `String` carrying serialized XML. Serialization to the
//! wire format happens exactly once, in the transport interpreter, at the
//! I/O boundary. Parsing from the wire format happens exactly once, in
//! [`super::frame::parse_frame`], before any event enters the state
//! machine.

use super::frame::InboundFrame;
use crate::xep::{CallThreadKind, CallThreadMedia};
use crate::Stanza;
use jid::{FullJid, Jid};
use waddle_sfu::MediaCapabilities;
use waddle_xmpp_core::xep0359::{OriginId, StanzaId};
use xmpp_parsers::message::Message;

mod outbound;

pub use outbound::OutboundEvent;

/// Reference to a previously-archived message.
///
/// Used by [`OutboundEvent::LookupArchivedMessage`] (issue #229 PR3 — rich
/// target validation for XEP-0308 / 0424 / 0425 / 0461). The variants are
/// split rather than collapsed into a single struct because they index
/// physically different storage paths:
///
/// - [`MessageRef::StanzaId`] is keyed on `(archive, id)` — what
///   XEP-0359 §5 stamps and what XEP-0424 retractions / XEP-0461 replies
///   reference. Carries the canonical [`StanzaId`] so the stamped id is
///   inseparable from its assigning archive (`by`) — see issue #329.
/// - [`MessageRef::OriginId`] is keyed on `(sender, origin_id)` — what
///   XEP-0359 §3 origin-ids carry and what XEP-0308 corrections may use as
///   a fallback when the original message hasn't yet been seen by stanza-id.
#[derive(Debug, Clone)]
pub enum MessageRef {
    /// XEP-0359 stable stanza-id, scoped to its stamping archive.
    StanzaId {
        /// The canonical [`StanzaId`] — opaque id paired with its
        /// stamping archive (`by`). Use the bare archive JID (user bare
        /// JID for 1:1, room JID for groupchat) when constructing.
        stanza_id: StanzaId,
    },
    /// XEP-0359 origin-id, scoped to the original sender.
    OriginId {
        /// Original sender identity: bare account JID in personal archives,
        /// exact full occupant JID in room archives.
        sender: Jid,
        /// The client-supplied opaque origin-id value (canonical
        /// [`OriginId`] from `waddle-xmpp-core`).
        origin_id: OriginId,
    },
}

/// XEP-0280 carbon copy variant — sent vs received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarbonKind {
    /// Mirror of an outgoing message (`<sent xmlns='urn:xmpp:carbons:2'>`)
    /// fanned out to the sender's other resources.
    Sent,
    /// Mirror of an incoming message (`<received xmlns='urn:xmpp:carbons:2'>`)
    /// fanned out to the recipient's other resources.
    Received,
}

/// Placeholder for the typed archived-message payload from issue #228.
///
/// PR1 of issue #229 reserves the variant slots for the
/// [`InboundEvent::ArchivedMessageLoaded`] callback shape; the typed
/// payload lands with #228. We model the placeholder as a newtype so
/// future field additions don't reshape every callsite, and so the
/// public surface stays grep-able.
#[derive(Debug, Clone)]
pub struct ArchivedMessage {
    /// XEP-0359 stamped stanza-id (canonical [`StanzaId`] from
    /// `waddle-xmpp-core`).
    pub stanza_id: StanzaId,
    /// The archived message stanza, typed.
    pub message: Box<Message>,
    /// XEP-0424 tombstone state — `true` when this archive entry has
    /// already been retracted. Used by `RichTargetValidationHandler` (PR3)
    /// to reject a redundant retraction with `<bad-request>`.
    pub tombstoned: bool,
}

/// Opaque identifier tying an async request to its eventual response.
///
/// Handlers emit outbound events containing a [`CallbackId`]; the interpreter
/// performs the async work and returns an inbound event carrying the same
/// id. The state machine stores per-id context until the response arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallbackId(pub u64);

/// Opaque identifier for a state-machine-owned timer.
///
/// The RFC 7395 keepalive clock reserves
/// [`crate::protocol::keepalive::KEEPALIVE_TIMER`]; later migration
/// steps allocate distinct ids for SCRAM timeouts and XEP-0198 ack
/// deadlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerId(pub u64);

/// Everything the state machine can react to.
///
/// Every variant carries typed protocol data — no raw XML strings.
#[derive(Debug)]
pub enum InboundEvent {
    /// A parsed frame arrived from the transport.
    FrameReceived(InboundFrame),
    /// Another connection's routing layer delivered a stanza to us.
    ///
    /// Boxed to keep `InboundEvent` small (see `InboundFrame::Stanza`).
    StanzaFromPeer(Box<Stanza>),
    /// The transport reports it is up and ready to carry frames (WS
    /// upgrade complete). Fed exactly once, before any client frame —
    /// arms the RFC 7395 §3.8 keepalive clock so even a connection
    /// that never authenticates is reaped by the liveness policy.
    TransportReady,
    /// The transport reports it has been closed.
    TransportClosed,
    /// A timer previously armed via [`OutboundEvent::SetTimer`] fired.
    Tick(TimerId),
    /// Transport-level liveness evidence. Under the issue #1090 policy
    /// *any* inbound frame proves the peer alive, so the WebSocket
    /// adapter feeds this for **every** received frame — text, binary,
    /// client-initiated ping, and pong (any payload) — because most
    /// frames are dispatched on the legacy path and never reach the
    /// machine as [`InboundEvent::FrameReceived`]. `FrameReceived`
    /// also marks the peer alive for the frames that *do* take the
    /// machine path; the double-mark is idempotent.
    KeepaliveAck,

    // -------------------------------------------------------------------
    // Async callback completions (matched to a previously emitted
    // [`OutboundEvent`] by [`CallbackId`]). Every completion carries a
    // typed payload — enrichment returns the rewritten `<message>`, the
    // SFU returns its reply `<iq>`, and so on.
    // -------------------------------------------------------------------
    /// Result of an earlier [`OutboundEvent::RequestEnrichment`] — the
    /// enricher has finished annotating the message with link previews,
    /// OGP metadata, etc., and returns the rewritten stanza.
    EnrichmentComplete {
        id: CallbackId,
        message: Box<Message>,
    },
    /// Result of an earlier [`OutboundEvent::AskSfu`] — the SFU actor
    /// has produced a reply Jingle IQ for us to forward to the client.
    SfuResponse {
        id: CallbackId,
        result: CallbackResult,
    },
    /// Result of an earlier [`OutboundEvent::QueryMam`] — the archive
    /// has finished a window-size query and returns a pre-built reply
    /// IQ (typically an `<iq type="result">` carrying the fin element).
    MamQueryComplete {
        id: CallbackId,
        result: CallbackResult,
    },
    /// Result of an earlier [`OutboundEvent::LoadScramCredentials`].
    ScramCredentialsLoaded {
        id: CallbackId,
        result: CallbackResult,
    },
    /// Result of an earlier [`OutboundEvent::ValidateOAuthBearer`].
    OAuthBearerValidated {
        id: CallbackId,
        result: CallbackResult,
    },
    /// Result of an earlier [`OutboundEvent::LookupArchivedMessage`].
    ///
    /// `result` is `None` when the archive has no entry matching the
    /// requested [`MessageRef`] — the handler treats that as
    /// `<item-not-found>` per XEP-0308 / 0424 / 0425 / 0461.
    ArchivedMessageLoaded {
        id: CallbackId,
        result: Option<Box<ArchivedMessage>>,
    },
}

/// Uniform success-or-error shape for callback completions.
///
/// Every async delegation emitted by a handler returns a result using
/// this envelope with a *typed* stanza payload — never raw XML. The
/// completion handler either forwards the stanza verbatim or extracts
/// typed values from it directly.
#[derive(Debug, Clone)]
pub enum CallbackResult {
    /// Successful completion. `stanza` is the reply to forward to the
    /// peer, or `None` when the result is acknowledgement-only.
    Ok { stanza: Option<Box<Stanza>> },
    /// The async operation failed. `stanza` is the pre-built stanza-error
    /// reply to forward to the peer.
    Err { stanza: Box<Stanza> },
}

/// Thread metadata accompanying an [`OutboundEvent::ProjectGroupchatInbox`]
/// projection.
///
/// Distinct from the wire `<thread/>` element so the interpreter doesn't
/// have to re-walk the typed message looking for the thread id at
/// projection time. `title` and `author_nick` are pre-derived by the
/// chain handler that emits the event (Waddle forum-thread metadata
/// or the first-message preview).
#[derive(Debug, Clone)]
pub struct GroupchatThreadProjection {
    /// Wire `<thread/>` parent identifier.
    pub thread_id: String,
    /// Thread title — `Some(...)` when extracted from a Waddle
    /// `CreateThread` action or fallback message preview.
    pub title: Option<String>,
    /// Thread author nickname (the resource component of `from`).
    pub author_nick: Option<String>,
    /// Call-thread anchor kind — `Some(...)` only when this projection
    /// is a thread root carrying a `urn:waddle:call-thread:0` anchor
    /// marker (a DM or MUC call session). Replies never carry it.
    pub call_thread_kind: Option<CallThreadKind>,
    /// Call-thread media flags from the anchor marker — present iff
    /// `call_thread_kind` is present.
    pub call_thread_media: Option<CallThreadMedia>,
}

/// Read-only context supplied to every stanza handler
/// ([`super::traits::IqHandler`], [`super::traits::MessageHandler`], and
/// [`super::traits::PresenceHandler`]).
///
/// The context carries only authenticated session data (server domain and the
/// full JID of the connection owner) — it deliberately does **not** hold
/// registries, storage, or actor handles. Handlers are pure: they emit events
/// and the interpreter resolves them.
#[derive(Debug, Clone, Copy)]
pub struct StanzaContext<'a> {
    /// The server's own domain (e.g. `"waddle.social"`).
    ///
    /// Borrowed from `AppState`'s static configuration; not a dynamic
    /// protocol value, hence `&str` rather than a parsed JID component.
    pub domain: &'a str,
    /// The currently authenticated full JID of the connection owner.
    pub full_jid: &'a FullJid,
    /// Media grants derived by the websocket layer's Muji gate from
    /// the sender's current XEP-0045 role, at the moment the gate
    /// authorized this stanza. `Some` only for Muji Jingle IQs that
    /// passed the occupancy check; `None` everywhere else (non-call
    /// stanzas, 1:1 Jingle, and dispatch paths with no gate). The
    /// Jingle handler's Muji mint consumes this — absence on that
    /// path fails closed to listen-only grants.
    pub media_capabilities: Option<MediaCapabilities>,
}
