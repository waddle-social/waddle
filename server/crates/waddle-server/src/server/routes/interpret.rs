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
//! - [`OutboundEvent::ProjectGroupchatInbox`] — sender plus durable
//!   recipient inbox upsert (channel + thread rows) plus the XEP-0430
//!   inbox push to the owner's other resources.
//!
//! - [`OutboundEvent::SendKeepaliveProbe`] — RFC 7395 §3.8 liveness
//!   probe; counted into [`InterpretOutcome::keepalive_probes`] for the
//!   transport adapter to map to a WS `Ping` frame (issue #1090).
//! - [`OutboundEvent::SetTimer`] / [`OutboundEvent::CancelTimer`] —
//!   relayed as typed [`TimerCommand`]s in
//!   [`InterpretOutcome::timer_commands`]; the adapter owns the actual
//!   clock and feeds `InboundEvent::Tick` back on expiry.
//!
//! Stubbed (warn-logged until migration steps land them):
//! - `AskSfu`, `QueryMam`, `LoadScramCredentials`,
//!   `ValidateOAuthBearer`, `RegisterConnection` — wired in later
//!   migration steps.
//! - `UnregisterConnection` — intentionally NOT wired: owner-gated registry
//!   removal is driven by the live teardown paths (which hold the session's
//!   ownership token), not by the token-less state machine. See the arm.

