//! Effect interpreter for [`waddle_xmpp::protocol::OutboundEvent`].
//!
//! The state machine in `waddle-xmpp::protocol` is pure and synchronous —
//! it emits typed outbound events that *describe* side effects but does
//! not perform them. This module is the async counterpart: it
//! pattern-matches each event and runs the real operation against the
//! transport, the connection registry, MUC rooms, MAM storage, the SFU
//! actor, etc.
//!
//! # Typed payloads at the I/O boundary
//!
//! Per the project's typed-payloads hard rule, stanzas travel through
//! the state machine as typed values (`Stanza`, `Iq`, `Message`).
//! Serialization to the XML wire format happens here, exactly once,
//! when we hand bytes off to the transport.
//!
//! # Current coverage
//!
//! Wired:
//! - [`OutboundEvent::SendStanza`] — serialize and emit on the local wire.
//! - [`OutboundEvent::CloseTransport`] — signal the main loop to close.
//! - [`OutboundEvent::Log`] — route through `tracing`.
//! - [`OutboundEvent::RouteToConnection`] — full-JID + bare-JID
//!   (RFC 6121 §8.5 resource selection) routing as
//!   `DeliveryKind::PeerStanza` so the recipient's main loop runs the
//!   recipient pass before any wire write. Bare-JID targets at a
//!   local user with no available resources fall through to a
//!   transient headless recipient pass (#229 PR15) so archive +
//!   inbox + XEP-0191 incoming-block still persist. Cross-domain
//!   bare JIDs (future s2s) drop without a recipient pass.
//! - [`OutboundEvent::SendCarbons`] — XEP-0280 carbon fan-out via the
//!   XEP-0297 `<sent>`/`<received>` envelope, including detached
//!   XEP-0198 resumable sessions.
//! - [`OutboundEvent::ArchiveDirect`] — XEP-0313 §5.1 personal MAM
//!   write keyed under `archive_jid`. Eligibility was vetted by
//!   [`waddle_xmpp::protocol::handlers::archive::ArchiveHandler`].
//! - [`OutboundEvent::ProjectInbox`] — Waddle inbox upsert keyed by
//!   `(owner, peer)` with `archive_ref` linking back to the MAM entry.
//! - [`OutboundEvent::UnregisterConnection`] — drop from the registry.
//! - [`OutboundEvent::LookupArchivedMessage`] — XEP-0359 stanza-id /
//!   origin-id lookup against personal MAM; result feeds back as
//!   [`InboundEvent::ArchivedMessageLoaded`] in
//!   [`InterpretOutcome::feedback`].
//! - [`OutboundEvent::RequestEnrichment`] — XEP-0372 link enrichment
//!   via `ExtensionManager`; result feeds back as
//!   [`InboundEvent::EnrichmentComplete`] in
//!   [`InterpretOutcome::feedback`]. Fail-open semantics match legacy.
//!
//! - [`OutboundEvent::DispatchToRoom`] — MUC sender-pass: hoists
//!   managed-room owner check + rich-target validation, queries the
//!   per-room actor for a frozen
//!   [`waddle_xmpp::muc::room_actor::RoomChainSnapshot`], runs the
//!   stateless room handler chain, and recursively interprets emitted
//!   events.
//! - [`OutboundEvent::ArchiveGroupchat`] — XEP-0313 §5.1.3 room MAM
//!   write keyed by room JID.
//! - [`OutboundEvent::ApplyGroupchatRetractionTombstone`] — XEP-0424
//!   §"prevent further distribution" tombstone replace against the
//!   room archive (mirrors the 1:1 retraction tombstone arm).
//! - [`OutboundEvent::ProjectGroupchatInbox`] — per-occupant inbox
//!   upsert (channel + thread rows) plus the XEP-0430 inbox push to
//!   the owner's other resources.
//!
//! Stubbed (warn-logged until migration steps land them):
//! - `AskSfu`, `QueryMam`, `LoadScramCredentials`,
//!   `ValidateOAuthBearer`, `SetTimer`, `CancelTimer`,
//!   `RegisterConnection` — wired in later migration steps.

use crate::auth::Session;
use crate::permissions::{CheckPermission, Object, ObjectType, Permission, Subject};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use jid::{BareJid, FullJid, Jid};
use kameo::actor::ActorRef;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use waddle_extensions::{
    message_has_framework_envelope, DisplayText, ExtensionEffect, ExtensionEnvelope,
    ExtensionManager, ReplyTarget, RoomJid, StanzaId, ThreadId, WaddleId,
};
use waddle_xmpp::carbons::{build_received_carbon, build_sent_carbon};
use waddle_xmpp::inbox::runtime::{direct_message_entry, groupchat_entry, groupchat_thread_entry};
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::inbox::InboxEntry;
use waddle_xmpp::mam::projection::build_direct_archived_message;
use waddle_xmpp::mam::storage::MamStorage;
use waddle_xmpp::mam::{
    ArchivedMention, ArchivedMessage as MamArchivedMessage, ArchivedReactionSet, ArchivedReference,
    ArchivedReply, ArchivedRetraction, ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone,
    RichMessageId, RichText, STANZA_ID_NS,
};
use waddle_xmpp::muc::room_actor::{
    GetAffiliation, GetNicknameGeneration, GetRoomSnapshot, JoinWithAffiliation, RoomActor,
    SetSubject,
};
use waddle_xmpp::muc::room_registry_actor::{GetRoom, RoomRegistryActor};
use waddle_xmpp::parse_managed_room_jid;
use waddle_xmpp::parser::{message_to_string, stanza_to_string};
use waddle_xmpp::protocol::event::{
    ArchivedMessage as ProtocolArchivedMessage, GroupchatThreadProjection, InboundEvent,
    MessageRef, StanzaIdRef, StanzaIdValue,
};
use waddle_xmpp::protocol::id_gen::UuidV4Generator;
use waddle_xmpp::protocol::room::{
    default_room_pipeline_dispatcher, OccupantSnapshot, RoomContext,
};
use waddle_xmpp::protocol::{
    Blocklist, CarbonKind, OutboundEvent, StanzaDispatcher, XmppStateMachine,
};
use waddle_xmpp::registry::ConnectionRegistry;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
use waddle_xmpp::xep::xep0191::BlockingStorage;
use waddle_xmpp::xep::xep0430::build_inbox_push;
use waddle_xmpp::xep::{
    extract_correction_from_message, extract_explicit_mentions, extract_forum_action,
    extract_reactions_from_message, extract_references_from_message,
    extract_retraction_from_message, parse_reply_from_message, remove_stanza_ids_by,
    set_reply_payload, ForumAction, ReplyReference, RetractionKind, NS_EXPLICIT_MENTIONS,
    NS_MESSAGE_CORRECT, NS_MESSAGE_RETRACT, NS_REACTIONS, NS_REFERENCE, NS_REPLY,
};
use waddle_xmpp::xep0201::set_thread_id;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Message, MessageType as XmppMessageType};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::server::routes::websocket::WebSocketState;

/// Outcome of interpreting a batch of [`OutboundEvent`]s.
///
/// The WebSocket transport uses `frames` to decide what to write back to
/// the client. `close` signals the main loop should drop the connection.
/// `feedback` carries [`InboundEvent`] callback completions the state
/// machine must consume to resume parked dispatches (XEP-0359
/// rich-target lookup, link enrichment). The caller is responsible for
/// pumping `feedback` back through `state_machine.handle(...)` until
/// the resulting outcome's `feedback` drains — see #229 PR9 cutover.
#[derive(Debug, Default)]
pub struct InterpretOutcome {
    /// Serialized XML frames to write to the transport, in order.
    pub frames: Vec<String>,
    /// Set to true when the state machine asked us to close the transport.
    pub close: bool,
    /// Async-callback completions to feed back to the state machine.
    pub feedback: Vec<InboundEvent>,
}

/// Typed dependency context for the interpreter.
///
/// Grows as later migration steps add storage/actor handles
/// (`extension_manager`, etc.). Threading dependencies through one
/// struct rather than as loose function parameters keeps the call-site
/// churn small.
#[derive(Clone)]
pub struct Deps<'a> {
    pub connection_registry: &'a ConnectionRegistry,
    /// XEP-0198 stream-management session registry. Used to fan
    /// XEP-0280 carbons out to *detached but resumable* resources so
    /// briefly-disconnected secondary devices don't lose carbon
    /// history while resumable. `None` in unit tests that don't
    /// exercise SM behaviour.
    pub sm_session_registry: Option<&'a Arc<InMemorySmSessionRegistry>>,
    /// XEP-0313 MAM persistence backend. `None` in unit tests that
    /// don't exercise archive writes; production wiring (`iq.rs`)
    /// always supplies it.
    pub mam_storage: Option<&'a Arc<dyn MamStorage>>,
    /// Waddle inbox-projection backend. `None` in unit tests; always
    /// supplied in production.
    pub inbox_storage: Option<&'a Arc<dyn InboxStorage>>,
    /// Wasm-extension manager for XEP-0372/etc. link enrichment.
    /// `None` in unit tests that don't exercise the
    /// [`OutboundEvent::RequestEnrichment`] arm.
    pub extension_manager: Option<&'a Arc<ExtensionManager>>,
    /// MUC room-registry actor. Used by the
    /// [`OutboundEvent::DispatchToRoom`] arm to look up the per-room
    /// actor and ask it for a frozen
    /// [`waddle_xmpp::muc::room_actor::RoomChainSnapshot`].
    pub room_registry: Option<&'a ActorRef<RoomRegistryActor>>,
    /// WebSocket route state. Used by the
    /// [`OutboundEvent::DispatchToRoom`] arm for the managed-room
    /// owner check (announcements room) and by
    /// [`OutboundEvent::ProjectGroupchatInbox`] for inbox upserts +
    /// XEP-0430 push fan-out. `None` in unit tests; always supplied
    /// in production via
    /// [`super::super::websocket::build_interpret_deps`].
    pub web_socket_state: Option<&'a WebSocketState>,
    /// The authenticated `Session` of the connection that emitted the
    /// outbound events being interpreted, when one is available.
    ///
    /// Threaded through so the
    /// [`OutboundEvent::DispatchToRoom`] arm can perform the
    /// managed-room owner check (announcements room admits server
    /// owners only) without re-querying. `None` for unauthenticated
    /// flows (early connection lifecycle, unit tests).
    pub authenticated_session: Option<&'a Session>,
    /// Authoritative local XMPP domain. Used by the
    /// [`OutboundEvent::RouteToConnection`] arm to gate the
    /// headless offline-recipient pass (#229 PR15) to local-domain
    /// bare JIDs only — cross-domain bare JIDs (future s2s) drop
    /// without the recipient pass.
    pub local_domain: &'a str,
    /// XEP-0191 blocklist persistence backend. Used by the headless
    /// recipient-pass runner to seed a transient
    /// [`XmppStateMachine`]'s [`Blocklist`] for an offline local
    /// recipient. `None` in unit tests that don't exercise the
    /// offline-pass; production wiring always supplies it.
    pub blocking_storage: Option<&'a Arc<dyn BlockingStorage>>,
    /// Per-process [`StanzaDispatcher`] handle, cloned cheaply into
    /// each transient [`XmppStateMachine`] the headless
    /// recipient-pass runner constructs. `None` in unit tests that
    /// don't exercise the offline-pass.
    pub message_dispatcher: Option<&'a Arc<StanzaDispatcher>>,
}

impl<'a> Deps<'a> {
    /// Build a minimal `Deps` with only the connection registry — a
    /// test-only convenience for unit tests that don't exercise SM
    /// fan-out, archive, or inbox storage. Defaults `local_domain` to
    /// `"example.com"` to match the test fixtures used throughout
    /// this module's tests.
    #[cfg(test)]
    pub fn registry_only(connection_registry: &'a ConnectionRegistry) -> Self {
        Self {
            connection_registry,
            sm_session_registry: None,
            mam_storage: None,
            inbox_storage: None,
            extension_manager: None,
            room_registry: None,
            web_socket_state: None,
            authenticated_session: None,
            local_domain: "example.com",
            blocking_storage: None,
            message_dispatcher: None,
        }
    }

    /// Build a `Deps` for unit tests that exercise the storage arms
    /// (`ArchiveDirect`, `ProjectInbox`). SM fan-out is left disabled
    /// so the carbon-detached path stays an independent test concern.
    #[cfg(test)]
    pub fn test_with_storage(
        connection_registry: &'a ConnectionRegistry,
        mam_storage: &'a Arc<dyn MamStorage>,
        inbox_storage: &'a Arc<dyn InboxStorage>,
    ) -> Self {
        Self {
            connection_registry,
            sm_session_registry: None,
            mam_storage: Some(mam_storage),
            inbox_storage: Some(inbox_storage),
            extension_manager: None,
            room_registry: None,
            web_socket_state: None,
            authenticated_session: None,
            local_domain: "example.com",
            blocking_storage: None,
            message_dispatcher: None,
        }
    }

    /// Build a `Deps` for unit tests that exercise the
    /// [`OutboundEvent::RequestEnrichment`] arm.
    #[cfg(test)]
    pub fn test_with_extension_manager(
        connection_registry: &'a ConnectionRegistry,
        extension_manager: &'a Arc<ExtensionManager>,
    ) -> Self {
        Self {
            connection_registry,
            sm_session_registry: None,
            mam_storage: None,
            inbox_storage: None,
            extension_manager: Some(extension_manager),
            room_registry: None,
            web_socket_state: None,
            authenticated_session: None,
            local_domain: "example.com",
            blocking_storage: None,
            message_dispatcher: None,
        }
    }
}

/// Execute the side effects described by `events`.
///
/// The function is `async` because future migration steps add variants
/// that genuinely require `.await` (registry lookups, actor calls, MAM
/// storage). The currently-supported variants are all synchronous, so this
/// function will return immediately for the ping/session flow.
pub async fn interpret(events: Vec<OutboundEvent>, deps: &Deps<'_>) -> InterpretOutcome {
    interpret_with_depth(events, deps, 0).await
}

/// Hard cap on the [`run_headless_recipient_pass`] recursion. The
/// outer dispatch (depth = 0) runs the sender pass; an offline-bare-JID
/// `RouteToConnection` re-enters at depth = 1 to run the recipient
/// pass; any further `RouteToConnection` from inside that pass drops
/// to prevent runaway recursion. See PR15 design notes.
const MAX_RECIPIENT_PASS_DEPTH: u8 = 1;

