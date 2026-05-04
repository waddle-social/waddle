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
use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use tracing::Level;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;

/// XEP-0359 stamped stanza-id value.
///
/// Newtype around the opaque id value so a stanza-id cannot be silently
/// swapped for an origin-id, message id, or any other XMPP identifier
/// crossing event boundaries. Per XEP-0359 §6 the value itself is
/// opaque (no internal structure), but the *kind* is type-significant
/// — typed-payloads hard rule (CLAUDE.md).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StanzaIdValue(String);

impl StanzaIdValue {
    /// Wrap an opaque id value coming from an [`super::id_gen::IdGenerator`]
    /// or from a parsed XEP-0359 `<stanza-id/>` element.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the underlying value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for StanzaIdValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// XEP-0359 client-supplied origin-id value.
///
/// Newtype companion to [`StanzaIdValue`] for the same reasons; an
/// origin-id is owned by the originating client and is a different
/// identity space from the server-stamped stanza-id, so it must not be
/// confused at handler/storage boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OriginIdValue(String);

impl OriginIdValue {
    /// Wrap an origin-id value parsed from a XEP-0359 `<origin-id/>`
    /// element.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the underlying value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OriginIdValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Reference to a previously-archived message.
///
/// Used by [`OutboundEvent::LookupArchivedMessage`] (issue #229 PR3 — rich
/// target validation for XEP-0308 / 0424 / 0425 / 0461). The variants are
/// split rather than collapsed into a single struct because they index
/// physically different storage paths:
///
/// - [`MessageRef::StanzaId`] is keyed on `(archive, id)` — what
///   XEP-0359 §5 stamps and what XEP-0424 retractions / XEP-0461 replies
///   reference.
/// - [`MessageRef::OriginId`] is keyed on `(sender, origin_id)` — what
///   XEP-0359 §3 origin-ids carry and what XEP-0308 corrections may use as
///   a fallback when the original message hasn't yet been seen by stanza-id.
#[derive(Debug, Clone)]
pub enum MessageRef {
    /// XEP-0359 stable stanza-id, scoped to its stamping archive.
    StanzaId {
        /// `by=` of the stamping archive (user bare JID for 1:1, room JID
        /// for groupchat).
        by: BareJid,
        /// The stamped opaque id value (typed via [`StanzaIdValue`] so
        /// it cannot be confused with an origin-id at call boundaries).
        id: StanzaIdValue,
    },
    /// XEP-0359 origin-id, scoped to the original sender.
    OriginId {
        /// Bare JID of the original sender.
        sender: BareJid,
        /// The client-supplied opaque origin-id value (typed via
        /// [`OriginIdValue`]).
        origin_id: OriginIdValue,
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

/// Typed reference to a stanza-id stamped by a specific archive.
///
/// Identical shape to [`MessageRef::StanzaId`] but kept separate because
/// `StanzaIdRef` is an output (e.g. an inbox row links to its archived
/// counterpart) whereas `MessageRef` is an input (a handler asks the
/// archive to look up something).
#[derive(Debug, Clone)]
pub struct StanzaIdRef {
    /// `by=` of the stamping archive.
    pub by: BareJid,
    /// The stamped opaque id value, typed via [`StanzaIdValue`] so it
    /// cannot be confused with an origin-id at handler / storage
    /// boundaries.
    pub id: StanzaIdValue,
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
    /// XEP-0359 stamped stanza-id.
    pub stanza_id: StanzaIdRef,
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
    /// Route a stanza to another local connection's state machine.
    ///
    /// `jid` is a typed [`jid::Jid`] — full when the handler can pin a
    /// specific resource, bare when it cannot. The interpreter performs
    /// resource selection against `ConnectionRegistry` (RFC 6121 §8.5
    /// delivery semantics: bare delivers to highest-priority resources;
    /// full delivers to that exact resource).
    ///
    /// Carrying a typed `Jid` instead of a `FullJid` keeps the
    /// typed-payloads hard rule honest — the prior shape forced
    /// handlers to synthesize a fake full JID via
    /// `format!("{}/", bare)` + `parse`, which violates the rule and
    /// produces an invalid resource.
    ///
    /// **Migration status (issue #229 PR1)**: the variant is renamed
    /// from `SendDirect` and the protocol-level intent is described
    /// below, but the live `waddle-server` interpreter still implements
    /// the legacy "write directly to the peer's outbound channel"
    /// behaviour to keep the existing integration tests green during
    /// the staged migration. The semantic change to "feed the
    /// destination's state machine via
    /// [`InboundEvent::StanzaFromPeer`]" lands in PR5 alongside the
    /// `message.rs` cutover, at which point the recipient pipeline
    /// (XEP-0191 incoming block, XEP-0359 recipient stamp, XEP-0313
    /// archive, XEP-0280 received-carbons, inbox projection) starts
    /// running on the destination side.
    ///
    /// **Intended semantic (PR5 onward)**: the interpreter resolves
    /// `jid` against `ConnectionRegistry` and feeds the stanza into
    /// the destination connection's machine as
    /// [`InboundEvent::StanzaFromPeer`]. The destination's
    /// recipient-pass pipeline runs and ultimately emits
    /// [`OutboundEvent::SendStanza`] to the destination's wire.
    ///
    /// If the target is offline the event is logged and dropped (XMPP
    /// offline-delivery semantics are archive-based, not
    /// routing-based).
    RouteToConnection { jid: jid::Jid, stanza: Box<Stanza> },
    /// Hand a `<message type='groupchat'>` to the room handler chain
    /// (Option C — issue #229 Q7) for occupancy validation,
    /// XEP-0359/XEP-0421 stamping, XEP-0313 §5.1.3 archiving, and
    /// per-occupant fan-out.
    ///
    /// The interpreter resolves the per-room actor against the room
    /// registry, asks for a frozen `RoomChainSnapshot`, builds a
    /// `RoomContext`, and runs `default_room_dispatcher().dispatch(...)`.
    /// Emitted events are recursively interpreted in the same call.
    DispatchToRoom {
        room: BareJid,
        message: Box<Message>,
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
    /// `sender_nickname_generation` is the per-XEP-0308 §3 nickname
    /// generation captured at dispatch start (carried through the
    /// chain via `RoomContext`) so the archive arm can stamp the
    /// archive row without a second `RoomActor::GetRoomSnapshot`
    /// round-trip (Copilot review on PR #279).
    ArchiveGroupchat {
        room: BareJid,
        sender: FullJid,
        message: Box<Message>,
        sender_nickname_generation: u64,
    },
    /// Persist a one-to-one direct message to the MAM archive.
    ///
    /// `archive_jid` identifies which personal archive to write to —
    /// the locality-aware [`super::handlers::archive::ArchiveHandler`]
    /// emits this field as the local user's bare JID, so the interpreter
    /// is dumb glue that does not need to reason about sender/recipient
    /// pass semantics. `from` and `to` carry the canonical message tuple
    /// for telemetry and remain on the typed `message` payload.
    ArchiveDirect {
        archive_jid: BareJid,
        from: BareJid,
        to: BareJid,
        message: Box<Message>,
    },
    /// Project a message into the local user's inbox (Waddle conversation
    /// summary). `archive_ref` links the inbox row to its MAM entry so
    /// clients can pivot to the archived stanza using the same XEP-0359
    /// stanza-id space.
    ///
    /// `increment_unread` is set by the locality-aware
    /// [`super::handlers::inbox::InboxHandler`]: `true` on the recipient
    /// pass (the message is *new* for this owner), `false` on the sender
    /// pass (it's the owner's own outgoing copy and shouldn't bump
    /// their unread count).
    ///
    /// Inbox is not a finalized XEP — this is a Waddle product surface;
    /// the field set is engineering, not protocol-mandated.
    ProjectInbox {
        owner: BareJid,
        peer: BareJid,
        message: Box<Message>,
        archive_ref: StanzaIdRef,
        increment_unread: bool,
    },
    /// XEP-0045 §8.1 subject-change persistence. Emitted by the room
    /// handler chain's subject handler when an authorized occupant has
    /// successfully changed the room subject. The interpreter forwards
    /// this to the room actor, which writes a `SubjectState` onto
    /// `MucRoom.subject`. The replay on next join is what produces the
    /// XEP-0045 §7.2.15 historical-subject emission with the right
    /// setter, timestamp, and XEP-0421 occupant-id derivation.
    ///
    /// **Ordering.** The room chain emits this event **before** the
    /// reflector's `OutboundEvent::RouteToConnection` events (handler
    /// position 3 vs 6). The interpreter drains events sequentially
    /// and awaits the actor ask, so persistence completes before the
    /// live broadcast leaves the server. Net result: every observer
    /// of the live broadcast on this connection's outbound stream is
    /// guaranteed to see the new subject reflected in any subsequent
    /// `JoinOutcome.subject_state` snapshot. The added latency is one
    /// `RoomActor` mailbox round-trip per subject change — subject
    /// changes are rare, so the simpler ordering is preferred over
    /// fire-and-forget concurrency.
    ///
    /// **Failure mode.** If the actor ask fails (mailbox closed, room
    /// destroyed mid-dispatch) the interpreter logs and continues
    /// draining — the live broadcast still goes out, just without a
    /// matching state update. Future joiners see the previous stored
    /// subject; the broadcast they missed is gone. This is an
    /// irreducible window for any out-of-band persistence path; the
    /// failure rate is bounded by `RoomActor` mailbox availability,
    /// which is shared with every other room operation.
    PersistRoomSubject {
        /// Room whose state is being mutated.
        room: BareJid,
        /// New subject texts keyed by `xml:lang` (`""` is the default
        /// language). Mirrors the originating §8.1 message's
        /// `<subject xml:lang='...'>` set so localized variants
        /// survive into the join-time replay. An entry with an empty
        /// value represents an explicit clear (still stored as
        /// `Some(SubjectState)` so the next join emits `<delay/>`
        /// per §7.2.15's SHOULD-include-delay-on-cleared).
        texts: crate::muc::RoomSubjectTexts,
        /// Setter's bare JID — input to the XEP-0421 occupant-id HMAC
        /// at next-join emission.
        setter: BareJid,
        /// Setter's nickname at the moment of the change. Frozen here
        /// rather than re-resolved at emission so historical join-time
        /// emissions stay stable across nick changes and after the
        /// setter has left the room.
        setter_nick: String,
        /// Wall-clock time of the change (UTC). Becomes the XEP-0203
        /// `<delay/>` `stamp` attribute on the next join's emission.
        set_at: DateTime<Utc>,
    },
    /// XEP-0424 §"prevent further distribution" — replace the target
    /// row in a room's MAM archive with a tombstone after a groupchat
    /// retraction passes authorization.
    ///
    /// Emitted by the room handler chain's archive handler when the
    /// in-flight message is a retraction request. The interpreter
    /// performs the actual `MamStorage::replace_with_tombstone` call.
    /// Mirrors the typed `ArchivedTombstone` semantic the 1:1 path
    /// invokes via [`OutboundEvent::ArchiveDirect`]'s retraction
    /// branch, but keyed by room JID instead of personal archive.
    ApplyGroupchatRetractionTombstone {
        /// Room JID whose archive holds the target row (the only
        /// archive-key used for groupchat persistence).
        room: BareJid,
        /// Wire id of the message being retracted — XEP-0424
        /// `<retract id='...'/>`.
        target_message_id: String,
        /// The retraction message itself, used to derive the tombstone's
        /// `retraction_id` (XEP-0424 §"tombstones cite the retraction").
        retraction_message: Box<Message>,
    },
    /// Project a groupchat message into one user's inbox (Waddle product
    /// surface). Sibling to [`OutboundEvent::ProjectInbox`] for the
    /// MUC-locality chain — emitted once per occupant by the room
    /// handler chain's inbox handler.
    ///
    /// `is_recipient` is `true` for everyone except the sender, who
    /// gets their own copy without bumping the unread counter.
    ///
    /// `thread` carries the message's `<thread/>` payload when present
    /// so the interpreter can write the thread-scoped inbox row
    /// alongside the channel-level one. `None` when the message is not
    /// thread-scoped — the channel row is still written.
    ProjectGroupchatInbox {
        /// Bare JID whose inbox is being updated.
        owner: BareJid,
        /// Room JID this projection belongs to.
        room: BareJid,
        /// The canonicalized groupchat message (post-chain mutations).
        message: Box<Message>,
        /// `true` for recipients (bumps unread); `false` for the sender.
        is_recipient: bool,
        /// Optional thread metadata for the thread-level row.
        thread: Option<GroupchatThreadProjection>,
        /// Single dispatch timestamp (Unix epoch seconds) shared
        /// across every per-occupant projection of this groupchat
        /// message. The chain captures `Utc::now().timestamp()` once
        /// at dispatch start and copies it into each per-occupant
        /// event so projections don't drift across a second-boundary
        /// (Copilot review on PR #279).
        dispatch_timestamp: i64,
    },
    /// XEP-0280 carbon-copy fan-out to the owner's other resources.
    ///
    /// Carbon-suppression rules (XEP-0280 §6.1 `<private/>`, §6.2
    /// `type='groupchat'`, XEP-0334 `<no-copy/>`) are enforced by the
    /// emitting handler so this event is only produced for messages that
    /// genuinely should be carboned. The interpreter wraps the message in
    /// `<sent>`/`<received>` → `<forwarded xmlns='urn:xmpp:forward:0'>`
    /// (XEP-0297) and delivers a copy to every resource of `owner`
    /// except `exclude`.
    SendCarbons {
        owner: BareJid,
        message: Box<Message>,
        kind: CarbonKind,
        exclude: FullJid,
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
    /// Look up an archived message by [`MessageRef`] for rich-target
    /// validation (XEP-0308 correction, XEP-0424 retraction,
    /// XEP-0425 moderation, XEP-0461 reply). Result arrives as
    /// [`InboundEvent::ArchivedMessageLoaded`].
    ///
    /// `archive` is the bare JID whose MAM is queried (the user's bare
    /// JID for 1:1 messages, the room JID for groupchat).
    LookupArchivedMessage {
        id: CallbackId,
        archive: BareJid,
        reference: MessageRef,
    },

    // -------------------------------------------------------------------
    // Timers
    // -------------------------------------------------------------------
    /// Ask the interpreter to wake the state machine with
    /// `InboundEvent::Tick` after `duration`.
    SetTimer { id: TimerId, duration_ms: u64 },
    /// Cancel a previously-set timer.
    CancelTimer(TimerId),

    /// XEP-0160 offline-message store: the recipient has no resource
    /// with non-negative presence priority online at intake time, and
    /// the [`crate::protocol::dm_routing::classify_dm_intake`] classifier
    /// has approved persistence (issue #209, locked Q1 = C / Q4 = A).
    ///
    /// The interpreter writes the row into
    /// [`crate::pending_delivery::storage::PendingDeliveryStorage`] and
    /// returns `<service-unavailable/>` to the sender on
    /// [`crate::pending_delivery::InsertOutcome::QuotaExceeded`] per
    /// XEP-0160 §3 step 3 (locked Q9b).
    ///
    /// `original_receipt_at` is the server-side intake timestamp; it is
    /// the value the server will eventually stamp onto `<delay/>` per
    /// XEP-0203 §4.1 + XEP-0198 §5 line 364 ("original (failed) delivery
    /// timestamp").
    QueueOfflineDelivery {
        recipient: BareJid,
        payload: crate::pending_delivery::PendingPayload,
        original_receipt_at: chrono::DateTime<chrono::Utc>,
    },

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