use crate::auth::Session;
use crate::permissions::{CheckPermission, Object, ObjectType, Permission, Subject};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use jid::{BareJid, FullJid, Jid};
use kameo::actor::ActorRef;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{debug, error, info, info_span, warn, Instrument};
use waddle_extensions::{
    message_has_framework_envelope, DisplayText, ExtensionEffect, ExtensionEnvelope,
    ExtensionManager, MessageMarkupKind, MessageMarkupSpan, ReplyTarget, RoomJid, StanzaId,
    ThreadId, WaddleId,
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
    ApplyPin, GetAffiliation, GetNicknameGeneration, GetRoomSnapshot, JoinAffiliationGrant,
    JoinWithAffiliation, RoomActor, SetSubject,
};
#[cfg(feature = "clustering")]
use waddle_xmpp::muc::room_registry_actor::DemoteRoomIfExactActor;
use waddle_xmpp::muc::room_registry_actor::{GetRoom, RoomRegistryActor};
use waddle_xmpp::parse_managed_room_jid;
use waddle_xmpp::parser::{message_to_string, stanza_to_string};
use waddle_xmpp::protocol::event::{
    ArchivedMessage as ProtocolArchivedMessage, GroupchatThreadProjection, InboundEvent, MessageRef,
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
use waddle_xmpp_core::xep0359::{
    add_stanza_id as replace_stanza_id, extract_stanza_id_by, StanzaId as Xep0359StanzaId,
};
use xmpp_parsers::message::{Message, MessageType as XmppMessageType};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::server::routes::websocket::WebSocketState;

mod archive_groupchat_event;
mod archive_lookup;
mod bot;
pub(crate) mod carbons;
mod deps;
mod direct_archive;
mod direct_call_thread;
mod direct_inbox;
mod direct_retraction;
mod displayed_marker;
mod groupchat_archive;
mod groupchat_inbox;
mod groupchat_validation;
mod handoff;
mod notification_activity_ingest;
mod offline_delivery;
mod room_dispatch;
mod room_pin;
mod room_subject;
mod room_system_message;
mod route_to_connection;
mod routing;

use archive_groupchat_event::{archive_groupchat_event, ArchiveGroupchatEventOutcome};
use archive_lookup::{
    build_carbon_envelope, lookup_archived_message, waddle_id_for_room_jid, ToElementString,
};
pub(crate) use bot::{
    build_extension_message_markup, dispatch_extension_bot_groupchat_response, ExtensionRoomMessage,
};
use carbons::send_carbons;
use direct_archive::archive_direct;
use direct_inbox::project_direct_inbox;
use direct_retraction::apply_retraction_tombstone;
use displayed_marker::mark_inbox_read_from_displayed;
use groupchat_archive::{
    apply_groupchat_retraction_tombstone, archive_groupchat_message, project_groupchat_inbox,
    resolve_room_claim_fence, ArchiveGroupchatOutcome,
};
#[cfg(test)]
pub(crate) use groupchat_inbox::reconcile_groupchat_notification_candidates;
pub(crate) use groupchat_inbox::reconcile_groupchat_notification_candidates_for_sweep;
use groupchat_inbox::{project_groupchat_inbox_event, ProjectGroupchatInboxEvent};
use groupchat_validation::{
    bad_request_error, build_message_error_reply, item_not_found_error, remove_framework_envelopes,
    resource_constraint_error, service_unavailable_error, validate_groupchat_rich_targets,
};
#[cfg(feature = "clustering")]
pub use handoff::OrderedRelayHandoffHandle;
pub use handoff::{
    OrderedRelayHandoffCompletion, OrderedRelayInboundSequence, SmInboundCompletionTracker,
};
use offline_delivery::queue_offline_delivery;
#[cfg(test)]
pub(crate) use offline_delivery::reconcile_xep0357_notification_candidates;
pub(crate) use offline_delivery::reconcile_xep0357_notification_candidates_for_sweep;
use room_dispatch::dispatch_to_room;
use room_pin::apply_pin_change_event;
use room_subject::{
    persist_room_subject_event, PersistRoomSubjectEventOutcome, PersistRoomSubjectRequest,
};
pub(crate) use route_to_connection::{bounce_undeliverable_iq, route_to_connection};
pub(crate) use routing::{deliver_direct_to_full, deliver_peer_to_full, FullJidDeliveryOutcome};
use routing::{
    deliver_peer_to_live_only, deliver_to_detached, run_fanout_recipient_pass,
    run_headless_recipient_pass, FanoutPassResult,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NotificationRecoverySweepOutcome {
    pub(crate) completed: usize,
    pub(crate) had_failure: bool,
}

#[cfg(feature = "clustering")]
pub use deps::OrderedRelayRouteOriginKind;
pub use deps::{
    Deps, GroupchatRetrySuppression, InterpretOutcome, OrderedRelayRouteOrigin, TimerCommand,
};
pub(crate) use groupchat_archive::push_inbox_update;
pub(crate) use notification_activity_ingest::{
    record_presence_available_activity_on_state, record_presence_unavailable_activity_on_state,
};

pub(crate) async fn broadcast_room_system_message(
    deps: &Deps<'_>,
    room: BareJid,
    message: Box<Message>,
) -> Option<String> {
    room_system_message::broadcast_room_system_message_event(deps, room, message, 0).await
}

#[cfg(feature = "clustering")]
pub(crate) async fn dispatch_muc_to_room_for_relay(
    deps: &Deps<'_>,
    room: BareJid,
    message: Message,
) -> InterpretOutcome {
    dispatch_to_room(deps, room, message, 0).await
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

#[derive(Clone, Debug)]
pub(crate) struct ArchiveIdRewrite {
    by: Jid,
    from_id: String,
    to_id: String,
}

impl ArchiveIdRewrite {
    pub(super) fn from_store_result(
        by: Jid,
        requested_id: String,
        stored_id: String,
    ) -> Option<Self> {
        if requested_id.is_empty() || requested_id == stored_id {
            return None;
        }
        Some(Self {
            by,
            from_id: requested_id,
            to_id: stored_id,
        })
    }

    fn rewritten_id_for(&self, by: &Jid, id: &str) -> Option<&str> {
        (&self.by == by && self.from_id == id).then_some(self.to_id.as_str())
    }
}

fn apply_archive_id_rewrites(event: &mut OutboundEvent, rewrites: &[ArchiveIdRewrite]) {
    if rewrites.is_empty() {
        return;
    }
    match event {
        OutboundEvent::SendStanza(stanza) | OutboundEvent::RouteToConnection { stanza, .. } => {
            rewrite_stanza_archive_ids(stanza, rewrites);
        }
        OutboundEvent::DispatchToRoom { message, .. }
        | OutboundEvent::ArchiveGroupchat { message, .. }
        | OutboundEvent::ArchiveDirect { message, .. }
        | OutboundEvent::ProjectGroupchatInbox { message, .. }
        | OutboundEvent::SendCarbons { message, .. }
        | OutboundEvent::RequestEnrichment { message, .. }
        | OutboundEvent::ApplyGroupchatRetractionTombstone {
            retraction_message: message,
            ..
        } => {
            rewrite_message_archive_ids(message, rewrites);
        }
        OutboundEvent::ProjectInbox {
            message,
            archive_ref,
            ..
        } => {
            rewrite_message_archive_ids(message, rewrites);
            rewrite_stanza_id(archive_ref, rewrites);
        }
        OutboundEvent::QueueOfflineDelivery {
            payload,
            original_message,
            ..
        } => {
            match payload {
                waddle_xmpp::pending_delivery::PendingPayload::Archived(stanza_id) => {
                    rewrite_stanza_id(stanza_id, rewrites);
                }
                waddle_xmpp::pending_delivery::PendingPayload::Transient(message) => {
                    rewrite_message_archive_ids(message, rewrites);
                }
            }
            rewrite_message_archive_ids(original_message, rewrites);
        }
        _ => {}
    }
}

enum BatchSuppression {
    None,
    All,
    /// Tombstone-hit swallow: everything is suppressed EXCEPT the
    /// idempotent, terminal-guarded retraction-tombstone application.
    /// A retry whose retraction-request row was itself tombstoned may
    /// still be healing a crash between the request's archive commit
    /// and the target tombstone apply — the CAS guard makes letting it
    /// through safe in every other case (Greptile review on PR #1412).
    /// `All` (ownership loss / subject-persist bounce) must NOT share
    /// this escape: a deposed node may not touch the archive at all.
    TombstoneSwallow,
    NonSender {
        sender: BareJid,
    },
}

impl BatchSuppression {
    fn allows(&self, event: &OutboundEvent) -> bool {
        match self {
            Self::None => true,
            Self::All => false,
            Self::TombstoneSwallow => {
                matches!(
                    event,
                    OutboundEvent::ApplyGroupchatRetractionTombstone { .. }
                )
            }
            Self::NonSender { sender } => match event {
                // A crash can commit the retraction-request archive row before
                // applying its target tombstone. A deduplicated retry must
                // finish that idempotent, monotonic second effect.
                OutboundEvent::ApplyGroupchatRetractionTombstone { .. } => true,
                OutboundEvent::RouteToConnection { jid, .. } => &jid.to_bare() == sender,
                OutboundEvent::ProjectGroupchatInbox { owner, .. } => owner == sender,
                _ => false,
            },
        }
    }
}

fn rewrite_stanza_archive_ids(stanza: &mut Stanza, rewrites: &[ArchiveIdRewrite]) {
    if let Stanza::Message(message) = stanza {
        rewrite_message_archive_ids(message, rewrites);
    }
}

pub(crate) fn rewrite_message_archive_ids(message: &mut Message, rewrites: &[ArchiveIdRewrite]) {
    for rewrite in rewrites {
        if extract_stanza_id_by(message, &rewrite.by).as_deref() == Some(rewrite.from_id.as_str()) {
            replace_stanza_id(
                message,
                &Xep0359StanzaId::new(rewrite.to_id.clone(), rewrite.by.clone()),
            );
        }
    }
}

fn rewrite_stanza_id(stanza_id: &mut Xep0359StanzaId, rewrites: &[ArchiveIdRewrite]) {
    for rewrite in rewrites {
        if let Some(rewritten) = rewrite.rewritten_id_for(&stanza_id.by, stanza_id.id.as_str()) {
            stanza_id.id = rewritten.to_owned();
        }
    }
}

/// Best-effort plain-text body of a message: the default-language
/// `<body/>` if present, else any body.
///
/// Shared interpret-layer helper used by both the DM and groupchat
/// notification-candidate paths (and the groupchat archive prototype) so
/// neither emission site has to reach into a sibling module for body
/// extraction.
pub(crate) fn prototype_body(message: &Message) -> Option<String> {
    message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .cloned()
}

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
    let mut archive_id_rewrites: Vec<ArchiveIdRewrite> = Vec::new();
    let mut batch_suppression = BatchSuppression::None;

    for mut event in events {
        if !batch_suppression.allows(&event) {
            continue;
        }
        apply_archive_id_rewrites(&mut event, &archive_id_rewrites);
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
                for stanza in route_to_connection(deps, jid, stanza, recursion_depth).await {
                    match stanza.to_element_string() {
                        Ok(xml) => outcome.frames.push(xml),
                        Err(err) => {
                            error!(
                                error = %err,
                                "failed to serialize route fallback stanza; dropping frame"
                            );
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
                // emission, durable-recipient inbox projection, and
                // per-occupant fan-out — emitting typed
                // [`OutboundEvent`]s the interpreter then resolves
                // recursively below.
                let nested =
                    Box::pin(dispatch_to_room(deps, room, *message, recursion_depth)).await;
                let InterpretOutcome {
                    frames: nested_frames,
                    close: nested_close,
                    feedback: nested_feedback,
                    keepalive_probes: nested_probes,
                    timer_commands: nested_timer_commands,
                    // Retry suppression is local to the nested room batch;
                    // dispatch_to_room consumes it before returning so it
                    // cannot suppress unrelated siblings in this outer batch.
                    retry_suppression: _,
                    archive_id_rewrites: nested_rewrites,
                } = nested;
                outcome.frames.extend(nested_frames);
                if nested_close {
                    outcome.close = true;
                }
                outcome.feedback.extend(nested_feedback);
                outcome.keepalive_probes += nested_probes;
                outcome.timer_commands.extend(nested_timer_commands);
                // Carry nested rewrites forward so later events in THIS
                // batch see them too.
                archive_id_rewrites.extend(nested_rewrites);
            }
            OutboundEvent::ProjectInbox {
                owner,
                peer,
                message,
                archive_ref,
                increment_unread,
            } => {
                project_direct_inbox(deps, owner, peer, message, archive_ref, increment_unread)
                    .await;
            }
            OutboundEvent::SendCarbons {
                owner,
                message,
                kind,
                exclude,
            } => {
                send_carbons(registry, deps, owner, message, kind, exclude).await;
            }
            OutboundEvent::MarkInboxReadFromDisplayed {
                owner,
                room,
                displayed_message_id,
            } => {
                mark_inbox_read_from_displayed(deps, owner, room, displayed_message_id).await;
            }
            OutboundEvent::LookupArchivedMessage {
                id,
                archive,
                archive_kind,
                reference,
            } => {
                let result =
                    lookup_archived_message(deps, &archive, archive_kind, &reference).await;
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
                // Deliberately a warn-only stub, NOT a registry unregister
                // (Greptile review on PR #1177). No producer emits this event,
                // and the variant carries only a `FullJid` — no ownership token
                // or SM stream id — so a plain `registry.unregister(&jid)` here
                // could evict an UNRELATED live session that holds the same full
                // JID, exactly the replacement-eviction footgun fixed on every
                // real teardown path (`unregister_if_owner` /
                // `unregister_if_sm_stream_id`). Owner-gated registry removal is
                // a server-side concern the live teardown paths already own
                // (`cleanup::cleanup_connection_shutdown`,
                // `stream_management::registration`'s resume-rollback helpers,
                // `cleanup_invalidated_detached_session`, the SM-expiry janitor);
                // the pure state machine has no ownership token to drive it
                // safely. If this is ever wired, the event MUST first carry the
                // owning session's token so the removal can be gated.
                warn!(
                    variant = "UnregisterConnection",
                    jid = %jid,
                    "OutboundEvent variant not wired in interpreter: registry \
                     unregister is owner-gated and driven by the live teardown \
                     paths, not the state machine"
                );
            }
            OutboundEvent::ArchiveGroupchat {
                room,
                sender,
                message,
                sender_nickname_generation,
                sender_item,
            } => {
                match archive_groupchat_event(
                    deps,
                    room,
                    sender,
                    message,
                    sender_nickname_generation,
                    sender_item,
                )
                .await
                {
                    ArchiveGroupchatEventOutcome::Stored(Some(rewrite)) => {
                        archive_id_rewrites.push(rewrite);
                    }
                    ArchiveGroupchatEventOutcome::Stored(None)
                    | ArchiveGroupchatEventOutcome::Skipped => {}
                    ArchiveGroupchatEventOutcome::Deduplicated { rewrite, sender } => {
                        if let Some(rewrite) = rewrite {
                            archive_id_rewrites.push(rewrite);
                        }
                        outcome.retry_suppression = Some(GroupchatRetrySuppression::Deduplicated);
                        batch_suppression = BatchSuppression::NonSender { sender };
                    }
                    ArchiveGroupchatEventOutcome::TombstoneHit => {
                        debug!(
                            "ArchiveGroupchat: tombstone hit; silently suppressing remaining dispatch batch"
                        );
                        outcome.retry_suppression =
                            Some(GroupchatRetrySuppression::TombstoneSwallowed);
                        batch_suppression = BatchSuppression::TombstoneSwallow;
                    }
                    ArchiveGroupchatEventOutcome::OwnershipLost(bounce) => {
                        // FIX 1: not archived, not fanned out — suppress
                        // every remaining event in this batch (the
                        // reflector's fan-out for this same message) and
                        // bounce the sender with the same typed
                        // recoverable error `dispatch_to_room`'s own
                        // pre-fan-out check uses.
                        match Stanza::Message(*bounce).to_element_string() {
                            Ok(xml) => outcome.frames.push(xml),
                            Err(error) => {
                                warn!(
                                    %error,
                                    "ArchiveGroupchat: failed to serialize ownership-gap bounce reply"
                                );
                            }
                        }
                        batch_suppression = BatchSuppression::All;
                    }
                }
            }
            OutboundEvent::ApplyPinChange { room, request } => {
                apply_pin_change_event(deps, room, request, recursion_depth).await;
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
                let tombstoned = apply_groupchat_retraction_tombstone(
                    mam_storage,
                    deps.sm_session_registry,
                    deps.pending_delivery_storage,
                    &room,
                    &target_message_id,
                    &retraction_message,
                )
                .await;
                if tombstoned {
                    if let Some(state) = deps.web_socket_state {
                        crate::server::routes::websocket::link_preview_refs::clear_current_message_preview_refs(
                            state.deps.app_state.db_pool.global_actor(),
                            &room,
                            &target_message_id,
                        )
                        .await;
                    }
                }
                // #414: cascade XEP-0424 retraction to the room's pin
                // list. If the retracted stanza-id is currently pinned,
                // remove it from the projection and broadcast a
                // synthetic unpin system message so live clients see the
                // tab update without a separate poll.
                room_pin::cascade_retraction_to_pin_list(
                    deps,
                    room,
                    target_message_id,
                    recursion_depth,
                )
                .await;
            }
            OutboundEvent::PersistRoomSubject {
                room,
                claim_fence,
                texts,
                setter,
                sender,
                message,
                setter_nick,
                set_at,
            } => {
                match persist_room_subject_event(
                    deps,
                    PersistRoomSubjectRequest {
                        room,
                        claim_fence,
                        texts,
                        setter,
                        sender,
                        message,
                        setter_nick,
                        set_at,
                    },
                )
                .await
                {
                    PersistRoomSubjectEventOutcome::Committed => {}
                    PersistRoomSubjectEventOutcome::BounceAndHalt(bounce) => {
                        match Stanza::Message(*bounce).to_element_string() {
                            Ok(xml) => outcome.frames.push(xml),
                            Err(error) => warn!(
                                %error,
                                "PersistRoomSubject: failed to serialize retryable bounce reply"
                            ),
                        }
                        batch_suppression = BatchSuppression::All;
                    }
                }
            }
            OutboundEvent::ProjectGroupchatInbox {
                owner,
                room,
                message,
                is_recipient,
                is_durable_recipient,
                is_live_occupant,
                room_members_only,
                sender_can_broadcast_channel_mention,
                thread,
                dispatch_timestamp,
            } => {
                project_groupchat_inbox_event(ProjectGroupchatInboxEvent {
                    deps,
                    owner,
                    room,
                    message,
                    is_recipient,
                    is_durable_recipient,
                    is_live_occupant,
                    room_members_only,
                    sender_can_broadcast_channel_mention,
                    thread,
                    dispatch_timestamp,
                })
                .await;
            }
            OutboundEvent::ArchiveDirect {
                archive_jid,
                from,
                to,
                message,
            } => {
                if let Some(rewrite) = archive_direct(deps, archive_jid, from, to, message).await {
                    archive_id_rewrites.push(rewrite);
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
            OutboundEvent::SendKeepaliveProbe => {
                // A liveness probe is a transport control frame, not a
                // stanza — it never serializes to XML here. The command
                // rides the outcome back to the transport adapter,
                // which maps it to its native frame (WS `Ping`).
                outcome.keepalive_probes += 1;
            }
            OutboundEvent::SetTimer { id, duration_ms } => {
                outcome
                    .timer_commands
                    .push(TimerCommand::Set { id, duration_ms });
            }
            OutboundEvent::CancelTimer(id) => {
                outcome.timer_commands.push(TimerCommand::Cancel(id));
            }
            OutboundEvent::QueueOfflineDelivery {
                recipient,
                payload,
                original_receipt_at,
                original_message,
            } => {
                queue_offline_delivery(
                    deps,
                    recipient,
                    payload,
                    original_receipt_at,
                    original_message,
                )
                .await;
            }
        }
    }

    outcome.archive_id_rewrites = archive_id_rewrites;
    outcome
}

async fn enrich_message_event(deps: &Deps<'_>, message: Message) -> Message {
    // Observability (#1320): time the enrichment pass and count the
    // embeds it adds as the payload-count delta before/after.
    let started = std::time::Instant::now();
    let payloads_before = message.payloads.len();
    let enriched = if deps.extension_manager.is_none() {
        debug!(
            "RequestEnrichment: no extension_manager in Deps; \
             feeding original message back unchanged"
        );
        message
    } else {
        debug!("RequestEnrichment: direct messages do not carry a typed Waddle scope; skipping");
        message
    };
    let embeds_added = enriched.payloads.len().saturating_sub(payloads_before) as u64;
    waddle_xmpp::metrics::record_extension_enrichment(
        started.elapsed().as_secs_f64() * 1000.0,
        embeds_added,
    );
    enriched
}

#[cfg(test)]
mod tests;