/// Internal entry point that threads the recursion depth. The public
/// [`interpret`] starts at depth 0; the offline-recipient pass
/// re-enters at depth 1 via [`run_headless_recipient_pass`].
async fn interpret_with_depth(
    events: Vec<OutboundEvent>,
    deps: &Deps<'_>,
    recursion_depth: u8,
) -> InterpretOutcome {
    let registry = deps.connection_registry;
    let mut outcome = InterpretOutcome::default();

    for event in events {
        match event {
            OutboundEvent::SendStanza(stanza) => match stanza.to_element_string() {
                Ok(xml) => outcome.frames.push(xml),
                Err(err) => {
                    error!(error = %err, "failed to serialize outbound stanza; dropping frame");
                }
            },
            OutboundEvent::CloseTransport => {
                outcome.close = true;
            }
            OutboundEvent::Log { level, message } => {
                // Route the log back through tracing so it ends up in the
                // application's log pipeline. We format the state-machine
                // message into the event text (via `%message`) rather than
                // as a structured field so it renders the same as the rest
                // of the codebase's logs.
                match level {
                    tracing::Level::ERROR => error!(%message, "protocol"),
                    tracing::Level::WARN => warn!(%message, "protocol"),
                    tracing::Level::INFO => info!(%message, "protocol"),
                    tracing::Level::DEBUG | tracing::Level::TRACE => {
                        debug!(%message, "protocol")
                    }
                }
            }

            // -------------------------------------------------------
            // Variants defined for future migration steps. We log only the
            // variant discriminant (and, where cheap, typed identifiers
            // like JIDs or stanza ids) — never the typed payload. Some of
            // these variants carry `Message` / `Iq` structs containing
            // user content, and their `Debug` impls would leak that
            // content into logs.
            // -------------------------------------------------------
            OutboundEvent::RouteToConnection { jid, stanza } => {
                // #229 PR12 cutover: the destination's main loop is
                // now wired (PR11) to dispatch on `DeliveryKind` and
                // run the recipient-pass pipeline for `PeerStanza`
                // values, so we deliver as `PeerStanza`. The
                // recipient's `XmppStateMachine::on_peer_stanza`
                // takes it from there: XEP-0191 incoming block,
                // XEP-0359 recipient stamp, XEP-0313 recipient-side
                // archive, XEP-0280 received-carbons, inbox
                // projection, then `SendStanza` to the wire.
                //
                // `jid` is a typed `Jid` — full or bare. Full-JID
                // targets deliver to that single resource. Bare-JID
                // targets go through RFC 6121 §8.5.2.1 resource
                // selection (highest-priority available resources;
                // tie-broken by delivering to all of them).
                //
                // Offline-recipient persistence (#229 PR15): when the
                // bare-JID target has no available resources but the
                // domain is local, run a headless recipient pass so
                // archive + inbox + incoming-blocking still execute
                // — see [`run_headless_recipient_pass`]. Cross-domain
                // bare JIDs (future s2s) drop without a recipient pass.
                //
                // Recursion guard (Codex P1 on PR #275): the depth check
                // gates the *entire* arm, not just the empty-targets
                // branch. At `recursion_depth >= MAX_RECIPIENT_PASS_DEPTH`
                // we are already inside a headless pass; any nested
                // `RouteToConnection` — full-JID or bare-JID, with or
                // without live targets — must drop, otherwise live
                // delivery would re-trigger a second recipient pass and
                // duplicate persistence. Persistence and incoming-block
                // for the offline recipient are owned by the OUTER
                // headless pass; nothing else.
                if recursion_depth >= MAX_RECIPIENT_PASS_DEPTH {
                    debug!(
                        target_jid = %jid,
                        recursion_depth,
                        "RouteToConnection: headless recipient-pass already running; \
                         dropping nested route (full or bare) to prevent duplicate \
                         delivery / persistence"
                    );
                } else {
                    match jid.clone().try_into_full() {
                        Ok(full) => {
                            deliver_peer_to_full(registry, deps.sm_session_registry, &full, &stanza)
                                .await
                        }
                        Err(bare) => {
                            // Enumerate XEP-0198 detached-but-resumable
                            // resources for the bare JID. The legacy
                            // `handle_message` direct-route path queued
                            // bare-JID DMs onto detached resources via
                            // `record_stanza_for_detached_bound_resource`
                            // so a recipient mid-resume didn't lose
                            // messages; we preserve that here.
                            let detached_targets: Vec<jid::FullJid> = match deps.sm_session_registry
                            {
                                Some(sm) => sm
                                    .detached_resources_for_user(&bare)
                                    .await
                                    .unwrap_or_else(|error| {
                                        warn!(
                                            bare_jid = %bare,
                                            %error,
                                            "RouteToConnection: failed to enumerate \
                                             detached resources for bare-JID delivery"
                                        );
                                        Vec::new()
                                    }),
                                None => Vec::new(),
                            };
                            // RFC 6121 §8.5.2.1.1 prefers presence-available
                            // resources for bare-JID delivery; fall back to
                            // any connected resource when none have emitted
                            // `<presence/>` yet. Many clients defer presence
                            // until after resource binding completes, and
                            // the legacy `handle_message` direct-route path
                            // delivered without consulting presence. This
                            // preserves that behaviour without giving up
                            // RFC priority routing for clients that do use
                            // presence.
                            let live_targets = {
                                let priority = registry.select_routable_resources_for_user(&bare);
                                if priority.is_empty() {
                                    registry.get_resources_for_user(&bare)
                                } else {
                                    priority
                                }
                            };
                            if live_targets.is_empty() && detached_targets.is_empty() {
                                if bare.domain().as_str() != deps.local_domain {
                                    debug!(
                                        bare_jid = %bare,
                                        local_domain = %deps.local_domain,
                                        "RouteToConnection: cross-domain bare JID with no \
                                         local resources; dropping (s2s out of scope)"
                                    );
                                } else {
                                    run_headless_recipient_pass(
                                        deps,
                                        &bare,
                                        *stanza,
                                        recursion_depth + 1,
                                    )
                                    .await;
                                }
                            } else {
                                // Build a set from the cached `live_targets`
                                // before iterating so we can both consume
                                // the targets for delivery and re-check
                                // membership when filtering the detached
                                // list — avoids re-querying the registry
                                // per detached resource (Copilot review on
                                // PR #276).
                                let live_set: std::collections::HashSet<jid::FullJid> =
                                    live_targets.iter().cloned().collect();
                                for full in live_targets {
                                    deliver_peer_to_full(
                                        registry,
                                        deps.sm_session_registry,
                                        &full,
                                        &stanza,
                                    )
                                    .await;
                                }
                                if let Some(sm) = deps.sm_session_registry {
                                    for full in detached_targets {
                                        // Skip if this resource was just
                                        // delivered live (race between
                                        // enumeration and live-resource
                                        // selection).
                                        if live_set.contains(&full) {
                                            continue;
                                        }
                                        // Known limitation: queues the
                                        // pre-recipient-pass stanza into
                                        // the detached XEP-0198 replay
                                        // buffer. When the resource
                                        // resumes, replay sends the
                                        // stored XML verbatim WITHOUT
                                        // running the recipient-pass
                                        // chain, so the replayed message
                                        // is missing the recipient-side
                                        // `<stanza-id by='recipient/>`
                                        // (XEP-0359 §5) and recipient-
                                        // side filtering / archive /
                                        // inbox effects don't fire.
                                        // This matches LEGACY behaviour
                                        // (which had no recipient pass
                                        // at all) and is therefore not a
                                        // regression. Closing the gap
                                        // properly requires running the
                                        // headless recipient pass per
                                        // detached target and queueing
                                        // its `SendStanza` output —
                                        // tracked as a follow-up to
                                        // #229 (Copilot review on
                                        // PR #276).
                                        let stanza_typed = (*stanza).clone();
                                        match sm
                                            .record_stanza_for_detached_bound_resource(
                                                &full,
                                                &stanza_typed,
                                            )
                                            .await
                                        {
                                            Ok(true) => {
                                                debug!(
                                                    jid = %full,
                                                    "RouteToConnection: bare-JID stanza queued \
                                                     for detached XEP-0198 replay"
                                                );
                                            }
                                            Ok(false) => {
                                                debug!(
                                                    jid = %full,
                                                    "RouteToConnection: detached session expired \
                                                     between enumeration and queue; dropping"
                                                );
                                            }
                                            Err(error) => {
                                                warn!(
                                                    jid = %full,
                                                    %error,
                                                    "RouteToConnection: failed to record bare-JID \
                                                     stanza for detached resource"
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            OutboundEvent::DispatchToRoom { room, message } => {
                // #229 PR18 — MUC cutover. Replaces the legacy
                // `deliver_groupchat_via_room_actor` bridge with the
                // stateless room handler chain (Q7 option C). The
                // chain handles XEP-0045 §7.4 occupancy validation,
                // §7.5 visitor-may-not-speak, XEP-0359 stanza-id
                // stamping, XEP-0421 occupant-id stamping, XEP-0313
                // archive-eligibility, XEP-0424 retraction tombstone
                // emission, per-occupant inbox projection, and
                // per-occupant fan-out — emitting typed
                // [`OutboundEvent`]s the interpreter then resolves
                // recursively below.
                let nested =
                    Box::pin(dispatch_to_room(deps, room, *message, recursion_depth)).await;
                let InterpretOutcome {
                    frames: nested_frames,
                    close: nested_close,
                    feedback: nested_feedback,
                } = nested;
                outcome.frames.extend(nested_frames);
                if nested_close {
                    outcome.close = true;
                }
                outcome.feedback.extend(nested_feedback);
            }
            OutboundEvent::ProjectInbox {
                owner,
                peer,
                message,
                archive_ref,
                increment_unread,
            } => {
                let Some(inbox_storage) = deps.inbox_storage else {
                    debug!(
                        owner = %owner,
                        peer = %peer,
                        "ProjectInbox: no inbox_storage in Deps; skipping (test fixture?)"
                    );
                    continue;
                };
                // Build the inbox entry from the typed message, then
                // overwrite its stanza-id with the typed `archive_ref`
                // so the inbox row links to the canonicalized MAM
                // entry the handler stamped (rather than re-deriving
                // from the wire `<message id=...>`).
                let timestamp = chrono::Utc::now().timestamp();
                let mut entry = direct_message_entry(peer.clone(), &message, timestamp);
                entry.last_stanza_id = archive_ref.id.as_str().to_string();
                if let Err(error) = inbox_storage.upsert(&owner, entry, increment_unread).await {
                    warn!(
                        owner = %owner,
                        peer = %peer,
                        %error,
                        "ProjectInbox: inbox upsert failed; dropping projection"
                    );
                } else {
                    debug!(
                        owner = %owner,
                        peer = %peer,
                        archive_ref = archive_ref.id.as_str(),
                        increment_unread,
                        "ProjectInbox: persisted"
                    );
                }
            }
            OutboundEvent::SendCarbons {
                owner,
                message,
                kind,
                exclude,
            } => {
                // Per XEP-0280 §5, a carbon copy is the original
                // <message/> wrapped in <sent>/<received> →
                // <forwarded xmlns='urn:xmpp:forward:0'> → original.
                // The outer envelope is addressed FROM the user's
                // bare JID TO the receiving resource. We fan out only
                // to other resources of `owner` that have explicitly
                // opted in via XEP-0280 enable.
                //
                // Suppression rules (groupchat, <private/>, no-copy,
                // body-less) are enforced by `CarbonsMessageHandler`
                // before emitting this event; the interpreter does
                // not re-check them — but it DOES per-target filter
                // through `get_other_carbon_resources_for_user` so a
                // resource that disabled carbons after the message
                // entered the pipeline still gets skipped.
                let owner_str = owner.to_string();
                let live_targets = registry.get_other_carbon_resources_for_user(&owner, &exclude);
                // Detached-but-resumable resources (XEP-0198 stream
                // management) — without this fan-out arm, briefly
                // disconnected secondary devices would silently lose
                // carbon copies during their detached window. The
                // legacy `message.rs` path queues carbons on detached
                // resources via
                // `record_stanza_for_detached_bound_resource`; the
                // interpreter does the same here.
                let detached_targets: Vec<jid::FullJid> = match deps.sm_session_registry {
                    Some(sm) => sm
                        .detached_carbon_resources_for_user(&owner, &exclude)
                        .await
                        .unwrap_or_else(|error| {
                            warn!(
                                owner = %owner,
                                %error,
                                "SendCarbons: failed to enumerate detached SM resources; \
                                 falling back to live-only fan-out"
                            );
                            Vec::new()
                        }),
                    None => Vec::new(),
                };
                if live_targets.is_empty() && detached_targets.is_empty() {
                    debug!(
                        owner = %owner,
                        kind = ?kind,
                        "SendCarbons: no carbon-enabled resources to fan out to"
                    );
                    continue;
                }
                for target in live_targets {
                    let envelope = match build_carbon_envelope(kind, &message, &owner_str, &target)
                    {
                        Ok(env) => env,
                        Err(error) => {
                            warn!(
                                target = %target,
                                kind = ?kind,
                                %error,
                                "SendCarbons: failed to build envelope; skipping target"
                            );
                            continue;
                        }
                    };
                    match registry.send_to(&target, Stanza::Message(envelope)).await {
                        waddle_xmpp::registry::SendResult::Sent => {
                            debug!(target = %target, kind = ?kind, "SendCarbons: delivered");
                        }
                        waddle_xmpp::registry::SendResult::NotConnected => {
                            // Race between get_other_carbon_resources and
                            // send_to — the resource transitioned to
                            // detached. Benign: if it's resumable the
                            // detached pass below picks it up;
                            // otherwise the carbon is dropped per
                            // standard offline-delivery semantics.
                            debug!(
                                target = %target,
                                kind = ?kind,
                                "SendCarbons: target offline at fan-out time, dropping"
                            );
                        }
                        waddle_xmpp::registry::SendResult::ChannelClosed => {
                            warn!(
                                target = %target,
                                kind = ?kind,
                                "SendCarbons: target channel closed, dropping"
                            );
                        }
                    }
                }
                // Detached pass — queue the same envelope for replay
                // when the resource resumes its XEP-0198 session.
                if let Some(sm) = deps.sm_session_registry {
                    for target in detached_targets {
                        let envelope =
                            match build_carbon_envelope(kind, &message, &owner_str, &target) {
                                Ok(env) => env,
                                Err(error) => {
                                    warn!(
                                        target = %target,
                                        kind = ?kind,
                                        %error,
                                        "SendCarbons: failed to build detached envelope; skipping"
                                    );
                                    continue;
                                }
                            };
                        let stanza = Stanza::Message(envelope);
                        match sm
                            .record_stanza_for_detached_bound_resource(&target, &stanza)
                            .await
                        {
                            Ok(true) => {
                                debug!(
                                    target = %target,
                                    kind = ?kind,
                                    "SendCarbons: queued for detached XEP-0198 resume"
                                );
                            }
                            Ok(false) => {
                                debug!(
                                    target = %target,
                                    kind = ?kind,
                                    "SendCarbons: detached session expired between enumeration \
                                     and queue; dropping"
                                );
                            }
                            Err(error) => {
                                warn!(
                                    target = %target,
                                    kind = ?kind,
                                    %error,
                                    "SendCarbons: failed to queue carbon for detached resource"
                                );
                            }
                        }
                    }
                }
            }
            OutboundEvent::LookupArchivedMessage {
                id,
                archive,
                reference,
            } => {
                let result = lookup_archived_message(deps, &archive, &reference).await;
                outcome
                    .feedback
                    .push(InboundEvent::ArchivedMessageLoaded { id, result });
            }
            OutboundEvent::RegisterConnection(jid) => {
                warn!(
                    variant = "RegisterConnection",
                    jid = %jid,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::UnregisterConnection(jid) => {
                let _entry = registry.unregister(&jid);
                debug!(jid = %jid, "UnregisterConnection: removed from registry");
            }
            OutboundEvent::ArchiveGroupchat {
                room,
                sender,
                message,
                sender_nickname_generation,
            } => {
                let Some(mam_storage) = deps.mam_storage else {
                    debug!(
                        room = %room,
                        sender = %sender,
                        "ArchiveGroupchat: no mam_storage in Deps; skipping (test fixture?)"
                    );
                    continue;
                };
                // Per XEP-0313 §5.1.3 the eligibility check ran inside
                // `MucArchiveHandler` before this event was emitted —
                // the interpreter only persists. Mirrors the legacy
                // `archive_groupchat_message` projection: derive a
                // fresh archive id, stamp the canonical
                // `<stanza-id by='room'/>` for replay, then persist.
                // `sender_nickname_generation` rides on the event so
                // we don't pay a second `RoomActor::GetRoomSnapshot`
                // round-trip per archive write (Copilot review on
                // PR #279).
                let archive_id = match archive_groupchat_message(
                    mam_storage,
                    &room,
                    &message,
                    sender_nickname_generation,
                )
                .await
                {
                    Some(id) => id,
                    None => continue,
                };
                debug!(
                    room = %room,
                    archive_id,
                    "ArchiveGroupchat: persisted"
                );
            }
            OutboundEvent::ApplyGroupchatRetractionTombstone {
                room,
                target_message_id,
                retraction_message,
            } => {
                let Some(mam_storage) = deps.mam_storage else {
                    debug!(
                        room = %room,
                        target = %target_message_id,
                        "ApplyGroupchatRetractionTombstone: no mam_storage in Deps; skipping"
                    );
                    continue;
                };
                apply_groupchat_retraction_tombstone(
                    mam_storage,
                    deps.sm_session_registry,
                    &room,
                    &target_message_id,
                    &retraction_message,
                )
                .await;
            }
            OutboundEvent::PersistRoomSubject {
                room,
                texts,
                setter,
                setter_nick,
                set_at,
            } => {
                let Some(room_registry) = deps.room_registry else {
                    debug!(
                        room = %room,
                        "PersistRoomSubject: no room_registry in Deps; skipping"
                    );
                    continue;
                };
                let room_actor = match room_registry
                    .ask(GetRoom {
                        room_jid: room.clone(),
                    })
                    .await
                {
                    Ok(Some(actor)) => actor,
                    Ok(None) => {
                        debug!(
                            room = %room,
                            "PersistRoomSubject: room not registered; skipping"
                        );
                        continue;
                    }
                    Err(error) => {
                        warn!(
                            room = %room,
                            error = ?error,
                            "PersistRoomSubject: room registry lookup failed; skipping"
                        );
                        continue;
                    }
                };
                if let Err(error) = room_actor
                    .ask(SetSubject {
                        texts,
                        setter: setter.clone(),
                        setter_nick,
                        set_at,
                    })
                    .await
                {
                    warn!(
                        room = %room,
                        setter = %setter,
                        error = ?error,
                        "PersistRoomSubject: SetSubject ask failed; subject left at previous state"
                    );
                }
            }
            OutboundEvent::ProjectGroupchatInbox {
                owner,
                room,
                message,
                is_recipient,
                thread,
                dispatch_timestamp,
            } => {
                let Some(inbox_storage) = deps.inbox_storage else {
                    debug!(
                        owner = %owner,
                        room = %room,
                        "ProjectGroupchatInbox: no inbox_storage in Deps; skipping (test fixture?)"
                    );
                    continue;
                };
                project_groupchat_inbox(
                    inbox_storage,
                    deps.connection_registry,
                    &owner,
                    &room,
                    &message,
                    is_recipient,
                    &thread,
                    dispatch_timestamp,
                )
                .await;
            }
            OutboundEvent::ArchiveDirect {
                archive_jid,
                from,
                to,
                message,
            } => {
                let Some(mam_storage) = deps.mam_storage else {
                    debug!(
                        archive_jid = %archive_jid,
                        from = %from,
                        to = %to,
                        "ArchiveDirect: no mam_storage in Deps; skipping (test fixture?)"
                    );
                    continue;
                };
                // Per XEP-0313 §5.1.3, the eligibility check is
                // upstream (ArchiveHandler) — the interpreter just
                // persists. The handler also already canonicalized the
                // XEP-0359 `<stanza-id by=archive_jid/>` stamp on the
                // typed message, so the projection serializer captures
                // it for replay.
                let archived = build_direct_archived_message(
                    &jid::Jid::from(archive_jid.clone()),
                    jid::Jid::from(from.clone()),
                    jid::Jid::from(to.clone()),
                    &message,
                );
                match mam_storage.store_message(&archive_jid, &archived).await {
                    Ok(archive_id) => {
                        debug!(
                            archive_jid = %archive_jid,
                            archive_id,
                            "ArchiveDirect: persisted"
                        );
                    }
                    Err(error) => {
                        // Archive errors must not block dispatch — the
                        // message is already on the wire to other
                        // resources via routing/carbons. Log and drop.
                        warn!(
                            archive_jid = %archive_jid,
                            from = %from,
                            to = %to,
                            %error,
                            "ArchiveDirect: store_message failed; dropping archive write"
                        );
                    }
                }

                // XEP-0424 §"prevent further distribution": when the
                // archived message is itself a retraction *request*,
                // replace the target message in this archive with a
                // tombstone. The dispatcher's
                // `RichTargetValidationHandler` already authorized
                // the request (same-author check via
                // `LookupArchivedMessage`), so the only remaining
                // step is the in-place tombstone replace. Mirrors
                // the legacy `apply_retraction_tombstones` helper
                // (which `handle_message` invoked inline) — once per
                // archive write so both sender's and recipient's
                // archives observe the tombstone independently.
                if let Some(waddle_xmpp::xep::xep0424::RetractionKind::Request(retraction)) =
                    waddle_xmpp::xep::xep0424::extract_retraction_from_message(&message)
                {
                    apply_retraction_tombstone(
                        mam_storage,
                        deps.sm_session_registry,
                        &archive_jid,
                        &retraction.retracts_id,
                        &message,
                    )
                    .await;
                }
            }
            OutboundEvent::RequestEnrichment { id, message } => {
                let enriched = enrich_message_event(deps, *message).await;
                outcome.feedback.push(InboundEvent::EnrichmentComplete {
                    id,
                    message: Box::new(enriched),
                });
            }
            OutboundEvent::AskSfu { id, .. } => {
                warn!(
                    variant = "AskSfu",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::QueryMam { id, .. } => {
                warn!(
                    variant = "QueryMam",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::LoadScramCredentials { id, .. } => {
                warn!(
                    variant = "LoadScramCredentials",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::ValidateOAuthBearer { id, .. } => {
                warn!(
                    variant = "ValidateOAuthBearer",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::SetTimer { id, duration_ms } => {
                warn!(
                    variant = "SetTimer",
                    timer_id = id.0,
                    duration_ms,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::CancelTimer(id) => {
                warn!(
                    variant = "CancelTimer",
                    timer_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
        }
    }

    outcome
}

/// Run XEP-0372 link enrichment against a typed message and return
/// the rewritten value. Fails open: when no extension manager is
/// supplied (test fixture, or a connection that opted out) or any
/// extension actor returns an error, the original message is
/// returned unchanged. This matches legacy `message.rs` behavior so
/// the dispatcher cutover (#229 PR9) does not regress UX when a wasm
/// extension is misbehaving.
/// Run the headless offline-recipient pass for a bare-JID target
/// whose user has no available resources (#229 PR15).
///
/// Constructs a transient [`XmppStateMachine`] for the offline
/// recipient on the local domain, seeds its XEP-0191 blocklist from
/// [`Deps::blocking_storage`], drives the recipient pass via
/// [`waddle_xmpp::protocol::InboundEvent::StanzaFromPeer`], and
/// recursively interprets the resulting outbound events with
/// `recursion_depth` bumped so the inner pass can't loop on
/// `RouteToConnection`. Per-event behaviour during the inner
/// `interpret_with_depth` call:
///
/// - [`OutboundEvent::ArchiveDirect`] persists to recipient's MAM
///   archive (the desired effect of the headless pass).
/// - [`OutboundEvent::ProjectInbox`] writes the recipient's inbox
///   row (the other desired effect).
/// - [`OutboundEvent::SendCarbons`] (kind=`Received`, emitted by
///   `CarbonsMessageHandler` on `Locality::Recipient`) goes through
///   the standard fan-out: live carbon-enabled resources of the
///   recipient (necessarily empty here because we're in the
///   offline branch — `select_routable_resources_for_user` returned
///   no live resources) and detached XEP-0198 SM sessions. Queueing
///   to detached SM sessions is *correct*: when those sessions
///   resume, they receive the recipient-side carbon copy per
///   XEP-0280 §6 — the same delivery they would get had the bare-JID
///   target been routable. This is not a no-op in production; it
///   is the offline counterpart to the live recipient pass's carbon
///   fan-out.
/// - [`OutboundEvent::SendStanza`] frames have no wire to write to
///   (the transient SM is ephemeral and never connected to a
///   transport), so the inner outcome's `frames` are discarded.
/// - [`OutboundEvent::RequestEnrichment`] does not fire from a
///   recipient pass: `EnrichmentDispatchHandler::is_eligible`
///   returns false when the local user is not the sender, so no
///   parking / callback round-trip is initiated. Enrichment was
///   already performed by the sender pass before this event was
///   emitted.
/// - Nested [`OutboundEvent::RouteToConnection`] drops via the
///   depth-cap in the outer arm, regardless of full/bare or live
///   targets.
///
/// **Blocklist load failure is fail-closed** (Copilot review on
/// PR #275): when [`BlockingStorage::list_blocked_jids`] returns an
/// error, this helper returns early without touching the recipient's
/// archive or inbox. Mirrors `load_blocklist_for_bind`'s policy.
/// Degrading to an empty blocklist would silently disable XEP-0191
/// incoming-block enforcement and risk persisting blocked messages
/// into the recipient's MAM / inbox.
///
/// The synthetic full-JID resource is the shared
/// [`waddle_xmpp::protocol::HEADLESS_RECIPIENT_RESOURCE`] constant because
/// the recipient-pass
/// [`waddle_xmpp::protocol::session_state::Locality`] derivation
/// matches `to` against the bound bare JID — the resource value is
/// irrelevant for locality, and the synthetic resource never reaches
/// the wire (no `SendStanza` frames bubble out).
async fn run_headless_recipient_pass(
    deps: &Deps<'_>,
    recipient_bare: &jid::BareJid,
    stanza: Stanza,
    depth: u8,
) {
    let Some(dispatcher) = deps.message_dispatcher else {
        debug!(
            bare_jid = %recipient_bare,
            "headless recipient-pass: no message_dispatcher in Deps; \
             skipping (test fixture)"
        );
        return;
    };

    // Synthetic FullJid for `transition_to_ready`. The resource value
    // is irrelevant — the recipient pass derives `Locality::Recipient`
    // from bare-as-bare matching when `to` is bare.
    let synthetic_resource =
        match jid::ResourcePart::new(waddle_xmpp::protocol::HEADLESS_RECIPIENT_RESOURCE) {
            Ok(rp) => rp,
            Err(error) => {
                warn!(
                    bare_jid = %recipient_bare,
                    %error,
                    "headless recipient-pass: synthetic resource part rejected; \
                     skipping (should not happen — static literal)"
                );
                return;
            }
        };
    let synthetic_full = recipient_bare.with_resource(&synthetic_resource);

    // Fail-closed on blocklist load error (Copilot review on PR #275).
    // Mirroring `load_blocklist_for_bind`'s fail-closed semantic and
    // PR13's bind-time policy: a transient storage error must not
    // disable XEP-0191 incoming-block enforcement, otherwise a blocked
    // sender could be persisted into the offline recipient's MAM /
    // inbox. We skip the recipient pass entirely; the outer arm has
    // already logged the routing intent, and the sender's archive
    // entry survives independently of the recipient pass.
    let blocklist = match deps.blocking_storage {
        Some(storage) => match storage.list_blocked_jids(recipient_bare).await {
            Ok(jids) => Blocklist::new(jids),
            Err(error) => {
                warn!(
                    bare_jid = %recipient_bare,
                    error = %error,
                    "headless recipient-pass: blocklist load failed; skipping \
                     recipient-side processing to preserve XEP-0191 incoming-block \
                     enforcement (fail-closed)"
                );
                return;
            }
        },
        None => Blocklist::empty(),
    };

    let mut transient = XmppStateMachine::new(deps.local_domain, (**dispatcher).clone());
    transient.set_has_live_transport(false);
    transient.transition_to_ready(synthetic_full, false);
    transient.set_blocklist(blocklist);

    let events = transient.handle(InboundEvent::StanzaFromPeer(Box::new(stanza)));

    // Recursively interpret with the depth bumped. The inner outcome
    // is *discarded*: the transient SM is ephemeral so any frames
    // (SendStanza) have no wire to write to and any feedback events
    // (callback completions) belong to a state machine that goes out
    // of scope at function return.
    let nested = Box::pin(interpret_with_depth(events, deps, depth)).await;
    let InterpretOutcome {
        frames,
        close,
        feedback,
    } = nested;
    debug!(
        bare_jid = %recipient_bare,
        discarded_frames = frames.len(),
        discarded_feedback = feedback.len(),
        nested_close = close,
        "headless recipient-pass: completed; transient outcome discarded"
    );
}

/// Apply a XEP-0424 §"prevent further distribution" tombstone to the
/// retraction target inside `archive`. Looks up the target via the
/// retraction's wire id (matches legacy
/// `lookup_retraction_target_message`), then replaces the row with a
/// tombstone using `mam_storage.replace_with_tombstone`.
///
/// Called from the [`OutboundEvent::ArchiveDirect`] arm once per
/// archive write, so sender's and recipient's archives both
/// independently observe the tombstone. Failures are logged at WARN
/// and ignored — the retraction message itself was already archived
/// and the original is the SHOULD-be-tombstoned target, never the
/// authoritative payload after this point.
async fn apply_retraction_tombstone(
    mam_storage: &Arc<dyn MamStorage>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    archive: &jid::BareJid,
    target_wire_id: &str,
    retraction_message: &Message,
) {
    let original = match mam_storage
        .get_message_by_message_id(archive, target_wire_id)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            debug!(
                archive = %archive,
                target = target_wire_id,
                "ApplyRetractionTombstone: target not found in archive; skipping"
            );
            return;
        }
        Err(error) => {
            warn!(
                archive = %archive,
                target = target_wire_id,
                %error,
                "ApplyRetractionTombstone: archive lookup failed; skipping"
            );
            return;
        }
    };
    let Some(retraction_id) = retraction_message
        .id
        .clone()
        .and_then(waddle_xmpp::mam::RichMessageId::new)
    else {
        warn!(
            archive = %archive,
            target = target_wire_id,
            "ApplyRetractionTombstone: retraction stanza missing valid message id; skipping"
        );
        return;
    };
    let tombstone = waddle_xmpp::mam::ArchivedTombstone {
        retraction_id: Some(retraction_id),
        stamp: chrono::Utc::now(),
        moderation: None,
    };
    match mam_storage
        .replace_with_tombstone(&original.id, tombstone)
        .await
    {
        Ok(true) => {
            debug!(
                archive = %archive,
                original_id = %original.id,
                "ApplyRetractionTombstone: replaced row with tombstone"
            );
        }
        Ok(false) => {
            warn!(
                archive = %archive,
                original_id = %original.id,
                "ApplyRetractionTombstone: target row not found at replace time"
            );
        }
        Err(error) => {
            warn!(
                archive = %archive,
                original_id = %original.id,
                %error,
                "ApplyRetractionTombstone: replace_with_tombstone failed"
            );
        }
    }
    // Drop matching unacked outbound copies from any detached XEP-0198
    // session queues so a recipient mid-resume does not replay the
    // pre-scrub stanza on the wire. XEP-0424 §"prevent further
    // distribution" applies to in-flight as well as archived copies.
    // Scope by the recipient archive's bare JID so a colliding wire id
    // in another conversation is not accidentally scrubbed (Codex P1).
    scrub_unacked_for_tombstone(
        sm_session_registry,
        target_wire_id,
        &archive.to_string(),
        "ApplyRetractionTombstone",
    )
    .await;
}

/// Deliver a single `Stanza` to a specific full-JID destination as
/// a [`waddle_xmpp::registry::DeliveryKind::PeerStanza`] so the
/// destination's main loop runs the recipient pass before any wire
/// write. Centralizes the per-target send + result-logging shape so
/// both the full-JID and bare-JID-resource-selection arms of
/// [`OutboundEvent::RouteToConnection`] go through the same path.
///
/// On `NotConnected` / `ChannelClosed`, falls back to recording the
/// stanza on the recipient's detached XEP-0198 stream-management
/// session (when one exists) so a recipient that's mid-resume doesn't
/// silently lose direct messages — matching the legacy
/// `handle_message` direct-route semantics. Cross-domain bare JIDs and
/// truly-offline recipients still drop here; the bare-JID arm above
/// runs the headless recipient pass for offline persistence
/// (archive/inbox/incoming-block) on local domains.
async fn deliver_peer_to_full(
    registry: &waddle_xmpp::registry::ConnectionRegistry,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    target: &jid::FullJid,
    stanza: &Stanza,
) {
    // The live-send path needs ownership for `send_peer_to`; the
    // detached fallback only borrows. Clone once here on the live
    // branch so the caller hands us an `&Stanza` and avoids a
    // redundant clone per live target on the bare-JID fan-out hot
    // path (Copilot review on PR #276).
    match registry.send_peer_to(target, stanza.clone()).await {
        waddle_xmpp::registry::SendResult::Sent => {
            debug!(jid = %target, "RouteToConnection: peer-stanza queued for recipient pass");
        }
        waddle_xmpp::registry::SendResult::NotConnected
        | waddle_xmpp::registry::SendResult::ChannelClosed => {
            // Known limitation (Copilot review on PR #276): queues the
            // pre-recipient-pass stanza into the detached XEP-0198
            // replay buffer. Replay sends the stored XML verbatim
            // WITHOUT a recipient-pass dispatch, so the replayed
            // message is missing the recipient-side
            // `<stanza-id by='recipient'/>` (XEP-0359 §5) and
            // recipient-side filtering / archive / inbox effects don't
            // fire. Matches LEGACY behaviour (which had no recipient
            // pass at all) and is therefore not a regression. Closing
            // the gap properly requires running the headless recipient
            // pass per detached target and queueing its `SendStanza`
            // output — tracked as a follow-up to #229.
            if let Some(sm) = sm_session_registry {
                match sm
                    .record_stanza_for_detached_bound_resource(target, stanza)
                    .await
                {
                    Ok(true) => {
                        debug!(
                            jid = %target,
                            "RouteToConnection: recipient detached, queued for XEP-0198 replay"
                        );
                    }
                    Ok(false) => {
                        debug!(
                            jid = %target,
                            "RouteToConnection: target offline and no detached session, dropping"
                        );
                    }
                    Err(error) => {
                        warn!(
                            jid = %target,
                            %error,
                            "RouteToConnection: failed to record stanza for detached resource"
                        );
                    }
                }
            } else {
                debug!(jid = %target, "RouteToConnection: target offline, dropping");
            }
        }
    }
}

/// Run the [`OutboundEvent::DispatchToRoom`] arm against the stateless
/// room handler chain (#229 PR18 — MUC cutover).
///
/// Order of operations:
///
/// 1. Enrich the message via [`ExtensionManager::enrich_message_for_waddle`]
///    (XEP-0372 link previews, fail-open).
/// 2. Resolve the per-room actor and ask for a frozen
///    [`waddle_xmpp::muc::room_actor::RoomChainSnapshot`] in one
///    round-trip.
/// 3. Validate rich-message targets (XEP-0308 corrections, XEP-0424
///    retractions) against the room archive — uses the snapshot's
///    `sender_nickname_generation` for the XEP-0308 §3 occupancy
///    continuity check. On Err, returns a typed message-error reply
///    in `outcome.frames`.
/// 4. Resolve the managed-room owner override (announcements room
///    admits server owners only). Hoisted from the chain so the
///    chain stays sync.
/// 5. Run the room handler chain — emits `SendStanza` (typed error
///    replies), `ArchiveGroupchat`, `ApplyGroupchatRetractionTombstone`,
///    `ProjectGroupchatInbox`, and `RouteToConnection` per occupant.
/// 6. Recursively interpret those events; the sender's echo flows
///    through `RouteToConnection` to the sender's own connection.
///
/// Returns an [`InterpretOutcome`] so the caller (the
/// `OutboundEvent::DispatchToRoom` arm) can fold the nested frames /
/// feedback / close into the parent outcome.
async fn dispatch_to_room(
    deps: &Deps<'_>,
    room_jid: jid::BareJid,
    incoming: Message,
    recursion_depth: u8,
) -> InterpretOutcome {
    let mut outcome = InterpretOutcome::default();
    let Some(state) = deps.web_socket_state else {
        warn!(
            variant = "DispatchToRoom",
            room = %room_jid,
            "DispatchToRoom: no web_socket_state in Deps; dropping. \
             Production must populate web_socket_state."
        );
        return outcome;
    };
    let Some(room_registry) = deps.room_registry else {
        warn!(
            variant = "DispatchToRoom",
            room = %room_jid,
            "DispatchToRoom: no room_registry in Deps; dropping"
        );
        return outcome;
    };
    let Some(sender_full) = incoming
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        warn!(
            room = %room_jid,
            "DispatchToRoom: message.from is missing or not a full JID; dropping"
        );
        return outcome;
    };

    // 1. Prepare the prototype the room gate sees. Enrichment is delayed
    //    until after occupancy / managed-room validation so unauthorized
    //    senders receive the XEP-0045 room error before any Waddle-specific
    //    extension payload checks or extension runtime calls.
    let mut prototype = incoming.clone();
    if prototype.id.is_none() {
        prototype.id = Some(uuid::Uuid::new_v4().to_string());
    }
    prototype.type_ = XmppMessageType::Groupchat;
    // Strip any client-claimed `<stanza-id by='room'/>` so the chain's
    // canonicalize handler stamps the canonical value. Mirrors the
    // legacy `remove_stanza_ids_by` call.
    remove_stanza_ids_by(&mut prototype, &jid::Jid::from(room_jid.clone()));
    // 2. Look up the room actor + snapshot in one round-trip each.
    let room_actor = match room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            warn!(room = %room_jid, "DispatchToRoom: room not registered; dropping");
            return outcome;
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "DispatchToRoom: room registry lookup failed; dropping"
            );
            return outcome;
        }
    };
    let snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: sender_full.clone(),
        })
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "DispatchToRoom: GetRoomSnapshot failed; dropping"
            );
            return outcome;
        }
    };

    // 3. Managed-room owner override (announcements room admits
    //    server owners only). Pre-derived synchronously here so the
    //    chain's `OccupancyValidationHandler` can read
    //    `managed_room_forbidden` without an async permission call.
    let managed_room_forbidden =
        if parse_managed_room_jid(&room_jid).as_deref() == Some("announcements") {
            !session_is_server_owner(state, deps.authenticated_session).await
        } else {
            false
        };

    // 4. Run the chain's occupancy / managed-room gate FIRST, BEFORE
    //    rich-target validation (Copilot review on PR #279). Otherwise
    //    a non-occupant or managed-room-forbidden sender would receive
    //    rich-target errors (potentially leaking archive-derived info
    //    like `<item-not-found/>`) instead of the required XEP-0045
    //    §7.4 `<not-acceptable/>` / managed-room `<forbidden/>` reply.
    //    The gate handler (`OccupancyValidationHandler`) is sync and
    //    pure — calling it directly here is equivalent to running a
    //    one-handler dispatcher, with no extra allocation.
    let occupants: Vec<OccupantSnapshot> = snapshot
        .occupants
        .iter()
        .map(|o| OccupantSnapshot {
            full_jid: o.full_jid.clone(),
            nick: o.nick.clone(),
            affiliation: o.affiliation,
            role: o.role,
        })
        .collect();
    let id_gen = UuidV4Generator;
    // Capture a single dispatch timestamp here so every per-occupant
    // `ProjectGroupchatInbox` event the chain emits carries the same
    // value (Copilot review on PR #279). Avoids per-occupant
    // `Utc::now()` drift across a second-boundary.
    let dispatch_timestamp = chrono::Utc::now().timestamp();
    normalize_thread_create_source(&mut prototype);
    let gate_ctx = RoomContext {
        room: &room_jid,
        sender_full: &sender_full,
        occupants: &occupants,
        managed_room_forbidden,
        room_moderated: snapshot.config.moderated,
        id_gen: &id_gen,
        occupant_id_secret: &state.deps.occupant_id_secret,
        sender_nickname_generation: snapshot.sender_nickname_generation.unwrap_or(0),
        project_sender_inbox: true,
        dispatch_timestamp,
    };
    let mut gate_working = prototype.clone();
    remove_framework_envelopes(&mut gate_working);
    use waddle_xmpp::protocol::room::RoomHandler;
    let gate_outcome =
        waddle_xmpp::protocol::room::occupancy_validation::OccupancyValidationHandler
            .handle(&mut gate_working, &gate_ctx);
    if let waddle_xmpp::protocol::room::RoomHandlerOutcome::Halt(gate_events) = gate_outcome {
        // Fold the nested outcome's full state — frames, close
        // signal, and async-callback feedback — back into the outer
        // outcome (Copilot review on PR #279). Dropping `close` /
        // `feedback` would silently lose stream-close requests or
        // pending callback completions if a future gate handler ever
        // emits them.
        let nested = Box::pin(interpret_with_depth(gate_events, deps, recursion_depth)).await;
        outcome.frames.extend(nested.frames);
        outcome.close = outcome.close || nested.close;
        outcome.feedback.extend(nested.feedback);
        return outcome;
    }

    if message_has_framework_envelope(&prototype) {
        let mut sanitized = incoming.clone();
        remove_framework_envelopes(&mut sanitized);
        let reply = build_message_error_reply(
            &sanitized,
            &room_jid,
            &sender_full,
            bad_request_error("Client-authored Waddle extension envelopes are not allowed."),
        );
        match Stanza::Message(reply).to_element_string() {
            Ok(xml) => outcome.frames.push(xml),
            Err(error) => {
                warn!(
                    room = %room_jid,
                    %error,
                    "DispatchToRoom: failed to serialize framework-envelope rejection"
                );
            }
        }
        return outcome;
    }

    // 5. Enrich the message before the post-gate chain sees it. The legacy
    //    bridge enriched on the prototype before
    //    `BuildGroupchatBroadcast`, so reflected copies carry the
    //    enrichment payloads. Fail-open: extension errors leave the
    //    message unchanged.
    let waddle_id = waddle_id_for_room_jid(&room_jid);
    let sender_room_nick_jid = snapshot
        .sender_nick
        .as_deref()
        .and_then(|nick| room_jid.clone().with_resource_str(nick).ok().map(Jid::from));
    if let Some(sender_room_nick_jid) = sender_room_nick_jid.as_ref() {
        prototype.from = Some(sender_room_nick_jid.clone());
    }
    let _extension_outcome = state
        .deps
        .protocol
        .extension_manager
        .process_message_enrichments_for_waddle_with_requester(
            &mut prototype,
            waddle_id,
            Some(sender_full.to_bare()),
        )
        .await;
    // 6. Rich-target validation against the room archive. Runs only
    //    after the gate has admitted the sender, so non-occupants /
    //    managed-room-forbidden senders never see archive-derived
    //    error conditions. Archive rows store `from` in the XEP-0045
    //    §7.2.13 `room/nick` form (the chain stamps it AFTER
    //    validation), so derive that view here for the same-sender
    //    comparison rather than relying on `prototype.from` (alice's
    //    real full JID).
    if let Err(stanza_error) = validate_groupchat_rich_targets(
        deps,
        &room_jid,
        &prototype,
        sender_room_nick_jid.as_ref(),
        &room_actor,
        snapshot.sender_nickname_generation,
    )
    .await
    {
        let reply = build_message_error_reply(&incoming, &room_jid, &sender_full, stanza_error);
        match Stanza::Message(reply).to_element_string() {
            Ok(xml) => outcome.frames.push(xml),
            Err(error) => {
                warn!(
                    room = %room_jid,
                    %error,
                    "DispatchToRoom: failed to serialize rich-target error reply"
                );
            }
        }
        return outcome;
    }

    // 7. Build context + run the rest of the chain (canonicalize,
    //    archive, inbox, reflect). Reuse the `gate_ctx` config — same
    //    snapshot, same managed-room flag, same id-gen.
    let ctx = RoomContext {
        room: &room_jid,
        sender_full: &sender_full,
        occupants: &occupants,
        managed_room_forbidden,
        // XEP-0045 §7.5 (Copilot review on PR #279): the chain's
        // `OccupancyValidationHandler` enforces visitor-may-not-speak
        // against this flag + the sender's snapshot role, replacing
        // the legacy `RoomActor::BuildGroupchatBroadcast` check that
        // previously emitted `RoomActorError::VisitorMayNotSpeak`.
        room_moderated: snapshot.config.moderated,
        id_gen: &id_gen,
        occupant_id_secret: &state.deps.occupant_id_secret,
        // Carry the sender's nickname-generation through the chain
        // so `MucArchiveHandler` can stamp it directly on
        // `OutboundEvent::ArchiveGroupchat`. Avoids a second
        // `RoomActor::GetRoomSnapshot` round-trip per groupchat
        // archive write (Copilot review on PR #279).
        sender_nickname_generation: snapshot.sender_nickname_generation.unwrap_or(0),
        project_sender_inbox: true,
        dispatch_timestamp,
    };
    let mut working = prototype;
    // Run only the post-gate pipeline (canonicalize → archive → inbox
    // → reflector). The occupancy gate already ran above as an
    // explicit stand-alone call (Copilot review on PR #279); using
    // the full `default_room_dispatcher()` here would re-run it.
    let dispatch_outcome = default_room_pipeline_dispatcher().dispatch(&mut working, &ctx);
    let observer_message = working.clone();

    // 6. Recursively interpret the chain's emitted events. Pass the
    //    depth through unchanged: `recursion_depth` is the headless
    //    offline-recipient pass guard, and the room handler chain
    //    legitimately emits one `RouteToConnection` per occupant —
    //    including offline ones, which the `RouteToConnection` arm
    //    promotes to a headless recipient pass (depth bumped there).
    //    Bumping here would break that path for every offline
    //    occupant.
    let nested = Box::pin(interpret_with_depth(
        dispatch_outcome.events,
        deps,
        recursion_depth,
    ))
    .await;
    outcome.frames.extend(nested.frames);
    if nested.close {
        outcome.close = true;
    }
    outcome.feedback.extend(nested.feedback);

    let mut observer_message = observer_message;
    let observer_outcome = state
        .deps
        .protocol
        .extension_manager
        .process_message_observers_for_waddle_with_requester(
            &mut observer_message,
            waddle_id_for_room_jid(&room_jid),
            Some(sender_full.to_bare()),
        )
        .await;
    for effect in observer_outcome.effects {
        if let ExtensionEffect::HostWarning(message) = effect {
            warn!(warning = %message.as_str(), "extension message observer emitted host warning");
            let reply = build_message_error_reply(
                &incoming,
                &room_jid,
                &sender_full,
                service_unavailable_error(message.as_str()),
            );
            match Stanza::Message(reply).to_element_string() {
                Ok(xml) => outcome.frames.push(xml),
                Err(error) => {
                    warn!(
                        room = %room_jid,
                        %error,
                        "DispatchToRoom: failed to serialize extension warning error reply"
                    );
                }
            }
        }
    }

    outcome
}

pub(crate) async fn dispatch_extension_bot_groupchat_response(
    deps: &Deps<'_>,
    room_jid: BareJid,
    response: ExtensionRoomMessage,
) -> Result<ExtensionRoomDispatchResult, ExtensionBotDispatchError> {
    let mut outcome = InterpretOutcome::default();
    let Some(state) = deps.web_socket_state else {
        warn!(
            room = %room_jid,
            "Extension bot groupchat dispatch has no WebSocket state; dropping"
        );
        return Err(ExtensionBotDispatchError::MissingWebSocketState);
    };
    let Some(room_registry) = deps.room_registry else {
        warn!(
            room = %room_jid,
            "Extension bot groupchat dispatch has no room registry; dropping"
        );
        return Err(ExtensionBotDispatchError::MissingRoomRegistry);
    };
    let room_actor = match room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            warn!(room = %room_jid, "Extension bot groupchat room not registered; dropping");
            return Err(ExtensionBotDispatchError::RoomNotRegistered);
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Extension bot groupchat room lookup failed; dropping"
            );
            return Err(ExtensionBotDispatchError::RoomLookupFailed);
        }
    };
    let bot_full = bot_full_jid(&state.deps.auth_state.xmpp_domain);
    match room_actor
        .ask(GetAffiliation {
            jid: bot_full.to_bare(),
        })
        .await
    {
        Ok(waddle_xmpp::Affiliation::Outcast) => {
            warn!(
                room = %room_jid,
                bot = %bot_full,
                "Extension bot is outcast from room; dropping room message"
            );
            return Err(ExtensionBotDispatchError::BotOutcast);
        }
        Ok(_) => {}
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Extension bot affiliation lookup failed; dropping"
            );
            return Err(ExtensionBotDispatchError::RoomLookupFailed);
        }
    }
    let initial_snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: bot_full.clone(),
        })
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Extension bot groupchat snapshot failed; dropping"
            );
            return Err(ExtensionBotDispatchError::SnapshotFailed);
        }
    };
    let initial_occupants: Vec<OccupantSnapshot> = initial_snapshot
        .occupants
        .iter()
        .map(|o| OccupantSnapshot {
            full_jid: o.full_jid.clone(),
            nick: o.nick.clone(),
            affiliation: o.affiliation,
            role: o.role,
        })
        .collect();
    let bot_nick = available_bot_nick(&initial_occupants);
    match room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bot_full.clone(),
            nick: bot_nick.clone(),
            effective_affiliation: waddle_xmpp::Affiliation::Member,
            local_domain: state.deps.auth_state.xmpp_domain.clone(),
        })
        .await
    {
        Ok(join) => {
            if !join.is_same_bare_multi_session_join {
                for existing in join.existing_occupants {
                    let from = match room_jid.clone().with_resource_str(&bot_nick) {
                        Ok(from) => from,
                        Err(error) => {
                            warn!(
                                room = %room_jid,
                                %error,
                                "Extension bot presence could not build room occupant JID"
                            );
                            continue;
                        }
                    };
                    let bot_bare = bot_full.to_bare();
                    let presence = waddle_xmpp::muc::build_occupant_presence(
                        &from,
                        &existing.jid,
                        join.new_occupant_affiliation,
                        join.new_occupant_role,
                        false,
                        &waddle_xmpp::xep::xep0421::OccupantIdentity {
                            bare_jid: &bot_bare,
                            real_jid: Some(&bot_full),
                            secret: &state.deps.occupant_id_secret,
                        },
                    );
                    let _ = state
                        .deps
                        .protocol
                        .connection_registry
                        .try_send_to(&existing.jid, Stanza::Presence(presence));
                }
            }
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Extension bot could not join room; dropping room message"
            );
            return Err(ExtensionBotDispatchError::BotJoinFailed);
        }
    }
    let snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: bot_full.clone(),
        })
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Extension bot groupchat snapshot after join failed; dropping"
            );
            return Err(ExtensionBotDispatchError::SnapshotFailed);
        }
    };
    let occupants: Vec<OccupantSnapshot> = snapshot
        .occupants
        .iter()
        .map(|o| OccupantSnapshot {
            full_jid: o.full_jid.clone(),
            nick: o.nick.clone(),
            affiliation: o.affiliation,
            role: o.role,
        })
        .collect();
    let nested = dispatch_bot_groupchat_response(
        deps,
        BotGroupchatDispatch {
            room_jid: &room_jid,
            occupants: &occupants,
            sender_full: &bot_full,
            room_actor: Some(&room_actor),
            room_moderated: snapshot.config.moderated,
            dispatch_timestamp: chrono::Utc::now().timestamp(),
            recursion_depth: 0,
            occupant_id_secret: &state.deps.occupant_id_secret,
        },
        response,
    )
    .await
    .map_err(|error| {
        warn!(
            room = %room_jid,
            error = ?error,
            "Extension bot groupchat dispatch failed"
        );
        error
    })?;
    outcome.frames.extend(nested.outcome.frames);
    outcome.close = outcome.close || nested.outcome.close;
    outcome.feedback.extend(nested.outcome.feedback);
    Ok(ExtensionRoomDispatchResult {
        outcome,
        stanza_id: nested.stanza_id,
    })
}

