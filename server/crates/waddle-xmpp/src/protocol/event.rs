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
use crate::Stanza;
use jid::{BareJid, FullJid};
use tracing::Level;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;

/// Opaque identifier tying an async request to its eventual response.
///
/// Handlers emit outbound events containing a [`CallbackId`]; the interpreter
/// performs the async work and returns an inbound event carrying the same
/// id. The state machine stores per-id context until the response arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallbackId(pub u64);

/// Opaque identifier for a state-machine-owned timer.
///
/// Later migration steps use this for SCRAM timeouts and
/// XEP-0198 stream-management keep-alives.
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
    /// The transport reports it has been closed.
    TransportClosed,

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

/// Every effect the state machine can cause.
///
/// The interpreter resolves these against real-world resources (sockets,
/// `ConnectionRegistry`, `MucRoomRegistry`, `MamStorage`, `SfuServiceActor`,
/// etc.).
///
/// Each variant is a **typed** expression of intent — no `format!()` XML,
/// no string-keyed actor calls, no `xml: String` payloads. The decoupling
/// means new XEPs add new variants rather than growing a single monolithic
/// handler.
#[derive(Debug, Clone)]
pub enum OutboundEvent {
    // -------------------------------------------------------------------
    // Framing
    // -------------------------------------------------------------------
    /// Write a typed stanza to the transport. The interpreter serializes
    /// the stanza to its XML wire form at the I/O boundary.
    SendStanza(Box<Stanza>),
    /// Close the transport gracefully.
    CloseTransport,

    // -------------------------------------------------------------------
    // Routing (per-connection)
    // -------------------------------------------------------------------
    /// Deliver a stanza to exactly one connection identified by its full
    /// JID.
    ///
    /// The interpreter resolves `jid` against `ConnectionRegistry` and
    /// writes the stanza to that connection's outbound channel. If the
    /// target is offline the event is typically logged and dropped
    /// (standard XMPP offline-delivery semantics are archive-based, not
    /// routing-based).
    SendDirect { jid: FullJid, stanza: Box<Stanza> },
    /// Deliver a message to every occupant of a MUC room.
    ///
    /// The interpreter resolves occupancy via `MucRoomRegistry`. The
    /// `exclude` field suppresses delivery to a specific JID (typically
    /// the sender, to avoid duplicate echoes).
    BroadcastToRoom {
        room: BareJid,
        message: Box<Message>,
        exclude: Option<FullJid>,
    },

    // -------------------------------------------------------------------
    // Connection lifecycle (per-connection state in the interpreter)
    // -------------------------------------------------------------------
    /// Add this connection to the `ConnectionRegistry` under `jid`.
    ///
    /// Emitted at the end of resource binding.
    RegisterConnection(FullJid),
    /// Remove this connection from the `ConnectionRegistry`.
    ///
    /// Emitted on `TransportClosed` after the state machine collects any
    /// leave broadcasts.
    UnregisterConnection(FullJid),

    // -------------------------------------------------------------------
    // Storage
    // -------------------------------------------------------------------
    /// Persist a groupchat message to the MAM archive.
    ///
    /// The interpreter's MAM storage layer owns ID generation and indexing.
    ArchiveGroupchat {
        room: BareJid,
        sender: FullJid,
        message: Box<Message>,
    },
    /// Persist a one-to-one direct message to the MAM archive.
    ArchiveDirect {
        from: BareJid,
        to: BareJid,
        message: Box<Message>,
    },

    // -------------------------------------------------------------------
    // Async delegations (two-phase callback pattern — see plan §Design
    // patterns)
    // -------------------------------------------------------------------
    /// Ask the enrichment service to annotate a message with link
    /// previews. Result arrives as a future `InboundEvent`.
    RequestEnrichment {
        id: CallbackId,
        message: Box<Message>,
    },
    /// Send a Jingle IQ to the SFU actor. Result arrives as a future
    /// `InboundEvent`.
    AskSfu { id: CallbackId, iq: Box<Iq> },
    /// Run a MAM query against the archive. Result arrives as a future
    /// `InboundEvent`.
    QueryMam { id: CallbackId, iq: Box<Iq> },
    /// Load SCRAM credentials for `username` from `AppState`. Result
    /// arrives as a future `InboundEvent`.
    ///
    /// `username` is an opaque authentication identifier supplied by the
    /// SASL client — not yet a JID, so it is carried as a `String`. The
    /// interpreter's credential store resolves it to a typed identity
    /// before the completion callback fires.
    LoadScramCredentials { id: CallbackId, username: String },
    /// Validate an OAUTHBEARER token via `AppState::validate_session_token`.
    ///
    /// `token` is an opaque bearer credential (per RFC 6750 §2.1) and has
    /// no internal structure to model; it stays a `String` by design.
    ValidateOAuthBearer { id: CallbackId, token: String },

    // -------------------------------------------------------------------
    // Timers
    // -------------------------------------------------------------------
    /// Ask the interpreter to wake the state machine with
    /// `InboundEvent::Tick` after `duration`.
    SetTimer { id: TimerId, duration_ms: u64 },
    /// Cancel a previously-set timer.
    CancelTimer(TimerId),

    // -------------------------------------------------------------------
    // Diagnostics
    // -------------------------------------------------------------------
    /// Emit a log entry.
    ///
    /// Logging is modelled as an event (rather than calling `tracing::info!`
    /// directly from the state machine) so that tests can assert on it and
    /// the interpreter can route it through the application's log pipeline.
    /// `message` is free-form human-facing diagnostic text — the sole
    /// legitimate `String` payload under the typed-payloads rule.
    Log { level: Level, message: String },
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
}
