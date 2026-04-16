//! Events flowing in and out of the XMPP state machine.
//!
//! The state machine consumes [`InboundEvent`] and emits
//! [`OutboundEvent`]. Side effects are performed by a transport-specific
//! interpreter, never inside the state machine itself.

use super::frame::InboundFrame;
use crate::connection::Stanza;
use jid::{BareJid, FullJid};
use tracing::Level;

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
/// New variants are added in later migration steps as more of the protocol
/// moves into the state machine:
/// - `Tick(Instant)` — for SCRAM/SM timeouts
/// - `EnrichmentComplete { id, message }` — GitHub link enrichment
/// - `SfuResponse { id, response }` — Jingle IQ delegation
/// - `MamQueryComplete { id, result }` — XEP-0313 query result
/// - `ScramCredentialsLoaded { id, result }` — SASL credential lookup
/// - `OAuthBearerValidated { id, result }` — token validation
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
    // [`OutboundEvent`] by [`CallbackId`]). Each variant carries an
    // `xml` payload that the completion handler can either forward as
    // a reply frame or use as raw data for further processing.
    // -------------------------------------------------------------------
    /// Result of an earlier [`OutboundEvent::RequestEnrichment`] — the
    /// enricher has finished annotating the message with link previews,
    /// OGP metadata, etc., and returns the rewritten stanza as XML.
    EnrichmentComplete { id: CallbackId, xml: String },
    /// Result of an earlier [`OutboundEvent::AskSfu`] — the SFU actor
    /// has produced a reply Jingle IQ for us to forward to the client.
    SfuResponse {
        id: CallbackId,
        result: CallbackResult,
    },
    /// Result of an earlier [`OutboundEvent::QueryMam`] — the archive
    /// has finished a window-size query and returns the matching
    /// stanzas plus a final fin IQ as XML blobs.
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
/// this envelope. Handlers don't care what the concrete error type was;
/// they only need to know whether to render a stanza-error response or
/// continue the happy path. Richer typed payloads (e.g., a parsed MAM
/// result set) can be carried as `xml` and re-parsed in the completion
/// handler, or — in a later iteration — swapped for a typed payload
/// enum without breaking the shape.
#[derive(Debug, Clone)]
pub enum CallbackResult {
    /// Successful completion. The `xml` is an outbound stanza ready to
    /// be forwarded, or empty when the result is acknowledgement-only.
    Ok { xml: String },
    /// The async operation failed. The `xml` is a pre-built stanza
    /// error response if the caller emitted one, otherwise a free-form
    /// diagnostic message.
    Err { xml: String },
}

/// Every effect the state machine can cause.
///
/// The interpreter resolves these against real-world resources (sockets,
/// `ConnectionRegistry`, `MucRoomRegistry`, `MamStorage`, `SfuServiceActor`,
/// etc.).
///
/// Each variant is a **typed** expression of intent — no `format!()` XML,
/// no string-keyed actor calls. The decoupling means new XEPs add new
/// variants rather than growing a single monolithic handler.
#[derive(Debug, Clone)]
pub enum OutboundEvent {
    // -------------------------------------------------------------------
    // Framing
    // -------------------------------------------------------------------
    /// Write a serialized stanza (XML text) to the transport.
    SendFrame(String),
    /// Close the transport gracefully.
    CloseTransport,

    // -------------------------------------------------------------------
    // Routing (per-connection)
    // -------------------------------------------------------------------
    /// Deliver a stanza to exactly one connection identified by its full
    /// JID.
    ///
    /// The interpreter resolves `jid` against `ConnectionRegistry` and
    /// writes the XML to that connection's outbound channel. If the
    /// target is offline the event is typically logged and dropped
    /// (standard XMPP offline-delivery semantics are archive-based, not
    /// routing-based).
    SendDirect { jid: FullJid, xml: String },
    /// Deliver a stanza to every occupant of a MUC room.
    ///
    /// The interpreter resolves occupancy via `MucRoomRegistry`. The
    /// `exclude` field suppresses delivery to a specific JID (typically
    /// the sender, to avoid duplicate echoes).
    BroadcastToRoom {
        room: BareJid,
        xml: String,
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
    /// The payload is a pre-serialized `<message/>` XML string; the
    /// interpreter's MAM storage layer owns ID generation and indexing.
    ArchiveGroupchat {
        room: BareJid,
        sender: FullJid,
        xml: String,
    },
    /// Persist a one-to-one direct message to the MAM archive.
    ArchiveDirect {
        from: BareJid,
        to: BareJid,
        xml: String,
    },

    // -------------------------------------------------------------------
    // Async delegations (two-phase callback pattern — see plan §Design
    // patterns)
    // -------------------------------------------------------------------
    /// Ask the enrichment service to annotate a message with link
    /// previews. Result arrives as a future `InboundEvent`.
    RequestEnrichment { id: CallbackId, xml: String },
    /// Send a Jingle IQ to the SFU actor. Result arrives as a future
    /// `InboundEvent`.
    AskSfu { id: CallbackId, xml: String },
    /// Run a MAM query against the archive. Result arrives as a future
    /// `InboundEvent`.
    QueryMam { id: CallbackId, xml: String },
    /// Load SCRAM credentials for `username` from `AppState`. Result
    /// arrives as a future `InboundEvent`.
    LoadScramCredentials { id: CallbackId, username: String },
    /// Validate an OAUTHBEARER token via `AppState::validate_session_token`.
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
    Log { level: Level, message: String },
}

/// Read-only context supplied to [`super::traits::IqHandler::handle`] and
/// the analogous message/presence handlers.
///
/// Deliberately does **not** hold registries, storage, or actor handles.
/// Handlers are pure — they emit events and the interpreter resolves them.
#[derive(Debug, Clone, Copy)]
pub struct IqContext<'a> {
    /// The server's own domain (e.g. `"waddle.social"`).
    pub domain: &'a str,
    /// The currently authenticated full JID of the connection owner.
    pub full_jid: &'a jid::FullJid,
}