struct BotGroupchatDispatch<'a> {
    room_jid: &'a BareJid,
    occupants: &'a [OccupantSnapshot],
    sender_full: &'a FullJid,
    room_actor: Option<&'a ActorRef<RoomActor>>,
    room_moderated: bool,
    dispatch_timestamp: i64,
    recursion_depth: u8,
    occupant_id_secret: &'a waddle_xmpp::xep::xep0421::OccupantIdSecret,
}

async fn dispatch_bot_groupchat_response(
    deps: &Deps<'_>,
    bot_ctx: BotGroupchatDispatch<'_>,
    response: ExtensionRoomMessage,
) -> Result<ExtensionRoomDispatchResult, ExtensionBotDispatchError> {
    let mut outcome = InterpretOutcome::default();
    if response.room.as_str() != bot_ctx.room_jid.to_string() {
        warn!(
            room = %bot_ctx.room_jid,
            response_room = response.room.as_str(),
            "Extension room message room did not match dispatch room; dropping"
        );
        return Err(ExtensionBotDispatchError::RoomMismatch);
    }

    let mut working = Message::new(Some(Jid::from(bot_ctx.room_jid.clone())));
    working.id = Some(
        response
            .stanza_id
            .as_ref()
            .map(|id| id.as_str().to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
    );
    working.from = Some(Jid::from(bot_ctx.sender_full.clone()));
    working.type_ = XmppMessageType::Groupchat;
    working.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body(response.body.as_str().to_string()),
    );

    if let Some(thread_id) = response.thread_id.as_ref() {
        set_thread_id(&mut working, thread_id.as_str());
    }
    if let Some(reply_to) = response.reply_to.as_ref() {
        let mut reply = ReplyReference::new(reply_to.id.as_str());
        if let Some(to) = reply_to
            .to
            .as_ref()
            .and_then(|to| room_scoped_reply_to_attr(to.as_str(), bot_ctx.room_jid))
        {
            reply = reply.with_to(to);
        }
        set_reply_payload(&mut working, &reply);
    }
    if let Some(extensions) = response.extensions.as_ref() {
        working.payloads.push(extensions.to_minidom());
    }

    if let Some(room_actor) = bot_ctx.room_actor {
        if let Err(stanza_error) = validate_groupchat_rich_targets(
            deps,
            bot_ctx.room_jid,
            &working,
            None,
            room_actor,
            Some(0),
        )
        .await
        {
            warn!(
                room = %bot_ctx.room_jid,
                error = ?stanza_error,
                "Extension room message failed rich-target validation; dropping"
            );
            return Err(ExtensionBotDispatchError::RichTargetInvalid);
        }
    }

    let id_gen = UuidV4Generator;
    let ctx = RoomContext {
        room: bot_ctx.room_jid,
        sender_full: bot_ctx.sender_full,
        occupants: bot_ctx.occupants,
        managed_room_forbidden: false,
        room_moderated: bot_ctx.room_moderated,
        id_gen: &id_gen,
        occupant_id_secret: bot_ctx.occupant_id_secret,
        sender_nickname_generation: 0,
        project_sender_inbox: false,
        dispatch_timestamp: bot_ctx.dispatch_timestamp,
    };

    let dispatch_outcome = default_room_pipeline_dispatcher().dispatch(&mut working, &ctx);
    let stanza_id = extract_room_stanza_id(&working, bot_ctx.room_jid)
        .and_then(|id| StanzaId::new(id).ok())
        .ok_or(ExtensionBotDispatchError::MissingCanonicalStanzaId)?;
    let nested = Box::pin(interpret_with_depth(
        dispatch_outcome.events,
        deps,
        bot_ctx.recursion_depth,
    ))
    .await;
    outcome.frames.extend(nested.frames);
    outcome.close = nested.close;
    outcome.feedback.extend(nested.feedback);
    Ok(ExtensionRoomDispatchResult { outcome, stanza_id })
}

