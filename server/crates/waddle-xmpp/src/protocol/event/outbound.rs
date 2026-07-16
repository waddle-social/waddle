use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use tracing::Level;
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;

use crate::mam::MamArchiveKind;
use crate::muc::PinChangeRequest;
use crate::Stanza;

use super::{CallbackId, CarbonKind, GroupchatThreadProjection, MessageRef, TimerId};

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
    /// Send a transport-native liveness probe (RFC 7395 §3.8 / issue
    /// #1090). The probe is a transport control frame with no stanza
    /// semantics, so the *adapter* — not the interpreter's XML path —
    /// maps it to its native mechanism: a WebSocket `Ping` frame with
    /// an empty payload today; a future TCP transport would map it to
    /// whitespace keepalive or XEP-0199. Emitted by
    /// [`crate::protocol::keepalive::KeepalivePolicy`] on an
    /// inbound-idle tick.
    SendKeepaliveProbe,
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
    /// **Semantics (issue #229, landed)**: the interpreter resolves
    /// `jid` against `ConnectionRegistry` and delivers the stanza to
    /// the destination connection as a peer stanza. The destination's
    /// recipient-pass pipeline runs — XEP-0191 incoming block,
    /// XEP-0359 recipient stamp, XEP-0313 recipient-side archive,
    /// XEP-0280 received-carbons, inbox projection — and ultimately
    /// emits [`OutboundEvent::SendStanza`] to the destination's wire.
    /// When a local bare-JID target has no available resources, the
    /// interpreter runs a headless recipient pass so persistence and
    /// incoming-block effects still execute.
    ///
    /// Cross-domain bare-JID targets with no local resources are
    /// logged and dropped (s2s is out of scope); wire delivery to an
    /// offline local recipient is archive-based, via the headless
    /// pass's persistence effects, not routing-based.
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
    ///
    /// `sender_item` is the sender's typed authority snapshot
    /// (real JID + affiliation + role) captured at dispatch time.
    /// XEP-0313 §MUC Archives requires non-anonymous rooms to
    /// disclose the sender's real JID in archived messages via a
    /// room-authored `<x xmlns='muc#user'><item jid=…/></x>`; the
    /// interpreter bakes it into the archived copy only — the live
    /// reflection never carries it (#1268). `None` for synthetic
    /// dispatches with no occupant snapshot.
    ArchiveGroupchat {
        room: BareJid,
        sender: FullJid,
        message: Box<Message>,
        sender_nickname_generation: u64,
        sender_item: Option<waddle_xmpp_core::mam::ArchivedMucSender>,
    },
    /// Persist a one-to-one direct message to the MAM archive.
    ///
    /// `archive_jid` identifies which personal archive to write to —
    /// the locality-aware [`super::handlers::archive::ArchiveHandler`]
    /// emits this field as the local user's bare JID, so the interpreter
    /// is dumb glue that does not need to reason about sender/recipient
    /// pass semantics. `from` and `to` carry the canonical message tuple
    /// for telemetry and MAM `with` matching. A MUC-PM peer remains a full
    /// occupant JID while the user's side remains its account JID; ordinary
    /// direct-message producers keep supplying bare JIDs for both sides.
    ArchiveDirect {
        archive_jid: BareJid,
        from: jid::Jid,
        to: jid::Jid,
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
        /// Canonical [`StanzaId`] linking the inbox row to its archived
        /// counterpart so clients can pivot to the MAM entry using the
        /// same XEP-0359 stanza-id space.
        archive_ref: StanzaId,
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
    /// **Failure mode.** If exact room authority is unavailable, lost, or the
    /// actor transport cannot confirm handling, the interpreter uses the
    /// captured sender/message to emit a retryable error and suppresses every
    /// later archive, inbox, and fan-out event in the same dispatch batch.
    PersistRoomSubject {
        /// Room whose state is being mutated.
        room: BareJid,
        /// Exact room-actor incarnation fence bound by the interpreter
        /// after it obtains the actor snapshot. `None` is valid only for
        /// an actor without durable ownership; a durable actor rejects a
        /// missing or different fence before applying the subject change.
        claim_fence: Option<crate::muc::RoomClaimFenceContext>,
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
        /// Exact sending session used for a retryable failure reply.
        sender: FullJid,
        /// Canonical subject-change message preserved for error correlation.
        message: Box<Message>,
        /// Setter's nickname at the moment of the change. Frozen here
        /// rather than re-resolved at emission so historical join-time
        /// emissions stay stable across nick changes and after the
        /// setter has left the room.
        setter_nick: String,
        /// Wall-clock time of the change (UTC). Becomes the XEP-0203
        /// `<delay/>` `stamp` attribute on the next join's emission.
        set_at: DateTime<Utc>,
    },
    /// Apply a pin/unpin request to a MUC room (#414).
    ///
    /// Emitted by [`super::room::pin::MucPinHandler`] when an
    /// authorized occupant publishes a `urn:waddle:pin:0`
    /// `<pinned/>` or `<unpinned/>` element on a groupchat message.
    /// The interpreter resolves the target message from MAM (for
    /// pins, to populate the preview), forwards the resolved change
    /// to the room actor's `ApplyPin` message, and emits the
    /// `<pin-event/>` system message broadcast itself.
    ///
    /// The chain handler is synchronous and cannot do the MAM
    /// lookup, so it carries only the request fields here — the
    /// interpreter is the async boundary that builds the resolved
    /// `PinnedEntry`.
    ApplyPinChange {
        /// Room whose pin state is being mutated.
        room: BareJid,
        /// The request to apply.
        request: PinChangeRequest,
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
    /// MUC-locality chain — emitted once for the sender's own row plus
    /// once per durable affiliation-derived recipient.
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
        /// `true` when the owner came from the durable affiliation-derived
        /// recipient set. XEP-0357 groupchat notification enqueue requires
        /// this; live occupancy alone must never expand push recipients.
        is_durable_recipient: bool,
        /// `true` when this recipient is currently joined to the room.
        /// Used by notification policy for XEP-0513 active channel
        /// mentions; it must not become the authoritative recipient set.
        is_live_occupant: bool,
        /// `true` when the room is members-only. Used by the server-side
        /// XEP-0492 projection gate to choose the group-chat default
        /// (`private_group` vs `public_group`) without re-querying room
        /// config after the dispatch snapshot.
        room_members_only: bool,
        /// XEP-0513 §"Multi-User Chats Permissions": typed permission
        /// snapshot of whether the sender's frozen XEP-0045 role permits
        /// broadcasting an `urn:xmpp:mentions:0#channel` mention for push
        /// purposes. Frozen at room-dispatch time from the sender's
        /// `OccupantSnapshot.role` so the T0 candidate classifier can
        /// downgrade a non-permitted channel mention to `NotifyAll`
        /// without re-querying room/occupant state. The mention is still
        /// delivered + archived unchanged; only the push class changes.
        ///
        /// Default server policy is `moderators` (XEP-0513 example value)
        /// — `role >= Role::Moderator`. The per-room override IQ surface
        /// is a follow-up slice.
        sender_can_broadcast_channel_mention: bool,
        /// Optional thread metadata for the thread-level row.
        thread: Option<GroupchatThreadProjection>,
        /// Single dispatch timestamp (Unix epoch seconds) shared
        /// across every inbox projection of this groupchat
        /// message. The chain captures `Utc::now().timestamp()` once
        /// at dispatch start and copies it into each projection
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
    /// except those in `exclude`.
    ///
    /// `exclude` is the full delivery set of the original stanza —
    /// XEP-0280 §6.3: "The receiving server MUST NOT send a forwarded
    /// copy to the client(s) the original <message/> stanza was
    /// addressed to, as these recipients receive the original
    /// <message/> stanza." For the sender pass and full-JID
    /// deliveries this is the single originating/receiving resource;
    /// for the shared bare-JID recipient pass (#1106) it is the whole
    /// RFC 6121 §8.5.2.1.1 same-priority delivery set.
    SendCarbons {
        owner: BareJid,
        message: Box<Message>,
        kind: CarbonKind,
        exclude: Vec<FullJid>,
    },
    /// XEP-0333 §3 — the sender displayed a message up to
    /// `displayed_message_id` in a MUC room. The interpreter resolves
    /// the message's thread via MAM and clears the matching inbox row(s)
    /// — both the channel-level row and the thread-level row when the
    /// displayed message belongs to a thread.
    ///
    /// Emitted by the room handler chain's displayed-marker handler at
    /// most once per dispatch — for the **sender only**, since a
    /// displayed marker reports the sender's own read state. Reflected
    /// markers received as recipients do NOT trigger a mark-read here:
    /// that would cross-clear other users' inboxes.
    ///
    /// `room` is the MUC bare JID (where the displayed message was
    /// archived); `owner` is the sender's bare JID (whose inbox is
    /// being mark-read); `displayed_message_id` is the wire id
    /// referenced by the `<displayed id='…'/>` element. The interpreter
    /// looks it up in `MamStorage` keyed by `room` to derive the
    /// thread.
    MarkInboxReadFromDisplayed {
        /// Sender's bare JID — whose inbox is being mark-read.
        owner: BareJid,
        /// Room bare JID — the MAM archive holding the displayed message.
        room: BareJid,
        /// Wire id from `<displayed id='…' xmlns='urn:xmpp:chat-markers:0'/>`.
        displayed_message_id: String,
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
    /// `archive` is the bare JID whose MAM is queried. `archive_kind`
    /// carries the protocol context that a bare JID alone cannot encode.
    LookupArchivedMessage {
        id: CallbackId,
        archive: BareJid,
        archive_kind: MamArchiveKind,
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
        /// The original inbound `<message>`, preserved verbatim so the
        /// `<service-unavailable/>` bounce path on
        /// `InsertOutcome::QuotaExceeded` can construct a typed reply
        /// via `protocol::handlers::errors::message_error_reply` —
        /// which swaps `from`/`to` and attaches a typed
        /// `xmpp_parsers::stanza_error::StanzaError`. RFC 6120 §8.3
        /// has the canonical wire shape; XEP-0160 §3 step 3 mandates
        /// it on quota overflow.
        original_message: Box<Message>,
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