pub(crate) struct ExtensionRoomMessage {
    pub body: DisplayText,
    pub room: RoomJid,
    pub stanza_id: Option<StanzaId>,
    pub thread_id: Option<ThreadId>,
    pub reply_to: Option<ReplyTarget>,
    pub extensions: Option<ExtensionEnvelope>,
}

pub(crate) struct ExtensionRoomDispatchResult {
    pub outcome: InterpretOutcome,
    pub stanza_id: StanzaId,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExtensionBotDispatchError {
    #[error("extension bot dispatch has no WebSocket state")]
    MissingWebSocketState,
    #[error("extension bot dispatch has no room registry")]
    MissingRoomRegistry,
    #[error("extension bot dispatch room is not registered")]
    RoomNotRegistered,
    #[error("extension bot dispatch room lookup failed")]
    RoomLookupFailed,
    #[error("extension bot dispatch room snapshot failed")]
    SnapshotFailed,
    #[error("extension bot is outcast from the room")]
    BotOutcast,
    #[error("extension bot could not join the room")]
    BotJoinFailed,
    #[error("extension room message target did not match dispatch room")]
    RoomMismatch,
    #[error("extension room message failed rich-target validation")]
    RichTargetInvalid,
    #[error("extension room message did not receive a canonical room stanza id")]
    MissingCanonicalStanzaId,
}

fn bot_full_jid(account_domain: &str) -> FullJid {
    format!("waddle-ai@extensions.{account_domain}/bot")
        .parse::<FullJid>()
        .expect("configured XMPP domain produces a valid extension bot JID")
}

fn available_bot_nick(occupants: &[OccupantSnapshot]) -> String {
    const BASE: &str = "waddle";
    if !occupants.iter().any(|occupant| occupant.nick == BASE) {
        return BASE.to_string();
    }
    for suffix in 2.. {
        let candidate = format!("{BASE}-{suffix}");
        if !occupants.iter().any(|occupant| occupant.nick == candidate) {
            return candidate;
        }
    }
    unreachable!("unbounded suffix search always returns")
}

fn normalize_thread_create_source(message: &mut Message) -> Option<String> {
    let Some(ForumAction::CreateThread(_)) = extract_forum_action(message) else {
        return None;
    };
    let thread_id = message
        .thread
        .as_ref()
        .map(|thread| thread.0.clone())
        .or_else(|| message.id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if message.id.is_none() {
        message.id = Some(thread_id.clone());
    }
    if message.thread.is_none() {
        set_thread_id(message, &thread_id);
    }
    Some(thread_id)
}

#[cfg(test)]
fn message_thread_id(message: &Message) -> Option<String> {
    message
        .thread
        .as_ref()
        .map(|thread| thread.0.clone())
        .or_else(|| {
            extract_forum_action(message).and_then(|action| match action {
                ForumAction::Reply(reply) => Some(reply.thread_id),
                ForumAction::CreateThread(_) => message.id.clone(),
            })
        })
}

/// Resolve the managed-room owner override against the deployment
/// permission actor. Mirrors the legacy
/// `session_is_server_owner` helper that lived on the legacy MUC
/// bridge — kept here so the room handler chain can stay synchronous
/// and the async permission-actor call lands in the interpreter.
async fn session_is_server_owner(state: &WebSocketState, session: Option<&Session>) -> bool {
    let Some(session) = session else {
        return false;
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
            subject: Subject::user(&session.user_id),
            permission: Permission::Owner,
        })
        .await
        .is_ok_and(|response| response.allowed)
}

async fn enrich_message_event(deps: &Deps<'_>, message: Message) -> Message {
    if deps.extension_manager.is_none() {
        debug!(
            "RequestEnrichment: no extension_manager in Deps; \
             feeding original message back unchanged"
        );
        return message;
    }
    debug!("RequestEnrichment: direct messages do not carry a typed Waddle scope; skipping");
    message
}

fn waddle_id_for_room_jid(room_jid: &BareJid) -> WaddleId {
    let value = if parse_managed_room_jid(room_jid).is_some() {
        "space".to_string()
    } else {
        "default".to_string()
    };
    WaddleId::new(value).expect("static room Waddle scope is non-empty")
}

/// Resolve a typed [`MessageRef`] against `archive`'s personal MAM
/// archive and project the storage row into the typed protocol
/// [`ProtocolArchivedMessage`] shape the
/// [`waddle_xmpp::protocol::handlers::rich_target_validation::RichTargetValidationHandler`]
/// expects on the [`InboundEvent::ArchivedMessageLoaded`] callback.
///
/// Storage failures are demoted to `Ok(None)` with a WARN log so the
/// handler treats them as `<item-not-found>` per XEP-0308 / 0424 /
/// 0425 / 0461 — the same surface clients see when the target
/// genuinely doesn't exist. We do not propagate the storage error
/// shape into the callback because the protocol-side type does not
/// model it and the resulting reply would be the same.
async fn lookup_archived_message(
    deps: &Deps<'_>,
    archive: &jid::BareJid,
    reference: &MessageRef,
) -> Option<Box<ProtocolArchivedMessage>> {
    let Some(mam_storage) = deps.mam_storage else {
        debug!(
            archive = %archive,
            "LookupArchivedMessage: no mam_storage in Deps; treating as not-found"
        );
        return None;
    };
    let lookup = match reference {
        MessageRef::StanzaId { id, .. } => {
            // Strict stanza-id match: `get_message_by_message_id`
            // matches only the `stanza_id` column (not `origin_id`)
            // so the OR-collision identified in #229 PR8 review
            // (origin-id colliding with someone else's stanza-id)
            // can't return the wrong row.
            mam_storage
                .get_message_by_message_id(archive, id.as_str())
                .await
        }
        MessageRef::OriginId { sender, origin_id } => {
            // No origin-id-only accessor on `MamStorage` today, so we
            // narrow with `MamQuery.with = sender` (storage-level
            // sender filter) and pick the first row whose `origin_id`
            // *or wire `id` attribute* (`stanza_id` column) matches
            // the requested value. The fall-through to the wire id
            // mirrors the legacy `lookup_correction_target_message`
            // behaviour — many clients (and the CUE e2e scenarios)
            // omit the explicit `<origin-id/>` payload and rely on
            // the message's `id` attribute as the correction
            // target. Sender-bound matching keeps the
            // OR-collision protection from #229 PR8: only rows sent
            // by the original author can satisfy the correction.
            let query = waddle_xmpp::mam::MamQuery {
                with: Some(jid::Jid::from(sender.clone())),
                ..Default::default()
            };
            match mam_storage.query_messages(archive, &query).await {
                Ok(result) => Ok(result.messages.into_iter().find(|row| {
                    row_matches_origin_id(row, sender, origin_id.as_str())
                        || row_matches_wire_id(row, sender, origin_id.as_str())
                })),
                Err(e) => Err(e),
            }
        }
    };
    match lookup {
        Ok(Some(row)) => project_archived_row(archive, row),
        Ok(None) => None,
        Err(error) => {
            warn!(
                archive = %archive,
                %error,
                "LookupArchivedMessage: storage error; treating as not-found"
            );
            None
        }
    }
}

/// Verify a storage row genuinely matches the typed
/// [`MessageRef::OriginId`] key — `origin_id` field equality plus
/// `sender` (`from`) equality via parsed [`jid::BareJid`] comparison.
/// String equality on `from` would mishandle case-folded localparts
/// or full-JID-vs-bare-JID strings; the typed compare normalizes both.
fn row_matches_origin_id(
    row: &MamArchivedMessage,
    expected_sender: &jid::BareJid,
    expected_origin_id: &str,
) -> bool {
    if row.origin_id.as_ref().map(|o| o.id.as_str()) != Some(expected_origin_id) {
        return false;
    }
    row.from.to_bare() == *expected_sender
}

/// Fallback match for the legacy XEP-0308 correction-target shape
/// where the message carries no explicit `<origin-id/>` payload —
/// matches the row's wire `id` attribute (the `stanza_id` storage
/// column) to the correction's `replaces_id`. Same sender-bound
/// scoping as [`row_matches_origin_id`].
fn row_matches_wire_id(
    row: &MamArchivedMessage,
    expected_sender: &jid::BareJid,
    expected_wire_id: &str,
) -> bool {
    if row.stanza_id.as_ref().map(|s| s.id.as_str()) != Some(expected_wire_id) {
        return false;
    }
    row.from.to_bare() == *expected_sender
}

/// Project a storage [`MamArchivedMessage`] row into the protocol-side
/// [`ProtocolArchivedMessage`] consumed by handler completions.
///
/// Falls back to an empty body-only stanza if `stanza_xml` is missing
/// or unparseable — the rich-target handler primarily inspects the
/// tombstone state and the sender's bare JID, both of which we can
/// reconstruct without the original wire form. Logs a WARN at each
/// projection failure mode so regressions stay observable.
fn project_archived_row(
    archive: &jid::BareJid,
    row: MamArchivedMessage,
) -> Option<Box<ProtocolArchivedMessage>> {
    let tombstoned = matches!(
        row.rich.as_ref().and_then(|r| r.payload.as_ref()),
        Some(waddle_xmpp::mam::ArchivedRichPayload::Tombstone(_))
    );

    let message = match parse_archived_message_xml(row.stanza_xml.as_deref()) {
        Some(m) => m,
        None => fallback_archived_message(&row),
    };

    let stamp_id = row
        .stanza_id
        .as_ref()
        .map(|s| s.id.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| row.id.clone());
    let stanza_id = StanzaIdRef {
        by: archive.clone(),
        id: StanzaIdValue::new(&stamp_id),
    };

    Some(Box::new(ProtocolArchivedMessage {
        stanza_id,
        message: Box::new(message),
        tombstoned,
    }))
}

fn parse_archived_message_xml(xml: Option<&str>) -> Option<Message> {
    let xml = xml?;
    let element = match Element::from_str(xml) {
        Ok(e) => e,
        Err(error) => {
            warn!(
                %error,
                "LookupArchivedMessage: failed to parse stored stanza_xml; \
                 falling back to body-only reconstruction"
            );
            return None;
        }
    };
    let element_name = element.name().to_string();
    let element_ns = element.ns().to_string();
    match Message::try_from(element) {
        Ok(message) => Some(message),
        Err(error) => {
            warn!(
                %error,
                element_name = %element_name,
                element_ns = %element_ns,
                "LookupArchivedMessage: stored stanza_xml parsed but failed \
                 to convert into xmpp_parsers::message::Message; falling \
                 back to body-only reconstruction"
            );
            None
        }
    }
}

fn fallback_archived_message(row: &MamArchivedMessage) -> Message {
    let mut msg = Message::new(Some(row.to.clone()));
    msg.from = Some(row.from.clone());
    msg.id = row.stanza_id.as_ref().map(|s| s.id.clone());
    // RFC 6121 §5.2.3: only emit `<body>` if the archived row recorded
    // one. `Some("")` round-trips as an empty `<body></body>` element;
    // `None` produces no `<body>` element at all (subject-only,
    // reaction-only, etc.).
    if let Some(body) = row.body.as_deref() {
        msg.bodies
            .insert(String::new(), xmpp_parsers::message::Body(body.to_owned()));
    }
    msg
}

/// Build the XEP-0297-wrapped carbon envelope for `kind`. Pulled out
/// so the live-resources fan-out and the detached-XEP-0198 fan-out
/// share one builder.
fn build_carbon_envelope(
    kind: CarbonKind,
    original: &xmpp_parsers::message::Message,
    owner_bare: &str,
    target_full: &jid::FullJid,
) -> Result<xmpp_parsers::message::Message, jid::Error> {
    let target = target_full.to_string();
    match kind {
        CarbonKind::Sent => build_sent_carbon(original, owner_bare, &target),
        CarbonKind::Received => build_received_carbon(original, owner_bare, &target),
    }
}

/// Helper trait so the interpreter has a single, typed serialization
/// entry point for any `Stanza` leaving the state machine. Keeping it
/// private to this module prevents callers from serializing stanzas in
/// other spots — the I/O boundary stays narrow.
trait ToElementString {
    fn to_element_string(&self) -> Result<String, waddle_xmpp::XmppError>;
}

impl ToElementString for waddle_xmpp::Stanza {
    fn to_element_string(&self) -> Result<String, waddle_xmpp::XmppError> {
        use waddle_xmpp::Stanza;
        match self {
            Stanza::Iq(iq) => stanza_to_string(iq.clone()),
            Stanza::Message(msg) => message_to_string(msg),
            Stanza::Presence(p) => stanza_to_string(p.clone()),
        }
    }
}

// -----------------------------------------------------------------------
// MUC cutover helpers (#229 PR18)
// -----------------------------------------------------------------------

/// Validate XEP-0308 corrections / XEP-0424 retractions against the
/// room archive. Mirrors the legacy `validate_rich_message_targets`
/// helper that lived on the legacy MUC bridge but operates against
/// `Deps::mam_storage` directly so the chain runner can invoke it
/// without standing up the legacy `WebSocketState` plumbing.
///
/// Returns `Ok(())` when the message has no rich payload requiring
/// validation (or when validation passes), or `Err(StanzaError)` with
/// the typed error reply the caller surfaces in `outcome.frames`.
async fn validate_groupchat_rich_targets(
    deps: &Deps<'_>,
    room: &BareJid,
    message: &Message,
    sender_room_nick_jid: Option<&Jid>,
    room_actor: &ActorRef<RoomActor>,
    sender_nickname_generation: Option<u64>,
) -> Result<(), StanzaError> {
    if message.from.is_none() {
        return Ok(());
    }
    if has_malformed_rich_payload(message) {
        return Err(bad_request_error(
            "Rich-message payload is missing a required identifier or contains an invalid JID.",
        ));
    }
    let Some(mam_storage) = deps.mam_storage else {
        // No archive available — nothing to validate against. Mirrors
        // the legacy bridge's `state.deps.protocol.mam_storage` use:
        // production always supplies it; in test fixtures without
        // storage we treat the validation as a no-op.
        return Ok(());
    };
    // The archive stores `from` in the XEP-0045 §7.2.13 `room/nick`
    // form (the chain stamps it AFTER validation), so the
    // same-sender check compares against the sender's room/nick view
    // — not against `prototype.from` (alice's real full JID, set by
    // the user-side state machine before `DispatchToRoom` was
    // emitted). When the snapshot has no nick for the sender (sender
    // not currently joined under any nickname), any rich-target
    // operation is forbidden because we cannot satisfy the
    // continuity check.
    let Some(sender_archive_view) = sender_room_nick_jid else {
        if extract_correction_from_message(message).is_some()
            || matches!(
                extract_retraction_from_message(message),
                Some(RetractionKind::Request(_))
            )
        {
            return Err(forbidden_error(
                "Sender is not joined to the room; rich-target operations require occupancy.",
            ));
        }
        return Ok(());
    };

    if let Some(correction) = extract_correction_from_message(message) {
        let original = match mam_storage
            .get_message_by_message_id(room, &correction.replaces_id)
            .await
        {
            Ok(Some(original)) => original,
            Ok(None) => return Err(item_not_found_error("Correction target not found.")),
            Err(_) => return Err(internal_server_error_for_lookup()),
        };
        if !sender_matches_groupchat_from(sender_archive_view, &original.from) {
            return Err(forbidden_error(
                "Only the original sender may correct a message.",
            ));
        }
        verify_groupchat_occupancy_generation(
            sender_archive_view,
            &original,
            room_actor,
            sender_nickname_generation,
        )
        .await?;
    }

    if let Some(RetractionKind::Request(retraction)) = extract_retraction_from_message(message) {
        let original =
            match lookup_groupchat_retraction_target(mam_storage, room, &retraction.retracts_id)
                .await
            {
                Ok(Some(original)) => original,
                Ok(None) => return Err(item_not_found_error("Retraction target not found.")),
                Err(_) => return Err(internal_server_error_for_lookup()),
            };
        if !sender_matches_groupchat_from(sender_archive_view, &original.from) {
            return Err(forbidden_error(
                "Only the original sender may retract a message.",
            ));
        }
    }
    Ok(())
}

async fn lookup_groupchat_retraction_target(
    mam_storage: &Arc<dyn MamStorage>,
    room: &BareJid,
    target_id: &str,
) -> Result<Option<MamArchivedMessage>, waddle_xmpp::mam::MamStorageError> {
    mam_storage
        .get_message(target_id)
        .await
        .map(|message| message.filter(|message| message.to.to_bare() == *room))
}

/// Compare a XEP-0045 sender (the in-room full JID `room/nick`) against
/// the archived `from` JID for groupchat ownership checks. Both are
/// typed `Jid` so we can compare structurally without round-tripping
/// through strings.
fn sender_matches_groupchat_from(sender: &Jid, original_from: &Jid) -> bool {
    sender == original_from
}

/// XEP-0308 §3 occupancy continuity check: a full-JID that left the
/// room and rejoined under the same nickname MUST NOT be allowed to
/// correct messages from the previous occupancy. Compares the
/// per-nickname generation captured on the archive row at write time
/// against the room actor's current generation for the sender's
/// nickname.
async fn verify_groupchat_occupancy_generation(
    sender: &Jid,
    original: &MamArchivedMessage,
    room_actor: &ActorRef<RoomActor>,
    sender_current_generation: Option<u64>,
) -> Result<(), StanzaError> {
    let Some(nick) = sender.resource().map(|r| r.to_string()) else {
        return Err(forbidden_error(
            "Correction sender has no MUC nickname for occupancy check.",
        ));
    };
    let Some(archived_generation) = original.nickname_generation else {
        return Err(forbidden_error(
            "Original message predates occupancy tracking; correction window has closed.",
        ));
    };
    // Prefer the generation snapshot already captured by `dispatch_to_room`
    // (it came from the same `GetRoomSnapshot` query that populated the
    // chain context); fall back to a fresh per-nickname query if the
    // snapshot didn't include the sender (unlikely — would mean the
    // sender is not joined under any nickname).
    let current_generation = match sender_current_generation {
        Some(value) => value,
        None => match room_actor
            .ask(GetNicknameGeneration { nick: nick.clone() })
            .await
        {
            Ok(value) => value,
            Err(_) => return Err(internal_server_error_for_lookup()),
        },
    };
    if current_generation != archived_generation {
        return Err(forbidden_error(
            "Occupancy generation has advanced; correction is no longer permitted across the leave/rejoin boundary.",
        ));
    }
    Ok(())
}

fn has_malformed_rich_payload(message: &Message) -> bool {
    message.payloads.iter().any(|payload| {
        (payload.ns() == NS_MESSAGE_CORRECT
            && payload.name() == "replace"
            && payload.attr("id").is_none_or(str::is_empty))
            || (payload.ns() == NS_MESSAGE_RETRACT
                && payload.name() == "retract"
                && payload.attr("id").is_none_or(str::is_empty))
            || (payload.ns() == NS_REACTIONS
                && payload.name() == "reactions"
                && payload.attr("id").is_none_or(str::is_empty))
            || (payload.ns() == NS_REPLY
                && payload.name() == "reply"
                && (payload.attr("id").is_none_or(str::is_empty)
                    || payload.attr("to").is_some_and(|to| {
                        to.trim().is_empty() || to.trim().parse::<Jid>().is_err()
                    })))
            || (payload.ns() == NS_REFERENCE
                && payload.name() == "reference"
                && (payload.attr("type").is_none_or(str::is_empty)
                    || payload.attr("uri").is_none_or(str::is_empty)))
            || (payload.ns() == NS_EXPLICIT_MENTIONS
                && payload.name() == "mention"
                && payload.attr("jid").is_none_or(str::is_empty)
                && payload.attr("occupantid").is_none_or(str::is_empty)
                && payload.attr("mentions").is_none_or(str::is_empty))
    })
}

fn remove_framework_envelopes(message: &mut Message) {
    message
        .payloads
        .retain(|payload| !payload.ns().starts_with("urn:waddle:"));
}

fn bad_request_error(text: &str) -> StanzaError {
    StanzaError::new(ErrorType::Modify, DefinedCondition::BadRequest, "en", text)
}

fn item_not_found_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        "en",
        text,
    )
}

fn forbidden_error(text: &str) -> StanzaError {
    StanzaError::new(ErrorType::Auth, DefinedCondition::Forbidden, "en", text)
}

fn service_unavailable_error(text: &str) -> StanzaError {
    StanzaError::new(
        ErrorType::Wait,
        DefinedCondition::ServiceUnavailable,
        "en",
        text,
    )
}

fn internal_server_error_for_lookup() -> StanzaError {
    StanzaError::new(
        ErrorType::Wait,
        DefinedCondition::InternalServerError,
        "en",
        "Archive lookup failed while validating rich-message target.",
    )
}

/// Build a typed `<message type='error'>` reply addressed from the
/// room JID back to the sender. Mirrors the legacy `error_message`
/// helper.
fn build_message_error_reply(
    incoming: &Message,
    room: &BareJid,
    sender: &FullJid,
    error: StanzaError,
) -> Message {
    let mut reply = incoming.clone();
    reply.type_ = XmppMessageType::Error;
    reply.from = Some(Jid::from(room.clone()));
    reply.to = Some(Jid::from(sender.clone()));
    reply.payloads.push(Element::from(error));
    reply
}

/// Persist a groupchat message to the room MAM archive. Mirrors the
/// legacy `archive_groupchat_message` projection so MAM replay
/// reproduces the canonicalized stanza byte-for-byte.
///
/// The archive primary key is the XEP-0359 stanza-id the chain's
/// `MucCanonicalizeHandler` already stamped on the message — we
/// reuse it rather than generating a second uuid so the storage
/// row's primary key matches the wire `<stanza-id by='room'/>` value
/// (legacy invariant the rich-target lookups rely on).
async fn archive_groupchat_message(
    mam_storage: &Arc<dyn MamStorage>,
    room: &BareJid,
    message: &Message,
    sender_nickname_generation: u64,
) -> Option<String> {
    let archive_clone = message.clone();
    let archive_id = match extract_room_stanza_id(&archive_clone, room) {
        Some(id) => id,
        None => {
            // Chain bug: `MucCanonicalizeHandler` MUST stamp
            // `<stanza-id by='room'/>` before `MucArchiveHandler`
            // emits `ArchiveGroupchat`. Persisting a fresh archive-
            // only id here would break the "archive id == wire
            // stanza-id" invariant — clients reflecting back the wire
            // stanza-id (XEP-0308 corrections, XEP-0424 retractions)
            // would fail to resolve the archive row. Skip the write;
            // the reflection still goes out, and a separate audit can
            // surface the chain regression (Copilot review on
            // PR #279).
            warn!(
                room = %room,
                "ArchiveGroupchat: message has no `<stanza-id by='room'/>`; \
                 skipping archive write because persisting an archive-only id would \
                 break the wire/archive stanza-id invariant (chain bug)"
            );
            return None;
        }
    };

    finish_archive_groupchat_message(
        mam_storage,
        room,
        archive_clone,
        archive_id,
        sender_nickname_generation,
    )
    .await
}

fn extract_room_stanza_id(message: &Message, room: &BareJid) -> Option<String> {
    let room_str = room.to_string();
    message
        .payloads
        .iter()
        .filter(|payload| payload.name() == "stanza-id" && payload.ns() == STANZA_ID_NS)
        .find(|payload| payload.attr("by").is_some_and(|by| by == room_str.as_str()))
        .and_then(|payload| payload.attr("id").map(ToOwned::to_owned))
}

async fn finish_archive_groupchat_message(
    mam_storage: &Arc<dyn MamStorage>,
    room: &BareJid,
    archive_clone: Message,
    archive_id: String,
    sender_nickname_generation: u64,
) -> Option<String> {
    // RFC 6121 §5.2.3: `<body>` is optional. Preserve the
    // None-vs-empty distinction so subject-only / reaction-only
    // groupchat messages don't materialize a fake empty body in the
    // archive's denormalized projection.
    let body = prototype_body(&archive_clone);
    let reply = extract_groupchat_reply_reference(&archive_clone, room);
    let origin_id = extract_origin_id(&archive_clone);
    let rich = rich_archive_payload(&archive_clone);
    let stanza_xml = serialize_groupchat_stanza_xml(&archive_clone);

    // XEP-0201: read the typed thread info (id + optional parent) from the
    // post-reattach payload form. `protocol::frame::parse_stanza` calls
    // `reattach_thread_parent` at the inbound boundary so the parent
    // attribute survives `Message::try_from` here. The collapsed
    // `Option<ThreadInfo>` field on `ArchivedMessage` accepts the
    // helper's result directly.
    let thread = waddle_xmpp::xep0201::thread_info_from_message_in_stanza_ns(
        &archive_clone,
        waddle_xmpp::xep0201::CLIENT_STANZA_NS,
    );

    // XEP-0045 §7.2: groupchat reflections always carry an in-room
    // sender JID; we treat a missing `from` as a malformed reflection
    // and refuse the archive write rather than persisting a sentinel.
    // (The protocol-side handler stamps `from` before reaching this
    // arm, so this guard is defensive.)
    let Some(from_jid) = archive_clone.from.clone() else {
        warn!(
            room = %room,
            "ArchiveGroupchat: missing from JID on reflection; dropping archive write"
        );
        return None;
    };
    let room_jid_full = jid::Jid::from(room.clone());
    let stanza_id = archive_clone
        .id
        .clone()
        .map(|id| waddle_xmpp_core::xep0359::StanzaId::new(id, room_jid_full.clone()));
    let archived = MamArchivedMessage {
        id: archive_id,
        timestamp: chrono::Utc::now(),
        from: from_jid,
        to: room_jid_full,
        body,
        stanza_id,
        thread,
        reply,
        origin_id,
        // Typed propagation: see #228 commit 8 — `ArchivedMessage.message_type`
        // is now `xmpp_parsers::message::MessageType`, not `String`. The
        // wire-typed value flows directly through; no lossy stringifier.
        message_type: archive_clone.type_.clone(),
        stanza_xml,
        rich,
        nickname_generation: Some(sender_nickname_generation),
    };

    match mam_storage.store_message(room, &archived).await {
        Ok(archive_id) => Some(archive_id),
        Err(error) => {
            warn!(
                room = %room,
                %error,
                "ArchiveGroupchat: store_message failed; dropping archive write"
            );
            None
        }
    }
}

async fn apply_groupchat_retraction_tombstone(
    mam_storage: &Arc<dyn MamStorage>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    room: &BareJid,
    target_message_id: &str,
    retraction_message: &Message,
) {
    let original = match mam_storage.get_message(target_message_id).await {
        Ok(Some(row)) if row.to.to_bare() == *room => row,
        Ok(_) => {
            debug!(
                archive = %room,
                target = target_message_id,
                "ApplyGroupchatRetractionTombstone: target not found in room archive; skipping"
            );
            return;
        }
        Err(error) => {
            warn!(
                archive = %room,
                target = target_message_id,
                %error,
                "ApplyGroupchatRetractionTombstone: archive lookup failed; skipping"
            );
            return;
        }
    };
    let Some(retraction_id) = retraction_message.id.clone().and_then(RichMessageId::new) else {
        warn!(
            archive = %room,
            target = target_message_id,
            "ApplyGroupchatRetractionTombstone: retraction stanza missing valid message id; skipping"
        );
        return;
    };
    let tombstone = ArchivedTombstone {
        retraction_id: Some(retraction_id),
        stamp: chrono::Utc::now(),
        moderation: None,
    };
    match mam_storage
        .replace_with_tombstone(&original.id, tombstone)
        .await
    {
        Ok(true) => {
            debug!(
                archive = %room,
                original_id = %original.id,
                "ApplyGroupchatRetractionTombstone: replaced with tombstone"
            );
        }
        Ok(false) => warn!(
            archive = %room,
            original_id = %original.id,
            "ApplyGroupchatRetractionTombstone: target row not found at replace time"
        ),
        Err(error) => warn!(
            archive = %room,
            original_id = %original.id,
            %error,
            "ApplyGroupchatRetractionTombstone: replace_with_tombstone failed"
        ),
    }
    // Drop matching unacked groupchat reflections from detached
    // XEP-0198 session queues. The reflection is what occupants see;
    // scrubbing here closes the resume-side replay leak for groupchat
    // retractions identically to the 1:1 case. Scope by the room JID
    // so the matcher's stanza-id branch can find groupchat reflections
    // that key by the room's XEP-0359 stamp, and so a colliding wire
    // id in another conversation is not accidentally scrubbed
    // (Codex P1, Copilot review on PR #305).
    scrub_unacked_for_tombstone(
        sm_session_registry,
        target_message_id,
        &room.to_string(),
        "ApplyGroupchatRetractionTombstone",
    )
    .await;
}

/// Walk the SM session registry and drop every unacked outbound
/// `<message/>` entry that matches a XEP-0424 / XEP-0425 tombstone.
/// `target_id` is matched against either the cached message's wire
/// `id` attribute or any XEP-0359 `<stanza-id id='…'/>` child, scoped
/// to `archive_jid` so cross-conversation collateral damage is
/// impossible. Returns silently on any registry error (logged at
/// WARN) — the archive scrub has already happened, and dropping the
/// in-flight copy is best-effort.
async fn scrub_unacked_for_tombstone(
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    target_id: &str,
    archive_jid: &str,
    site: &'static str,
) {
    let Some(sm) = sm_session_registry else {
        return;
    };
    use waddle_xmpp::stream_management::SmSessionRegistry as _;
    match sm.scrub_unacked_for_tombstone(target_id, archive_jid).await {
        Ok(removed) if removed > 0 => {
            debug!(
                target = target_id,
                archive = archive_jid,
                removed,
                "{site}: scrubbed unacked SM queue entries for tombstoned message"
            );
        }
        Ok(_) => {}
        Err(error) => {
            warn!(
                target = target_id,
                archive = archive_jid,
                %error,
                "{site}: scrub_unacked_for_tombstone failed; pre-scrub stanza may still replay on resume"
            );
        }
    }
}

/// Apply the `(owner, room, message)` projection against the inbox
/// storage. Mirrors the legacy
/// `deliver_groupchat_via_room_actor`'s per-occupant
/// channel + thread upserts and the XEP-0430 inbox push to the
/// owner's other resources.
#[allow(clippy::too_many_arguments)]
async fn project_groupchat_inbox(
    inbox_storage: &Arc<dyn InboxStorage>,
    connection_registry: &waddle_xmpp::registry::ConnectionRegistry,
    owner: &BareJid,
    room: &BareJid,
    message: &Message,
    is_recipient: bool,
    thread: &Option<GroupchatThreadProjection>,
    dispatch_timestamp: i64,
) {
    let entry = groupchat_entry(room.clone(), message, dispatch_timestamp);
    match inbox_storage.upsert(owner, entry, is_recipient).await {
        Ok(updated) if is_recipient => {
            push_inbox_update(connection_registry, owner, &updated).await;
        }
        Ok(_) => {}
        Err(error) => {
            warn!(
                jid = %owner,
                room = %room,
                %error,
                "ProjectGroupchatInbox: channel-row upsert failed"
            );
        }
    }
    let Some(thread) = thread else {
        return;
    };
    let thread_entry = groupchat_thread_entry(
        room.clone(),
        message,
        dispatch_timestamp,
        &thread.thread_id,
        thread.title.as_deref(),
        thread.author_nick.as_deref(),
    );
    match inbox_storage
        .upsert(owner, thread_entry, is_recipient)
        .await
    {
        Ok(updated) if is_recipient => {
            push_inbox_update(connection_registry, owner, &updated).await;
        }
        Ok(_) => {}
        Err(error) => {
            warn!(
                jid = %owner,
                room = %room,
                %error,
                "ProjectGroupchatInbox: thread-row upsert failed"
            );
        }
    }
}

/// XEP-0430 inbox push to all live resources of `user`. Decoupled
/// from `WebSocketState` (Copilot review on PR #279) so unit tests
/// and non-WebSocket callers can drive the projection without
/// standing up the full route stack.
async fn push_inbox_update(
    connection_registry: &waddle_xmpp::registry::ConnectionRegistry,
    user: &BareJid,
    entry: &InboxEntry,
) {
    let resources = connection_registry.get_resources_for_user(user);
    for resource_jid in resources {
        let msg = build_inbox_push(Jid::from(resource_jid.clone()), entry);
        let _ = connection_registry
            .send_to(&resource_jid, Stanza::Message(msg))
            .await;
    }
}

fn extract_groupchat_reply_reference(
    message: &Message,
    room: &BareJid,
) -> Option<waddle_xmpp_core::mam::ArchivedReply> {
    use waddle_xmpp_core::mam::{ArchivedReply, RichMessageId};
    let reply = message
        .payloads
        .iter()
        .find(|payload| payload.name() == "reply" && payload.ns() == NS_REPLY)?;
    let id = RichMessageId::new(reply.attr("id")?)?;
    // XEP-0461 §3 makes `id` MUST and `to` SHOULD; for groupchat we
    // additionally restrict `to` to a room-scoped JID. A `to` that
    // fails the scope check is dropped (the reply still carries the
    // id) rather than rejecting the entire reply reference.
    let to = reply
        .attr("to")
        .and_then(|value| room_scoped_reply_to_attr(value, room));
    Some(ArchivedReply { id, to })
}

pub(crate) fn room_scoped_reply_to_attr(value: &str, room: &BareJid) -> Option<Jid> {
    value
        .parse::<Jid>()
        .ok()
        .filter(|jid| jid.to_bare() == *room)
}

fn extract_origin_id(message: &Message) -> Option<waddle_xmpp_core::xep0359::OriginId> {
    waddle_xmpp_core::xep0359::extract_origin_id(message)
}

fn prototype_body(message: &Message) -> Option<String> {
    message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .map(|body| body.0.clone())
}

fn serialize_groupchat_stanza_xml(message: &Message) -> Option<String> {
    let mut msg = message.clone();
    msg.to = None;
    match message_to_string(&msg) {
        Ok(xml) => Some(xml),
        Err(error) => {
            warn!(%error, "Failed to serialize groupchat stanza XML for MAM archive");
            None
        }
    }
}

fn rich_archive_payload(message: &Message) -> Option<ArchivedRichMessage> {
    let payload = extract_correction_from_message(message)
        .and_then(|correction| {
            RichMessageId::new(correction.replaces_id)
                .map(|replaces_id| ArchivedRichPayload::Correction { replaces_id })
        })
        .or_else(|| {
            extract_retraction_from_message(message).and_then(|kind| match kind {
                RetractionKind::Request(retraction) => RichMessageId::new(retraction.retracts_id)
                    .map(|target_id| {
                        ArchivedRichPayload::Retraction(ArchivedRetraction {
                            target_id,
                            stamp: None,
                            retraction_id: message.id.clone().and_then(RichMessageId::new),
                        })
                    }),
                RetractionKind::Tombstone(retracted) => message.id.clone().and_then(|id| {
                    RichMessageId::new(id).map(|target_id| {
                        ArchivedRichPayload::Retraction(ArchivedRetraction {
                            target_id,
                            stamp: retracted
                                .stamp
                                .as_deref()
                                .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
                                .map(|stamp| stamp.with_timezone(&chrono::Utc)),
                            retraction_id: RichMessageId::new(retracted.retraction_id),
                        })
                    })
                }),
            })
        })
        .or_else(|| {
            extract_reactions_from_message(message).and_then(|reactions| {
                RichMessageId::new(reactions.message_id).map(|target_id| {
                    ArchivedRichPayload::Reactions(ArchivedReactionSet {
                        target_id,
                        emojis: reactions
                            .emojis
                            .into_iter()
                            .filter_map(RichText::new)
                            .collect(),
                    })
                })
            })
        });
    let reply = parse_reply_from_message(message).and_then(|reply| {
        RichMessageId::new(reply.id).map(|id| ArchivedReply { id, to: reply.to })
    });
    let references = extract_references_from_message(message)
        .into_iter()
        .filter_map(|reference| {
            let ref_type = RichText::new(reference.ref_type.as_str())?;
            Some(ArchivedReference {
                ref_type,
                begin: reference.begin.and_then(|value| value.try_into().ok()),
                end: reference.end.and_then(|value| value.try_into().ok()),
                uri: RichText::new(reference.uri),
                anchor: reference.anchor.and_then(RichText::new),
            })
        })
        .collect::<Vec<_>>();
    let mentions = extract_explicit_mentions(message)
        .map(|mentions| {
            mentions
                .mentions
                .into_iter()
                .map(|mention| ArchivedMention {
                    begin: mention.begin,
                    end: mention.end,
                    jid: mention.jid,
                    occupant_id: mention.occupant_id.and_then(RichText::new),
                    mentions: mention.mentions.and_then(RichText::new),
                    uri: mention.uri.and_then(RichText::new),
                    active: mention.active,
                    noping: mention.noping,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if payload.is_none() && reply.is_none() && references.is_empty() && mentions.is_empty() {
        None
    } else {
        Some(ArchivedRichMessage {
            payload,
            reply,
            references,
            mentions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::xep::{set_thread_create, ThreadCreate};
    use waddle_xmpp::Stanza;
    use xmpp_parsers::iq::{Iq, IqType};
    use xmpp_parsers::minidom::Element;

    fn test_registry() -> ConnectionRegistry {
        ConnectionRegistry::new()
    }

    fn result_iq(id: &str) -> Iq {
        Iq {
            from: None,
            to: None,
            id: id.to_string(),
            payload: IqType::Result(Some(Element::builder("query", "jabber:iq:roster").build())),
        }
    }

    #[tokio::test]
    async fn interprets_send_stanza() {
        let events = vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq(
            "x",
        ))))];
        let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
        assert_eq!(outcome.frames.len(), 1);
        assert!(outcome.frames[0].contains("type=\"result\""));
        assert!(outcome.frames[0].contains("id=\"x\""));
        assert!(!outcome.close);
    }

    #[tokio::test]
    async fn interprets_close_transport() {
        let events = vec![OutboundEvent::CloseTransport];
        let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
        assert!(outcome.close);
        assert!(outcome.frames.is_empty());
    }

    #[tokio::test]
    async fn interprets_log_is_noop_for_caller() {
        let events = vec![OutboundEvent::Log {
            level: tracing::Level::INFO,
            message: "hello".to_string(),
        }];
        let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
        assert!(outcome.frames.is_empty());
        assert!(!outcome.close);
    }

    // -----------------------------------------------------------------
    // XEP-0280 — SendCarbons fan-out
    // -----------------------------------------------------------------

    fn chat_msg(from: &str, to: &str, body: &str) -> xmpp_parsers::message::Message {
        let mut m = xmpp_parsers::message::Message::new(Some(to.parse().expect("jid")));
        m.from = Some(from.parse().expect("jid"));
        m.type_ = xmpp_parsers::message::MessageType::Chat;
        m.bodies
            .insert(String::new(), xmpp_parsers::message::Body(body.to_string()));
        m
    }

    fn drain_inbound(
        rx: &mut tokio::sync::mpsc::Receiver<waddle_xmpp::registry::OutboundStanza>,
    ) -> Vec<waddle_xmpp::registry::OutboundStanza> {
        let mut out = Vec::new();
        while let Ok(stanza) = rx.try_recv() {
            out.push(stanza);
        }
        out
    }

    #[tokio::test]
    async fn xep_0280_send_carbons_fans_out_to_other_carbon_enabled_resources() {
        let registry = ConnectionRegistry::new();
        // Owner: alice. Two resources — web (originating, excluded)
        // and phone (carbon-enabled, expected target).
        let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let alice_phone: jid::FullJid = "alice@example.com/phone".parse().expect("jid");
        let (_web_tx, _web_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_web.clone(), _web_tx, true);
        let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_phone.clone(), phone_tx, true);

        let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
        let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::SendCarbons {
            owner,
            message: Box::new(original),
            kind: CarbonKind::Sent,
            exclude: alice_web,
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        // Verify the XEP-0280 <sent xmlns='urn:xmpp:carbons:2'> wrapper and
        // its nested XEP-0297 <forwarded xmlns='urn:xmpp:forward:0'> payload.
        let received = drain_inbound(&mut phone_rx);
        assert_eq!(received.len(), 1, "alice/phone received one carbon");
        let stanza = &received[0].stanza;
        let msg = match stanza {
            Stanza::Message(m) => m,
            other => panic!("expected Message stanza, got {other:?}"),
        };
        let sent = msg
            .payloads
            .iter()
            .find(|p| p.name() == "sent" && p.ns() == "urn:xmpp:carbons:2")
            .expect("carbon must carry <sent xmlns='urn:xmpp:carbons:2'/>");
        assert!(
            sent.children()
                .any(|p| p.name() == "forwarded" && p.ns() == "urn:xmpp:forward:0"),
            "carbon <sent/> must carry <forwarded xmlns='urn:xmpp:forward:0'/>"
        );
    }

    #[tokio::test]
    async fn xep_0280_send_carbons_skips_originating_resource() {
        let registry = ConnectionRegistry::new();
        let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let (web_tx, mut web_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_web.clone(), web_tx, true);

        let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
        let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::SendCarbons {
            owner,
            message: Box::new(original),
            kind: CarbonKind::Sent,
            exclude: alice_web,
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        // No carbon to alice/web — it's the originating resource.
        let received = drain_inbound(&mut web_rx);
        assert!(received.is_empty(), "originating resource excluded");
    }

    #[tokio::test]
    async fn xep_0280_send_carbons_skips_resources_without_carbons_enabled() {
        let registry = ConnectionRegistry::new();
        let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let alice_phone: jid::FullJid = "alice@example.com/phone".parse().expect("jid");
        let (_web_tx, _web_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_web.clone(), _web_tx, true);
        // alice/phone has carbons DISABLED.
        let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_phone.clone(), phone_tx, false);

        let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
        let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::SendCarbons {
            owner,
            message: Box::new(original),
            kind: CarbonKind::Sent,
            exclude: alice_web,
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        let received = drain_inbound(&mut phone_rx);
        assert!(received.is_empty(), "carbons-disabled resource skipped");
    }

    #[tokio::test]
    async fn xep_0280_send_carbons_received_kind_emits_received_envelope() {
        let registry = ConnectionRegistry::new();
        let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
        let (_desk_tx, _desk_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(bob_desk.clone(), _desk_tx, true);
        let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(bob_phone.clone(), phone_tx, true);

        let owner: jid::BareJid = "bob@example.com".parse().expect("bare");
        let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::SendCarbons {
            owner,
            message: Box::new(original),
            kind: CarbonKind::Received,
            exclude: bob_desk,
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        let received = drain_inbound(&mut phone_rx);
        assert_eq!(received.len(), 1);
        let msg = match &received[0].stanza {
            Stanza::Message(m) => m,
            other => panic!("expected Message, got {other:?}"),
        };
        assert!(
            msg.payloads
                .iter()
                .any(|p| p.name() == "received" && p.ns() == "urn:xmpp:carbons:2"),
            "kind=Received emits <received xmlns='urn:xmpp:carbons:2'/>"
        );
    }

    #[tokio::test]
    async fn xep_0280_send_carbons_queues_for_detached_xep_0198_resources() {
        // Regression test for the carbon-fan-out-skipping-detached-SM
        // bug: a XEP-0198-resumable session that briefly disconnected
        // must still receive its carbon copies via
        // record_stanza_for_detached_bound_resource so the queued
        // stanzas replay on resume. Without the detached pass, brief
        // disconnects silently lose carbon history.
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

        let registry = ConnectionRegistry::new();
        let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let alice_phone: jid::FullJid = "alice@example.com/phone".parse().expect("jid");

        // alice/web: live, originating resource (excluded).
        let (_web_tx, _web_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(alice_web.clone(), _web_tx, true);

        // alice/phone: detached, carbons-enabled, resumable via SM.
        let sm = Arc::new(InMemorySmSessionRegistry::new());
        let detached = DetachedSession {
            stream_id: "phone-stream-id".to_string(),
            user_id: "alice".to_string(),
            jid: alice_phone.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
        };
        sm.store_session(detached).await.expect("store session");

        let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
        let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let deps = Deps {
            connection_registry: &registry,
            sm_session_registry: Some(&sm),
            mam_storage: None,
            inbox_storage: None,
            extension_manager: None,
            room_registry: None,
            web_socket_state: None,
            authenticated_session: None,
            local_domain: "example.com",
            blocking_storage: None,
            message_dispatcher: None,
        };
        let _outcome = interpret(
            vec![OutboundEvent::SendCarbons {
                owner: owner.clone(),
                message: Box::new(original),
                kind: CarbonKind::Sent,
                exclude: alice_web,
            }],
            &deps,
        )
        .await;

        // The detached resource should have a queued carbon ready
        // for resume — peek the session and assert a non-empty
        // outbound replay queue.
        let session = sm
            .peek_session("phone-stream-id")
            .await
            .expect("peek")
            .expect("session present");
        assert!(
            !session.unacked_stanzas.is_empty(),
            "detached SM session must have at least one queued carbon for resume"
        );
    }

    // -----------------------------------------------------------------
    // XEP-0313 — ArchiveDirect persistence
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn xep_0313_archive_direct_persists_to_mam_storage() {
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let from: jid::BareJid = "alice@example.com".parse().expect("bare");
        let to: jid::BareJid = "bob@example.com".parse().expect("bare");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "hello");
        msg.id = Some("orig-1".to_string());

        let events = vec![OutboundEvent::ArchiveDirect {
            archive_jid: archive_jid.clone(),
            from,
            to,
            message: Box::new(msg),
        }];
        let _outcome = interpret(events, &deps).await;

        let stored = mam
            .query_messages(&archive_jid, &Default::default())
            .await
            .expect("query");
        assert_eq!(
            stored.messages.len(),
            1,
            "ArchiveDirect persists exactly one row"
        );
        let row = &stored.messages[0];
        assert_eq!(row.from.to_string(), "alice@example.com");
        assert_eq!(row.to.to_string(), "bob@example.com");
        assert_eq!(row.body.as_deref(), Some("hello"));
        assert_eq!(
            row.stanza_id.as_ref().map(|s| s.id.as_str()),
            Some("orig-1")
        );
    }

    #[tokio::test]
    async fn xep_0359_archive_ref_pivots_inbox_row_to_mam_row_via_archive_or_stanza_id() {
        // End-to-end of the bug Qodo + Codex flagged: inbox writes
        // `archive_ref` from the canonical XEP-0359 `<stanza-id>`
        // stamp, and `MamStorage::get_message_by_archive_or_stanza_id`
        // must resolve that same id against `archive_jid` by querying
        // both the archive's primary key (`id`) and the wire id
        // (`stanza_id`). If the projection ever stops using the
        // canonical stamp as `ArchivedMessage.id`, the inbox row
        // points at a dangling stanza-id and clients can't pivot to
        // the archive.
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::protocol::event::{StanzaIdRef, StanzaIdValue};
        use waddle_xmpp_core::xep0359::build_stanza_id_element;
        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
        let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
        let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "pivot test");
        msg.id = Some("wire-id".to_string());
        // Simulate CanonicalizeHandler stamping the canonical id
        // under alice's archive — the same id InboxHandler will
        // emit as `archive_ref`.
        let canonical_id = "alice-canonical-1";
        msg.payloads.push(build_stanza_id_element(
            canonical_id,
            &jid::Jid::from(alice.clone()),
        ));

        let events = vec![
            OutboundEvent::ArchiveDirect {
                archive_jid: alice.clone(),
                from: alice.clone(),
                to: bob.clone(),
                message: Box::new(msg.clone()),
            },
            OutboundEvent::ProjectInbox {
                owner: alice.clone(),
                peer: bob.clone(),
                message: Box::new(msg),
                archive_ref: StanzaIdRef {
                    by: alice.clone(),
                    id: StanzaIdValue::new(canonical_id),
                },
                increment_unread: false,
            },
        ];
        let _outcome = interpret(events, &deps).await;

        // Inbox row carries the canonical stamp.
        let entries = inbox_concrete.list(&alice).await.expect("inbox list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].last_stanza_id, canonical_id);

        // The same id resolves a MAM row in alice's archive — pivot
        // works. The XEP-0359 canonical stamp is stored as the row's
        // `id` (primary key) per the legacy projection shape, so the
        // pivot uses `get_message_by_archive_or_stanza_id` (queries
        // both `id` and `stanza_id`).
        let row = mam
            .get_message_by_archive_or_stanza_id(&alice, canonical_id)
            .await
            .expect("mam lookup")
            .expect("MAM row keyed by canonical stanza-id");
        assert_eq!(row.id, canonical_id);
        assert_eq!(row.body.as_deref(), Some("pivot test"));
    }

    #[tokio::test]
    async fn xep_0313_archive_direct_writes_one_entry_per_event() {
        // Sender pass + recipient pass on the same dispatch (true
        // local-to-local) emit two events with distinct archive_jids
        // — the interpreter writes one entry per archive.
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
        let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
        let msg = chat_msg("alice@example.com/web", "bob@example.com", "yo");

        let events = vec![
            OutboundEvent::ArchiveDirect {
                archive_jid: alice.clone(),
                from: alice.clone(),
                to: bob.clone(),
                message: Box::new(msg.clone()),
            },
            OutboundEvent::ArchiveDirect {
                archive_jid: bob.clone(),
                from: alice.clone(),
                to: bob.clone(),
                message: Box::new(msg),
            },
        ];
        let _outcome = interpret(events, &deps).await;

        let alice_archive = mam
            .query_messages(&alice, &Default::default())
            .await
            .expect("query alice");
        let bob_archive = mam
            .query_messages(&bob, &Default::default())
            .await
            .expect("query bob");
        assert_eq!(
            alice_archive.messages.len(),
            1,
            "alice archive has the sender-pass entry"
        );
        assert_eq!(
            bob_archive.messages.len(),
            1,
            "bob archive has the recipient-pass entry"
        );
    }

    #[tokio::test]
    async fn xep_0313_archive_direct_drops_when_storage_errors() {
        // Storage errors must NOT fail dispatch. We use a fake that
        // always errors and assert interpret returns normally; the
        // archive write is logged-and-dropped.
        use async_trait::async_trait;
        use waddle_xmpp::mam::storage::{MamStorage, MamStorageError};
        use waddle_xmpp::mam::{ArchivedMessage, MamQuery, MamResult};

        struct FailingMam;
        #[async_trait]
        impl MamStorage for FailingMam {
            async fn store_message(
                &self,
                _: &jid::BareJid,
                _: &ArchivedMessage,
            ) -> Result<String, MamStorageError> {
                Err(MamStorageError::Database("simulated".into()))
            }
            async fn query_messages(
                &self,
                _: &jid::BareJid,
                _: &MamQuery,
            ) -> Result<MamResult, MamStorageError> {
                Ok(MamResult {
                    messages: Vec::new(),
                    complete: true,
                    first_id: None,
                    last_id: None,
                    count: Some(0),
                })
            }
            async fn get_message(
                &self,
                _: &str,
            ) -> Result<Option<ArchivedMessage>, MamStorageError> {
                Ok(None)
            }
            async fn replace_with_tombstone(
                &self,
                _: &str,
                _: waddle_xmpp::mam::ArchivedTombstone,
            ) -> Result<bool, MamStorageError> {
                Ok(false)
            }
            async fn get_message_by_stanza_id(
                &self,
                _: &jid::BareJid,
                _: &str,
            ) -> Result<Option<ArchivedMessage>, MamStorageError> {
                Ok(None)
            }
            async fn get_message_by_message_id(
                &self,
                _: &jid::BareJid,
                _: &str,
            ) -> Result<Option<ArchivedMessage>, MamStorageError> {
                Ok(None)
            }
            async fn get_message_by_archive_or_stanza_id(
                &self,
                _: &jid::BareJid,
                _: &str,
            ) -> Result<Option<ArchivedMessage>, MamStorageError> {
                Ok(None)
            }
            async fn count_messages(&self, _: &jid::BareJid) -> Result<u32, MamStorageError> {
                Ok(0)
            }
            async fn delete_before(
                &self,
                _: &jid::BareJid,
                _: chrono::DateTime<chrono::Utc>,
            ) -> Result<u64, MamStorageError> {
                Ok(0)
            }
        }

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(FailingMam);
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
        let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
        let msg = chat_msg("alice@example.com/web", "bob@example.com", "yo");
        let events = vec![OutboundEvent::ArchiveDirect {
            archive_jid: alice.clone(),
            from: alice,
            to: bob,
            message: Box::new(msg),
        }];
        let outcome = interpret(events, &deps).await;
        // No frames, no close — error swallowed.
        assert!(outcome.frames.is_empty());
        assert!(!outcome.close);
    }

    // -----------------------------------------------------------------
    // Inbox projection
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn inbox_project_writes_owner_peer_keyed_row_with_typed_archive_ref() {
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::protocol::event::{StanzaIdRef, StanzaIdValue};

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
        let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
        let peer: jid::BareJid = "bob@example.com".parse().expect("bare");
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "hi there");
        msg.id = Some("origin-X".to_string());

        let events = vec![OutboundEvent::ProjectInbox {
            owner: owner.clone(),
            peer: peer.clone(),
            message: Box::new(msg),
            archive_ref: StanzaIdRef {
                by: owner.clone(),
                id: StanzaIdValue::new("alice-archive-1"),
            },
            increment_unread: false,
        }];
        let _outcome = interpret(events, &deps).await;

        let entries = inbox_concrete.list(&owner).await.expect("list");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.partner, peer);
        assert_eq!(
            entry.last_stanza_id, "alice-archive-1",
            "last_stanza_id is sourced from the typed archive_ref, not the wire id"
        );
        assert_eq!(entry.unread, 0, "increment_unread=false leaves unread at 0");
    }

    #[tokio::test]
    async fn inbox_project_increment_unread_bumps_recipient_count() {
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::protocol::event::{StanzaIdRef, StanzaIdValue};

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
        let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let owner: jid::BareJid = "bob@example.com".parse().expect("bare");
        let peer: jid::BareJid = "alice@example.com".parse().expect("bare");
        let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi bob");

        let events = vec![OutboundEvent::ProjectInbox {
            owner: owner.clone(),
            peer: peer.clone(),
            message: Box::new(msg),
            archive_ref: StanzaIdRef {
                by: owner.clone(),
                id: StanzaIdValue::new("bob-archive-1"),
            },
            increment_unread: true,
        }];
        let _outcome = interpret(events, &deps).await;

        let total = inbox_concrete.total_unread(&owner).await.expect("unread");
        assert_eq!(
            total, 1,
            "increment_unread=true bumps the owner's unread count"
        );
    }

    // -----------------------------------------------------------------
    // XEP-0308/0424/0461 — LookupArchivedMessage callback round-trip
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn xep_0424_lookup_archived_message_by_stanza_id_feeds_archived_loaded_back() {
        use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
        use waddle_xmpp::protocol::event::CallbackId;
        use waddle_xmpp::protocol::event::StanzaIdValue;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        // Seed the archive with a row keyed under alice's bare,
        // canonical stamp = "canon-A1".
        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let row = ArchivedMessage {
            id: "canon-A1".to_string(),
            timestamp: chrono::Utc::now(),
            from: "alice@example.com".parse().expect("jid"),
            to: "bob@example.com".parse().expect("jid"),
            body: Some("hello".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "canon-A1",
                jid::Jid::from(archive_jid.clone()),
            )),
            thread: None,
            reply: None,
            origin_id: None,
            message_type: XmppMessageType::Chat,
            stanza_xml: Some(
                r#"<message xmlns='jabber:client' type='chat' from='alice@example.com/web' to='bob@example.com'><body>hello</body></message>"#.to_string(),
            ),
            rich: None,
            nickname_generation: None,
        };
        mam.store_message(&archive_jid, &row).await.expect("seed");

        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(7),
            archive: archive_jid.clone(),
            reference: MessageRef::StanzaId {
                by: archive_jid.clone(),
                id: StanzaIdValue::new("canon-A1"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        assert_eq!(outcome.feedback.len(), 1);
        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded { id, result } => {
                assert_eq!(id, CallbackId(7));
                let archived = result.expect("row resolved");
                assert_eq!(archived.stanza_id.id.as_str(), "canon-A1");
                assert_eq!(archived.stanza_id.by, archive_jid);
                assert!(!archived.tombstoned);
                assert_eq!(
                    archived.message.bodies.get("").map(|b| b.0.clone()),
                    Some("hello".to_string()),
                    "stanza_xml is parsed back into a typed Message"
                );
            }
            other => panic!("expected ArchivedMessageLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn xep_0424_lookup_archived_message_not_found_feeds_none_back() {
        use waddle_xmpp::mam::InMemoryMamStorage;
        use waddle_xmpp::protocol::event::CallbackId;
        use waddle_xmpp::protocol::event::StanzaIdValue;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(11),
            archive: archive_jid.clone(),
            reference: MessageRef::StanzaId {
                by: archive_jid,
                id: StanzaIdValue::new("never-stamped"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded {
                id: CallbackId(11),
                result: None,
            } => {}
            other => panic!("expected ArchivedMessageLoaded(None), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn xep_0359_lookup_archived_message_by_origin_id_feeds_archived_loaded_back() {
        // OriginId lookup MUST be sender-scoped per the typed
        // `MessageRef::OriginId { sender, origin_id }` contract.
        // Seed two rows in alice's archive that share the same
        // `origin_id` value but come from different senders:
        // post-filter on `sender` must pick the alice-authored row,
        // not the bob-authored one.
        use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
        use waddle_xmpp::protocol::event::{CallbackId, OriginIdValue};

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let alice_bare: jid::BareJid = "alice@example.com".parse().expect("bare");

        // Bob-authored row in alice's archive (cross-resource self
        // chat / received DM) sharing the colliding origin-id.
        let bob_row = ArchivedMessage {
            id: "row-from-bob".to_string(),
            timestamp: chrono::Utc::now(),
            from: "bob@example.com".parse().expect("jid"),
            to: "alice@example.com".parse().expect("jid"),
            body: Some("from bob".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "alice-stamp-bob",
                jid::Jid::from(archive_jid.clone()),
            )),
            thread: None,
            reply: None,
            origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("collision")),
            message_type: XmppMessageType::Chat,
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
        };
        // Alice-authored row in alice's archive (sender-side) with
        // the same origin-id.
        let alice_row = ArchivedMessage {
            id: "row-from-alice".to_string(),
            timestamp: chrono::Utc::now(),
            from: "alice@example.com".parse().expect("jid"),
            to: "bob@example.com".parse().expect("jid"),
            body: Some("from alice".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "alice-stamp-alice",
                jid::Jid::from(archive_jid.clone()),
            )),
            thread: None,
            reply: None,
            origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("collision")),
            message_type: XmppMessageType::Chat,
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
        };
        // Insert bob's row FIRST so a naive OR-matcher would return it.
        mam.store_message(&archive_jid, &bob_row)
            .await
            .expect("seed bob");
        mam.store_message(&archive_jid, &alice_row)
            .await
            .expect("seed alice");

        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(21),
            archive: archive_jid.clone(),
            reference: MessageRef::OriginId {
                sender: alice_bare.clone(),
                origin_id: OriginIdValue::new("collision"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded {
                id: CallbackId(21),
                result: Some(archived),
            } => {
                let body = archived
                    .message
                    .bodies
                    .get("")
                    .map(|b| b.0.clone())
                    .unwrap_or_default();
                assert_eq!(
                    body, "from alice",
                    "OriginId lookup must scope to sender; bob's row was a collision decoy"
                );
            }
            other => panic!("expected alice-authored row, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn xep_0359_lookup_archived_message_by_origin_id_rejects_cross_sender_collision() {
        // Same archive, same origin_id, different sender than
        // requested -> result MUST be None (handler will treat as
        // <item-not-found>).
        use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
        use waddle_xmpp::protocol::event::{CallbackId, OriginIdValue};

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let row = ArchivedMessage {
            id: "row-1".to_string(),
            timestamp: chrono::Utc::now(),
            from: "bob@example.com".parse().expect("jid"),
            to: "alice@example.com".parse().expect("jid"),
            body: Some("bob's".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "alice-stamp",
                jid::Jid::from(archive_jid.clone()),
            )),
            thread: None,
            reply: None,
            origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("oid-1")),
            message_type: XmppMessageType::Chat,
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
        };
        mam.store_message(&archive_jid, &row).await.expect("seed");

        // Look up for a DIFFERENT sender (charlie) with the colliding
        // origin-id. Must surface as not-found.
        let charlie_bare: jid::BareJid = "charlie@example.com".parse().expect("bare");
        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(31),
            archive: archive_jid,
            reference: MessageRef::OriginId {
                sender: charlie_bare,
                origin_id: OriginIdValue::new("oid-1"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded {
                id: CallbackId(31),
                result: None,
            } => {}
            other => {
                panic!("OriginId lookup must reject cross-sender collisions, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn xep_0359_lookup_archived_message_strict_stanza_id_ignores_origin_id_collision() {
        // StanzaId path uses `get_message_by_message_id` (stanza_id
        // ONLY), so a row whose `origin_id` happens to equal the
        // requested stanza-id MUST NOT be returned.
        use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
        use waddle_xmpp::protocol::event::{CallbackId, StanzaIdValue};

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        // Row whose origin_id equals the value the caller is looking
        // up via StanzaId. Stanza_id is something else.
        let collision_row = ArchivedMessage {
            id: "row-collide".to_string(),
            timestamp: chrono::Utc::now(),
            from: "alice@example.com".parse().expect("jid"),
            to: "bob@example.com".parse().expect("jid"),
            body: Some("collide".to_string()),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "real-stamp",
                jid::Jid::from(archive_jid.clone()),
            )),
            thread: None,
            reply: None,
            origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("queried-id")),
            message_type: XmppMessageType::Chat,
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
        };
        mam.store_message(&archive_jid, &collision_row)
            .await
            .expect("seed");

        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(41),
            archive: archive_jid.clone(),
            reference: MessageRef::StanzaId {
                by: archive_jid,
                id: StanzaIdValue::new("queried-id"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded {
                id: CallbackId(41),
                result: None,
            } => {}
            other => {
                panic!("strict stanza-id lookup must ignore origin-id collisions, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn xep_0424_lookup_archived_message_propagates_tombstone_state() {
        use waddle_xmpp::mam::{
            ArchivedMessage, ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone,
            InMemoryMamStorage, RichMessageId,
        };
        use waddle_xmpp::protocol::event::CallbackId;
        use waddle_xmpp::protocol::event::StanzaIdValue;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> =
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        let deps = Deps::test_with_storage(&registry, &mam, &inbox);

        let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
        let row = ArchivedMessage {
            id: "tomb-1".to_string(),
            timestamp: chrono::Utc::now(),
            from: "alice@example.com".parse().expect("jid"),
            to: "bob@example.com".parse().expect("jid"),
            body: None,
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                "tomb-1",
                jid::Jid::from(archive_jid.clone()),
            )),
            thread: None,
            reply: None,
            origin_id: None,
            message_type: XmppMessageType::Chat,
            stanza_xml: None,
            rich: Some(ArchivedRichMessage {
                payload: Some(ArchivedRichPayload::Tombstone(ArchivedTombstone {
                    retraction_id: Some(RichMessageId::new("retract-1").expect("rich id")),
                    stamp: chrono::Utc::now(),
                    moderation: None,
                })),
                reply: None,
                references: Vec::new(),
                mentions: Vec::new(),
            }),
            nickname_generation: None,
        };
        mam.store_message(&archive_jid, &row).await.expect("seed");

        let events = vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(13),
            archive: archive_jid.clone(),
            reference: MessageRef::StanzaId {
                by: archive_jid,
                id: StanzaIdValue::new("tomb-1"),
            },
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::ArchivedMessageLoaded {
                result: Some(archived),
                ..
            } => {
                assert!(
                    archived.tombstoned,
                    "ArchivedRichPayload::Tombstone surfaces as `tombstoned: true`"
                );
            }
            other => panic!("expected Some archived row, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // XEP-0372 — RequestEnrichment callback round-trip
    // -----------------------------------------------------------------

    #[test]
    fn extension_waddle_scope_matches_managed_room_context() {
        let managed_room: BareJid = "general@muc.example.com".parse().expect("room jid");
        assert_eq!(waddle_id_for_room_jid(&managed_room).as_str(), "space");

        let unmanaged_room: BareJid = "conference.example.com".parse().expect("room jid");
        assert_eq!(waddle_id_for_room_jid(&unmanaged_room).as_str(), "default");
    }

    #[tokio::test]
    async fn enrichment_request_without_extension_manager_fails_open_with_original_message() {
        // No extension manager in Deps -> the original typed message
        // is returned unchanged via EnrichmentComplete. This is the
        // legacy fail-open contract (see `enrich_message` in the
        // legacy `message.rs` path).
        use waddle_xmpp::protocol::event::CallbackId;
        let registry = ConnectionRegistry::new();
        let deps = Deps::registry_only(&registry);

        let mut original = chat_msg("alice@example.com/web", "bob@example.com", "look https://x");
        original.id = Some("orig-id".to_string());

        let events = vec![OutboundEvent::RequestEnrichment {
            id: CallbackId(42),
            message: Box::new(original.clone()),
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::EnrichmentComplete {
                id: CallbackId(42),
                message,
            } => {
                assert_eq!(message.id, original.id);
                assert_eq!(
                    message.bodies.get("").map(|b| b.0.clone()),
                    Some("look https://x".to_string()),
                );
            }
            other => panic!("expected EnrichmentComplete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn enrichment_failure_fail_open_feeds_original_message_back() {
        // Fail-open contract: when the extension manager has no
        // working actors (e.g. all extension RPCs failed at startup,
        // or the deployment intentionally disabled extensions),
        // `enrich_message` is a no-op and the dispatch must still
        // resume with the *original* message via EnrichmentComplete
        // — never block on enrichment, never drop the message.
        // We model this with a disabled config (no actors loaded),
        // which is the exact failure mode legacy `message.rs` falls
        // back to when the wasm runtime can't start any extension.
        use waddle_extensions::{ExtensionConfig, ExtensionManager};
        use waddle_xmpp::protocol::event::CallbackId;

        let registry = ConnectionRegistry::new();
        let em = Arc::new(
            ExtensionManager::from_config(ExtensionConfig {
                enabled: false,
                ..Default::default()
            })
            .await
            .expect("disabled extension manager"),
        );
        let deps = Deps::test_with_extension_manager(&registry, &em);

        let mut original = chat_msg(
            "alice@example.com/web",
            "bob@example.com",
            "check https://example.com",
        );
        original.id = Some("fail-open-id".to_string());
        let original_payload_count = original.payloads.len();

        let events = vec![OutboundEvent::RequestEnrichment {
            id: CallbackId(123),
            message: Box::new(original.clone()),
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::EnrichmentComplete {
                id: CallbackId(123),
                message,
            } => {
                assert_eq!(
                    message.id.as_deref(),
                    Some("fail-open-id"),
                    "fail-open path returns the original message id"
                );
                assert_eq!(
                    message.bodies.get("").map(|b| b.0.clone()),
                    original.bodies.get("").map(|b| b.0.clone()),
                    "fail-open path returns the original body unchanged"
                );
                assert_eq!(
                    message.payloads.len(),
                    original_payload_count,
                    "fail-open path adds no payloads when no actor produces enrichment"
                );
            }
            other => panic!("expected EnrichmentComplete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn enrichment_request_calls_extension_manager_and_feeds_complete_back() {
        // Wire a real (empty) ExtensionManager — no extension actors
        // configured, so `enrich_message` returns 0 enrichments and
        // we still feed back the original message via
        // EnrichmentComplete with the original CallbackId. This proves
        // the callback round-trip without depending on a live wasm
        // runtime.
        use waddle_extensions::{ExtensionConfig, ExtensionManager};
        use waddle_xmpp::protocol::event::CallbackId;

        let registry = ConnectionRegistry::new();
        let em = Arc::new(
            ExtensionManager::from_config(ExtensionConfig {
                enabled: false,
                ..Default::default()
            })
            .await
            .expect("disabled extension manager"),
        );
        let deps = Deps::test_with_extension_manager(&registry, &em);

        let mut original = chat_msg("alice@example.com/web", "bob@example.com", "ping");
        original.id = Some("e-id".to_string());

        let events = vec![OutboundEvent::RequestEnrichment {
            id: CallbackId(99),
            message: Box::new(original),
        }];
        let outcome = interpret(events, &deps).await;

        match outcome.feedback.into_iter().next().expect("feedback") {
            InboundEvent::EnrichmentComplete {
                id: CallbackId(99),
                message,
            } => {
                assert_eq!(message.id.as_deref(), Some("e-id"));
            }
            other => panic!("expected EnrichmentComplete, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // #229 PR12 — RouteToConnection delivers as PeerStanza
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn route_to_connection_full_jid_queues_peer_stanza_kind() {
        // Locks in the staged-cutover contract: full-JID
        // RouteToConnection events queue an OutboundStanza tagged
        // PeerStanza so the destination's main loop runs the
        // recipient pass before any wire write.
        use waddle_xmpp::registry::DeliveryKind;
        let registry = ConnectionRegistry::new();
        let bob: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(bob.clone(), bob_tx, false);

        let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(bob.clone()),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        let queued = drain_inbound(&mut bob_rx);
        assert_eq!(queued.len(), 1, "delivered to bob's queue exactly once");
        assert_eq!(
            queued[0].kind,
            DeliveryKind::PeerStanza,
            "RouteToConnection MUST tag PeerStanza so the destination main \
             loop runs the recipient pass; got {:?}",
            queued[0].kind
        );
    }

    #[tokio::test]
    async fn route_to_connection_bare_jid_selects_highest_priority_available_resources() {
        // RFC 6121 §8.5.2.1 resource selection: deliver to every
        // resource tied at the highest available priority. A
        // bare-JID `to` from the sender pass (handlers/route.rs
        // emits `message.to` verbatim) lands here; without selection
        // the cutover would silently drop bare-targeted 1:1 traffic.
        use waddle_xmpp::registry::DeliveryKind;
        let registry = ConnectionRegistry::new();
        let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
        let bob_tablet: jid::FullJid = "bob@example.com/tablet".parse().expect("jid");
        let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
        let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
        let (tablet_tx, mut tablet_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(bob_desk.clone(), desk_tx, false);
        registry.register_with_carbons(bob_phone.clone(), phone_tx, false);
        registry.register_with_carbons(bob_tablet.clone(), tablet_tx, false);
        // desk + phone available at priority 5 (tied); tablet at
        // lower priority 1. Tablet must NOT receive.
        registry.update_presence(&bob_desk, true, 5);
        registry.update_presence(&bob_phone, true, 5);
        registry.update_presence(&bob_tablet, true, 1);

        let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi bare");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        let desk_q = drain_inbound(&mut desk_rx);
        let phone_q = drain_inbound(&mut phone_rx);
        let tablet_q = drain_inbound(&mut tablet_rx);
        assert_eq!(
            desk_q.len(),
            1,
            "desk (tied at max priority) gets the message"
        );
        assert_eq!(
            phone_q.len(),
            1,
            "phone (tied at max priority) gets the message"
        );
        assert!(
            tablet_q.is_empty(),
            "tablet (lower priority) is excluded by RFC 6121 §8.5.2.1.2"
        );
        for q in [&desk_q, &phone_q] {
            assert_eq!(q[0].kind, DeliveryKind::PeerStanza);
        }
    }

    #[tokio::test]
    async fn route_to_connection_bare_jid_falls_back_to_connected_resources_without_presence() {
        // RFC 6121 §8.5.2.1.1 prefers presence-available resources
        // for bare-JID delivery, but Waddle falls back to *any*
        // connected resource when no resource has emitted
        // `<presence/>` yet (matching legacy `handle_message`
        // behaviour and unblocking integration tests where clients
        // bind without sending presence). This test pins that
        // fall-back: a bare-JID DM addressed to a user with one
        // registered-but-not-presence-available resource is delivered
        // to that resource instead of falling through to the offline
        // headless pass.
        let registry = ConnectionRegistry::new();
        let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
        let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
        registry.register_with_carbons(bob_desk.clone(), desk_tx, false);
        // Registered but presence NOT made available — legacy
        // routing still delivers to this resource.

        let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

        let delivered = drain_inbound(&mut desk_rx);
        assert_eq!(
            delivered.len(),
            1,
            "no presence -> still delivered to connected resource as a legacy fallback"
        );
    }

    #[tokio::test]
    async fn preserves_frame_order_across_multiple_events() {
        let events = vec![
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq("a")))),
            OutboundEvent::Log {
                level: tracing::Level::DEBUG,
                message: "between".to_string(),
            },
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq("b")))),
        ];
        let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
        assert_eq!(outcome.frames.len(), 2);
        assert!(outcome.frames[0].contains("id=\"a\""));
        assert!(outcome.frames[1].contains("id=\"b\""));
    }

    #[tokio::test]
    async fn send_stanza_preserves_xep_0201_thread_on_wire() {
        let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "threaded hi");
        msg.thread = Some(xmpp_parsers::message::Thread("root-thread".to_string()));

        let events = vec![OutboundEvent::SendStanza(Box::new(Stanza::Message(msg)))];
        let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;

        assert_eq!(outcome.frames.len(), 1);
        assert!(
            outcome.frames[0].contains("<thread>root-thread</thread>"),
            "SendStanza must preserve RFC 6121/XEP-0201 thread on the wire: {}",
            outcome.frames[0]
        );
    }

    // -----------------------------------------------------------------
    // #229 PR18 — DispatchToRoom interpreter arm runs the room handler
    // chain (Q7 option C). The end-to-end semantics (managed-room owner
    // check, rich-target validation, MAM archive, retraction
    // tombstones, per-occupant inbox projection, occupant fan-out) are
    // exercised by the integration tests in
    // `crates/waddle-server/tests/*_ws.rs`; the L1 unit test below pins
    // the chain wiring against the lightweight in-process `Deps` shape.
    // -----------------------------------------------------------------

    /// Without `web_socket_state` the arm logs a warn and drops the
    /// event without panicking — production must wire `web_socket_state`
    /// via [`super::super::websocket::build_interpret_deps`].
    #[tokio::test]
    async fn dispatch_to_room_drops_when_no_web_socket_state_in_deps() {
        let registry = ConnectionRegistry::new();
        let room_jid: jid::BareJid = "testroom@muc.example.com".parse().expect("parse room jid");
        let mut message =
            xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
        message.type_ = xmpp_parsers::message::MessageType::Groupchat;
        message.from = Some(
            "alice@example.com/web"
                .parse::<jid::FullJid>()
                .map(jid::Jid::from)
                .expect("from"),
        );

        let events = vec![OutboundEvent::DispatchToRoom {
            room: room_jid,
            message: Box::new(message),
        }];
        let outcome = interpret(events, &Deps::registry_only(&registry)).await;
        assert!(outcome.frames.is_empty());
        assert!(!outcome.close);
    }

    #[tokio::test]
    async fn extension_room_message_dispatches_threaded_muc_message() {
        use waddle_extensions::{
            DisplayText, FullJidValue, ReplyTarget, RoomJid, StanzaId, ThreadId,
        };

        let registry = ConnectionRegistry::new();
        let room_jid: jid::BareJid = "chat@muc.example.com".parse().expect("room jid");
        let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
        let bob: jid::FullJid = "bob@example.com/web".parse().expect("bob jid");
        let bot: jid::FullJid = "chat@example.com/bot".parse().expect("bot jid");
        let (alice_tx, mut alice_rx) = tokio::sync::mpsc::channel(8);
        let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
        registry.register(alice.clone(), alice_tx);
        registry.register(bob.clone(), bob_tx);

        let occupants = vec![
            OccupantSnapshot {
                full_jid: alice.clone(),
                nick: "alice".to_string(),
                affiliation: waddle_xmpp::Affiliation::Member,
                role: waddle_xmpp::Role::Participant,
            },
            OccupantSnapshot {
                full_jid: bob.clone(),
                nick: "bob".to_string(),
                affiliation: waddle_xmpp::Affiliation::Member,
                role: waddle_xmpp::Role::Participant,
            },
            OccupantSnapshot {
                full_jid: bot.clone(),
                nick: "waddle".to_string(),
                affiliation: waddle_xmpp::Affiliation::Member,
                role: waddle_xmpp::Role::Participant,
            },
        ];
        let response = ExtensionRoomMessage {
            body: DisplayText::new("bot answer").expect("body"),
            room: RoomJid::new(room_jid.to_string()).expect("room"),
            stanza_id: None,
            thread_id: Some(ThreadId::new("root-msg").expect("thread")),
            reply_to: Some(ReplyTarget {
                id: StanzaId::new("root-msg").expect("reply id"),
                to: Some(FullJidValue::new(alice.to_string()).expect("reply to")),
            }),
            extensions: None,
        };

        let test_secret = waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("test secret meets length floor");
        let outcome = dispatch_bot_groupchat_response(
            &Deps::registry_only(&registry),
            BotGroupchatDispatch {
                room_jid: &room_jid,
                occupants: &occupants,
                sender_full: &bot,
                room_actor: None,
                room_moderated: false,
                dispatch_timestamp: 1777629203,
                recursion_depth: 0,
                occupant_id_secret: &test_secret,
            },
            response,
        )
        .await;
        let outcome = outcome.expect("bot dispatch should succeed").outcome;

        assert!(outcome.frames.is_empty());
        assert!(!outcome.close);

        let alice_delivered = drain_inbound(&mut alice_rx);
        let bob_delivered = drain_inbound(&mut bob_rx);
        assert_eq!(alice_delivered.len(), 1);
        assert_eq!(bob_delivered.len(), 1);

        let Stanza::Message(message) = &alice_delivered[0].stanza else {
            panic!("expected bot groupchat message");
        };
        assert_eq!(message.type_, xmpp_parsers::message::MessageType::Groupchat);
        assert_eq!(
            message.from.as_ref().map(ToString::to_string),
            Some(format!("{room_jid}/waddle"))
        );
        assert_eq!(
            message.thread.as_ref().map(|thread| thread.0.as_str()),
            Some("root-msg")
        );
        assert_eq!(
            message.bodies.get("").map(|body| body.0.as_str()),
            Some("bot answer")
        );
        let reply = parse_reply_from_message(message).expect("reply payload");
        assert_eq!(reply.id, "root-msg");
        assert_eq!(reply.to, None);
        assert!(
            !message
                .payloads
                .iter()
                .any(|payload| payload.ns() == "urn:waddle:forums:0"),
            "plain MUC bot responses must not reuse forum metadata"
        );
    }

    #[test]
    fn groupchat_reply_to_attr_only_preserves_room_occupant_jids() {
        let room: BareJid = "chat@muc.example.com".parse().expect("room");

        assert_eq!(
            room_scoped_reply_to_attr("chat@muc.example.com/alice", &room),
            Some(
                "chat@muc.example.com/alice"
                    .parse::<Jid>()
                    .expect("occupant jid")
            )
        );
        assert_eq!(
            room_scoped_reply_to_attr("alice@example.com/web", &room),
            None
        );
        assert_eq!(room_scoped_reply_to_attr("not a jid", &room), None);
    }

    #[test]
    fn message_thread_id_reads_existing_forum_reply_without_rfc_thread() {
        let xml = r#"<message xmlns='jabber:client' id='child'>
            <thread-reply xmlns='urn:waddle:forums:0' thread-id='root-msg'/>
        </message>"#;
        let element: Element = xml.parse().expect("element");
        let message = Message::try_from(element).expect("message");
        assert_eq!(message_thread_id(&message).as_deref(), Some("root-msg"));
    }

    #[test]
    fn thread_create_source_is_normalized_for_inbox_projection() {
        let mut message = Message::new(Some(Jid::from(
            "chat@muc.example.com"
                .parse::<jid::BareJid>()
                .expect("room jid"),
        )));
        message.id = Some("live-forum-root".to_string());
        message.type_ = xmpp_parsers::message::MessageType::Groupchat;
        set_thread_create(&mut message, &ThreadCreate::new("Live forum root"));

        let thread_id = normalize_thread_create_source(&mut message);

        assert_eq!(thread_id.as_deref(), Some("live-forum-root"));
        assert_eq!(
            message.thread.as_ref().map(|thread| thread.0.as_str()),
            Some("live-forum-root")
        );
        assert!(matches!(
            extract_forum_action(&message),
            Some(ForumAction::CreateThread(_))
        ));
    }

    #[test]
    fn bot_nick_avoids_existing_occupant_collision() {
        let occupants = vec![
            OccupantSnapshot {
                full_jid: "alice@example.com/web".parse().expect("alice jid"),
                nick: "waddle".to_string(),
                affiliation: waddle_xmpp::Affiliation::Member,
                role: waddle_xmpp::Role::Participant,
            },
            OccupantSnapshot {
                full_jid: "bob@example.com/web".parse().expect("bob jid"),
                nick: "waddle-2".to_string(),
                affiliation: waddle_xmpp::Affiliation::Member,
                role: waddle_xmpp::Role::Participant,
            },
        ];

        assert_eq!(available_bot_nick(&occupants), "waddle-3");
    }

    // -----------------------------------------------------------------
    // #229 PR15 — headless offline-recipient pass
    // -----------------------------------------------------------------
    //
    // When `RouteToConnection` lands a bare-JID at a local user with no
    // available resources, the interpreter constructs a transient
    // `XmppStateMachine` for the recipient (loaded blocklist), feeds
    // `StanzaFromPeer`, and recursively interprets the resulting events
    // with a recursion depth cap. Persists archive + inbox + incoming
    // blocking; drops `RouteToConnection`/`SendStanza`/`SendCarbons`
    // from the headless pass.

    /// Build a `Deps` configured for offline-recipient-pass tests:
    /// real dispatcher with the message handler chain registered, real
    /// MAM + inbox storage, blocklist storage seeded by the caller.
    fn offline_pass_deps<'a>(
        registry: &'a ConnectionRegistry,
        mam: &'a Arc<dyn MamStorage>,
        inbox: &'a Arc<dyn InboxStorage>,
        blocking: &'a Arc<dyn BlockingStorage>,
        dispatcher: &'a Arc<StanzaDispatcher>,
    ) -> Deps<'a> {
        Deps {
            connection_registry: registry,
            sm_session_registry: None,
            mam_storage: Some(mam),
            inbox_storage: Some(inbox),
            extension_manager: None,
            room_registry: None,
            web_socket_state: None,
            authenticated_session: None,
            local_domain: "example.com",
            blocking_storage: Some(blocking),
            message_dispatcher: Some(dispatcher),
        }
    }

    fn pipelined_dispatcher() -> Arc<StanzaDispatcher> {
        let mut d = StanzaDispatcher::new();
        waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut d);
        Arc::new(d)
    }

    #[tokio::test]
    async fn offline_recipient_pass_persists_archive_for_bare_jid_target() {
        // Sender pass already wrote alice's archive entry; the offline
        // recipient pass must additionally write bob's archive entry
        // because bob is local but has no available resources.
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
        let dispatcher = pipelined_dispatcher();
        let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

        // alice -> bob bare; no resources for bob registered.
        let msg = chat_msg("alice@example.com/web", "bob@example.com", "hello bob");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let _ = interpret(events, &deps).await;

        let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
        let bob_archive = mam
            .query_messages(&bob_bare, &Default::default())
            .await
            .expect("query bob");
        assert_eq!(
            bob_archive.messages.len(),
            1,
            "headless recipient pass writes one archive entry under bob's bare"
        );
        assert_eq!(bob_archive.messages[0].body.as_deref(), Some("hello bob"));
    }

    #[tokio::test]
    async fn offline_recipient_pass_persists_inbox_for_bare_jid_target() {
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
        let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
        let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
        let dispatcher = pipelined_dispatcher();
        let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

        let msg = chat_msg("alice@example.com/web", "bob@example.com", "inbox row?");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let _ = interpret(events, &deps).await;

        let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
        let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
        let entries = inbox_concrete.list(&bob).await.expect("list");
        assert_eq!(
            entries.len(),
            1,
            "headless pass projects one inbox row keyed under bob"
        );
        assert_eq!(
            entries[0].partner, alice,
            "inbox row pairs (owner=bob, peer=alice)"
        );
    }

    #[tokio::test]
    async fn route_to_connection_at_max_recursion_depth_drops_without_persistence() {
        // Direct unit test of the Codex-P1 recursion guard.
        // Calling `interpret_with_depth(...)` at
        // `MAX_RECIPIENT_PASS_DEPTH` simulates the inner-pass entry — a
        // `RouteToConnection` emitted from inside an in-flight headless
        // pass. The guard MUST short-circuit the entire arm (whether
        // the bare-JID has live targets or not), so no headless pass
        // runs and no recipient archive / inbox row is written.
        //
        // This pins the guard against regressions: removing or
        // weakening the depth check would let nested
        // `RouteToConnection` re-enter and cause duplicate persistence
        // in production. The test does not depend on which event the
        // transient SM's recipient pass actually emits.
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
        let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
        let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
        let dispatcher = pipelined_dispatcher();
        let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

        let msg = chat_msg("alice@example.com/web", "bob@example.com", "guard");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let outcome = interpret_with_depth(events, &deps, MAX_RECIPIENT_PASS_DEPTH).await;

        let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
        let bob_archive = mam
            .query_messages(&bob, &Default::default())
            .await
            .expect("query bob");
        assert!(
            bob_archive.messages.is_empty(),
            "recursion guard at MAX_RECIPIENT_PASS_DEPTH prevents the headless \
             pass from running — bob's archive must remain empty"
        );
        let entries = inbox_concrete.list(&bob).await.expect("list");
        assert!(
            entries.is_empty(),
            "recursion guard prevents inbox projection at max depth"
        );
        assert!(
            outcome.frames.is_empty(),
            "recursion guard drops the route entirely — no frames produced"
        );
    }

    #[tokio::test]
    async fn offline_recipient_pass_drops_send_stanza_no_wire() {
        // The transient SM emits `SendStanza` at the end of the
        // recipient pass (it's the wire-write effect for a live
        // connection). Without a live wire, those frames must not
        // bubble out into the *outer* `InterpretOutcome.frames`.
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
        let dispatcher = pipelined_dispatcher();
        let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

        let msg = chat_msg("alice@example.com/web", "bob@example.com", "drop wire");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let outcome = interpret(events, &deps).await;

        assert!(
            outcome.frames.is_empty(),
            "headless pass discards inner SendStanza frames; outer outcome stays empty"
        );
        assert!(
            outcome.feedback.is_empty(),
            "headless pass discards inner feedback events"
        );
        assert!(!outcome.close, "headless pass does not propagate close");
    }

    #[tokio::test]
    async fn offline_recipient_pass_blocklist_loaded_from_storage_blocks_filtered_message() {
        // BlockingFilterHandler runs first in the recipient pass.
        // With alice on bob's blocklist, the message must be HALTed
        // before reaching ArchiveHandler — bob's archive stays empty.
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        let blocking_concrete = Arc::new(InMemoryBlockingStorage::new());
        let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
        let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
        blocking_concrete.set_blocklist(bob.clone(), vec![alice.clone()]);
        let blocking: Arc<dyn BlockingStorage> = blocking_concrete.clone();
        let dispatcher = pipelined_dispatcher();
        let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

        let msg = chat_msg("alice@example.com/web", "bob@example.com", "blocked");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(bob.clone()),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let _ = interpret(events, &deps).await;

        let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
        let bob_archive = mam
            .query_messages(&bob_bare, &Default::default())
            .await
            .expect("query bob");
        assert!(
            bob_archive.messages.is_empty(),
            "BlockingFilterHandler halts the headless pass before ArchiveHandler — \
             no archive entry written for a blocked sender"
        );
    }

    #[tokio::test]
    async fn offline_recipient_pass_blocklist_storage_error_skips_recipient_persistence() {
        // Fail-closed semantic (Copilot review on PR #275): when the
        // blocklist storage errors, the helper MUST skip the recipient
        // pass entirely — no archive, no inbox row — to preserve
        // XEP-0191 incoming-block enforcement. Mirrors PR13's bind-time
        // policy where a blocklist load error fails the bind.
        // Degrading to `Blocklist::empty()` would silently allow blocked
        // senders into the recipient's MAM / inbox.
        use async_trait::async_trait;
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};

        #[derive(Debug, thiserror::Error)]
        #[error("simulated blocking storage failure")]
        struct SimulatedFailure;

        struct FailingBlocking;
        #[async_trait]
        impl BlockingStorage for FailingBlocking {
            async fn list_blocked_jids(
                &self,
                _: &jid::BareJid,
            ) -> Result<Vec<jid::BareJid>, BlockingStorageError> {
                Err(BlockingStorageError::new(SimulatedFailure))
            }
        }

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
        let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
        let blocking: Arc<dyn BlockingStorage> = Arc::new(FailingBlocking);
        let dispatcher = pipelined_dispatcher();
        let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

        let msg = chat_msg("alice@example.com/web", "bob@example.com", "fail-closed");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let _ = interpret(events, &deps).await;

        let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
        let bob_archive = mam
            .query_messages(&bob, &Default::default())
            .await
            .expect("query bob");
        assert!(
            bob_archive.messages.is_empty(),
            "blocklist load error fails closed — recipient archive NOT written"
        );
        let entries = inbox_concrete.list(&bob).await.expect("list");
        assert!(
            entries.is_empty(),
            "blocklist load error fails closed — recipient inbox NOT written"
        );
    }

    #[tokio::test]
    async fn xep_0359_offline_recipient_pass_emits_recipient_archive_with_recipient_stanza_id() {
        // L4 wire-trace integration: drive alice's *live* sender pass
        // through the dispatcher chain, then take alice's
        // RouteToConnection event and feed it into the interpreter.
        // The headless offline-recipient pass should write bob's
        // archive entry stamped `<stanza-id by='bob@example.com'>`
        // and project bob's inbox keyed (bob, alice). No frames are
        // produced for bob (no wire).
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::protocol::handlers::register_default_message_handlers;
        use waddle_xmpp::protocol::InboundEvent;
        use waddle_xmpp::protocol::InboundFrame;
        use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

        // ---- alice/web: live SM driving the sender pass ----
        let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
        let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
        let alice_bare: jid::BareJid = "alice@example.com".parse().expect("bare");

        let mut sender_dispatch = StanzaDispatcher::new();
        register_default_message_handlers(&mut sender_dispatch);
        let mut alice_sm = XmppStateMachine::new("example.com", sender_dispatch);
        alice_sm.transition_to_ready(alice_web.clone(), false);

        let mut wire_msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(bob.clone())));
        wire_msg.from = Some(jid::Jid::from(alice_web.clone()));
        wire_msg.type_ = xmpp_parsers::message::MessageType::Chat;
        wire_msg.id = Some("wire-id".to_string());
        wire_msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("wire-trace body".to_string()),
        );

        let alice_events = alice_sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(
            Box::new(Stanza::Message(wire_msg)),
        )));

        // ---- shared storage + dispatcher for the headless pass ----
        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
        let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
        let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
        // The headless pass constructs a transient `XmppStateMachine`
        // for bob, cloning this dispatcher so the recipient handler
        // chain runs against bob's bare JID. XEP-0359 stanza-id
        // determinism is owned by the per-machine `IdGenerator` (see
        // `XmppStateMachine::with_id_gen`), not by the dispatcher
        // itself — this fixture relies on uniqueness rather than
        // deterministic ids.
        let mut headless_dispatch = StanzaDispatcher::new();
        register_default_message_handlers(&mut headless_dispatch);
        let dispatcher = Arc::new(headless_dispatch);
        let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

        // Run the interpreter on alice's full event batch. The
        // ArchiveDirect for alice's bare lands in alice's archive,
        // ProjectInbox for (alice, bob) lands in alice's inbox, and
        // the bare-JID RouteToConnection for bob with no live
        // resources triggers the headless pass.
        let outcome = interpret(alice_events, &deps).await;

        // alice's MAM has 1 entry; <stanza-id by='alice@example.com'>
        // present.
        let alice_archive = mam
            .query_messages(&alice_bare, &Default::default())
            .await
            .expect("query alice");
        assert_eq!(
            alice_archive.messages.len(),
            1,
            "alice archive has one entry"
        );
        assert!(
            alice_archive.messages[0]
                .stanza_xml
                .as_deref()
                .map(|xml| xml.contains("by=\"alice@example.com\""))
                .unwrap_or(false),
            "alice archive entry carries XEP-0359 <stanza-id by='alice@example.com'/>: \
             {:?}",
            alice_archive.messages[0].stanza_xml
        );

        // bob's MAM has 1 entry; <stanza-id by='bob@example.com'>
        // present (recipient-side stamp by the headless pass).
        let bob_archive = mam
            .query_messages(&bob, &Default::default())
            .await
            .expect("query bob");
        assert_eq!(
            bob_archive.messages.len(),
            1,
            "headless pass writes one archive entry for bob"
        );
        assert!(
            bob_archive.messages[0]
                .stanza_xml
                .as_deref()
                .map(|xml| xml.contains("by=\"bob@example.com\""))
                .unwrap_or(false),
            "bob archive entry carries XEP-0359 <stanza-id by='bob@example.com'/>: \
             {:?}",
            bob_archive.messages[0].stanza_xml
        );

        // bob's inbox has 1 row at (bob, alice).
        let bob_inbox = inbox_concrete.list(&bob).await.expect("inbox bob");
        assert_eq!(
            bob_inbox.len(),
            1,
            "headless pass projects exactly one inbox row for bob"
        );
        assert_eq!(bob_inbox[0].partner, alice_bare);

        // No frames for bob — the headless pass discards any inner
        // SendStanza. The outer outcome may still carry alice's own
        // sender-side frames (none in this fixture because there's no
        // alice connection registered), so this asserts only the
        // negative: no frame addressed 'to=bob' leaks out.
        for frame in &outcome.frames {
            assert!(
                !frame.contains("to=\"bob@example.com\""),
                "headless pass must not produce wire frames for offline bob; got: {frame}"
            );
        }
    }

    #[tokio::test]
    async fn offline_recipient_pass_skipped_for_remote_domain() {
        // bob@other.example with `local_domain="example.com"` -> drop,
        // no recipient pass run, no archive, no inbox.
        use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
        use waddle_xmpp::mam::storage::InMemoryMamStorage;
        use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
        let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
        let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
        let dispatcher = pipelined_dispatcher();
        let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

        let msg = chat_msg("alice@example.com/web", "bob@other.example.com", "remote");
        let events = vec![OutboundEvent::RouteToConnection {
            jid: "bob@other.example.com"
                .parse::<jid::Jid>()
                .expect("bare jid"),
            stanza: Box::new(Stanza::Message(msg)),
        }];
        let _ = interpret(events, &deps).await;

        let bob_remote: jid::BareJid = "bob@other.example.com".parse().expect("bare");
        let bob_archive = mam
            .query_messages(&bob_remote, &Default::default())
            .await
            .expect("query bob");
        assert!(
            bob_archive.messages.is_empty(),
            "cross-domain bare JID drops without running the headless pass"
        );
        let entries = inbox_concrete.list(&bob_remote).await.expect("list");
        assert!(
            entries.is_empty(),
            "cross-domain bare JID drops without inbox projection"
        );
    }

    // -----------------------------------------------------------------
    // XEP-0045 §8.1 — PersistRoomSubject interpreter arm
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn xep_0045_persist_room_subject_writes_state_via_room_actor() {
        // Per-arm coverage for `OutboundEvent::PersistRoomSubject`
        // (Copilot review, PR #319). Drives the event through
        // `interpret(...)` against a real `RoomRegistryActor` and a
        // pre-created room actor, then queries the room snapshot to
        // confirm the actor wrote `MucRoom.subject` to a `SubjectState`
        // matching the event payload.
        use chrono::TimeZone;
        use waddle_xmpp::muc::room_actor::GetSnapshot;
        use waddle_xmpp::muc::room_registry_actor::CreateRoom;
        use waddle_xmpp::muc::RoomConfig;
        use waddle_xmpp::xep::xep0421::OccupantIdSecret;

        let registry = ConnectionRegistry::new();
        let room_registry = kameo::spawn(RoomRegistryActor::new(
            "muc.example.com".to_string(),
            OccupantIdSecret::new(b"persist-subject-arm-test-secret-32b".to_vec())
                .expect("test secret meets length floor"),
        ));
        let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
        let _room_actor = room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room");

        let deps = Deps {
            connection_registry: &registry,
            sm_session_registry: None,
            mam_storage: None,
            inbox_storage: None,
            extension_manager: None,
            room_registry: Some(&room_registry),
            web_socket_state: None,
            authenticated_session: None,
            local_domain: "example.com",
            blocking_storage: None,
            message_dispatcher: None,
        };

        let setter: jid::BareJid = "alice@example.com".parse().expect("setter bare jid");
        let texts = waddle_xmpp::muc::RoomSubjectTexts::from_iter([
            (String::new(), "Default subject".to_string()),
            ("en".to_string(), "English subject".to_string()),
        ]);
        let set_at = chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();

        let events = vec![OutboundEvent::PersistRoomSubject {
            room: room_jid.clone(),
            texts: texts.clone(),
            setter: setter.clone(),
            setter_nick: "alice-nick".to_string(),
            set_at,
        }];
        let _outcome = interpret(events, &deps).await;

        // Verify the room actor wrote `SubjectState` matching the event payload.
        let actor = room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("registry ask")
            .expect("room actor present");
        let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
        let stored = snapshot
            .room
            .subject
            .expect("PersistRoomSubject must land a SubjectState");
        assert_eq!(stored.texts, texts);
        assert_eq!(stored.setter, setter);
        assert_eq!(stored.setter_nick, "alice-nick");
        assert_eq!(stored.set_at, set_at);
    }

    #[tokio::test]
    async fn xep_0045_persist_room_subject_with_no_registry_is_noop() {
        // Defensive coverage for the `room_registry: None` skip arm —
        // a `PersistRoomSubject` arriving in a deployment without a
        // room registry must be logged-and-skipped, not panicked.
        use chrono::TimeZone;

        let registry = ConnectionRegistry::new();
        let deps = Deps::registry_only(&registry);

        let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
        let setter: jid::BareJid = "alice@example.com".parse().expect("setter bare jid");
        let texts =
            waddle_xmpp::muc::RoomSubjectTexts::from_iter([(String::new(), "ignored".to_string())]);
        let events = vec![OutboundEvent::PersistRoomSubject {
            room: room_jid,
            texts,
            setter,
            setter_nick: "alice-nick".to_string(),
            set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
        }];
        let outcome = interpret(events, &deps).await;
        assert!(outcome.frames.is_empty());
        assert!(!outcome.close);
    }
}
