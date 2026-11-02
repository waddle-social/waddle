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

use crate::permissions::{CheckPermission, Object, ObjectType, Permission, Subject};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use jid::{BareJid, FullJid, Jid};
use kameo::actor::ActorRef;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{debug, error, info, info_span, warn, Instrument};
use waddle_extensions::{
    message_has_framework_envelope, DisplayText, ExtensionEnvelope, ExtensionManager,
    MessageMarkupKind, MessageMarkupSpan, ReplyTarget, RoomJid, StanzaId, ThreadId, WaddleId,
};
use waddle_xmpp::carbons::{build_received_carbon, build_sent_carbon};
use waddle_xmpp::inbox::runtime::{direct_message_entry, groupchat_entry, groupchat_thread_entry};
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::inbox::InboxEntry;
use waddle_xmpp::ingress::IngressEffectIntent;
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
use waddle_xmpp::muc::SubjectState;
use waddle_xmpp::parse_managed_room_jid;
use waddle_xmpp::parser::{message_to_string, stanza_to_string};
use waddle_xmpp::protocol::event::{
    ArchivedMessage as ProtocolArchivedMessage, GroupchatInboxAudience, GroupchatThreadProjection,
    InboundEvent, MessageRef,
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
pub mod effects;
mod message_plan;
pub(crate) mod preview_plan;
pub(crate) use message_plan::reject_malformed_message;
pub use message_plan::{build_plan_deps, plan_message_dispatch};
mod direct_archive;
mod direct_call_thread;
mod direct_inbox;
mod direct_retraction;
mod displayed_marker;
mod groupchat_archive;
mod groupchat_inbox;
mod groupchat_validation;
mod handoff;
pub(crate) mod mention_audience;
mod notification_activity_ingest;
pub(crate) mod offline_delivery;
mod recipient_selection;
mod room_dispatch;
mod room_pin;
#[cfg(feature = "clustering")]
mod room_routing;
mod room_subject;
mod room_system_message;
mod route_to_connection;
mod routing;
#[cfg(feature = "clustering")]
pub(crate) use recipient_selection::{local_recipient_inventory, RecipientInventory};

pub(crate) use deps::DeliveryExecutionContext;

use archive_groupchat_event::{archive_groupchat_event, ArchiveGroupchatEventOutcome};
use archive_lookup::{
    build_carbon_envelope, lookup_archived_message, waddle_id_for_room_jid, ToElementString,
};
#[cfg(feature = "clustering")]
pub(crate) use bot::extension_room_owner;
pub(crate) use bot::{
    build_extension_message_markup, plan_extension_bot_groupchat, BotOccupancy,
    ExtensionBotDispatchError, ExtensionRoomMessage,
};
#[cfg(test)]
pub(crate) use bot::{
    BotSnapshotGate, TestJoinPresenceRoute, TEST_BOT_SNAPSHOT_GATE, TEST_JOIN_PRESENCE_ROUTE,
    TEST_SIGNING_TIME,
};
use carbons::send_carbons;
use direct_archive::archive_direct;
use direct_inbox::project_direct_inbox;
use direct_retraction::apply_retraction_tombstone;
use displayed_marker::mark_inbox_read_from_displayed;
#[cfg(test)]
use groupchat_archive::apply_groupchat_retraction_tombstone;
use groupchat_archive::{
    apply_groupchat_retraction_tombstone_outcome, archive_groupchat_message,
    project_groupchat_inbox, resolve_room_claim_fence, ArchiveGroupchatOutcome,
    GroupchatRetractionTombstoneOutcome,
};
#[cfg(test)]
pub(crate) use groupchat_inbox::reconcile_groupchat_notification_candidates;
pub(crate) use groupchat_inbox::reconcile_groupchat_notification_candidates_for_sweep;
pub(crate) use groupchat_inbox::reconcile_groupchat_notification_recovery;
use groupchat_inbox::{project_groupchat_inbox_event, ProjectGroupchatInboxEvent};
use groupchat_validation::{
    bad_request_error, build_message_error_reply, item_not_found_error, remove_framework_envelopes,
    resource_constraint_error, validate_groupchat_rich_targets,
};
#[cfg(feature = "clustering")]
pub use handoff::OrderedRelayHandoffHandle;
pub use handoff::{
    OrderedRelayHandoffCompletion, OrderedRelayInboundSequence, SmInboundCompletionTracker,
};
#[cfg(all(test, feature = "clustering"))]
pub(crate) use message_plan::ownership_plan_tests::PlanningClaims;
use offline_delivery::queue_offline_delivery;
#[cfg(test)]
pub(crate) use offline_delivery::reconcile_xep0357_notification_candidates;
pub(crate) use offline_delivery::{
    reconcile_xep0357_notification_candidates_for_sweep, PendingNotificationRecoveryCursor,
};
use room_dispatch::dispatch_to_room;
use room_pin::apply_pin_change_event;
use room_subject::{
    persist_room_subject_event, PersistRoomSubjectEventOutcome, PersistRoomSubjectRequest,
};
#[cfg(feature = "clustering")]
pub(crate) use route_to_connection::deliver_ordered_local_copy;
#[cfg(all(test, feature = "clustering"))]
pub(crate) use route_to_connection::CONTROLLED_REGISTERED_REMOTE_DELIVERY;
pub(crate) use route_to_connection::{
    bounce_undeliverable_iq, deliver_direct_to_full_locally,
    deliver_direct_to_full_with_registered_remote, deliver_frozen_invitation_resource,
    deliver_full_jid_via_ordered_relay, deliver_peer_to_full_with_registered_remote,
    deliver_registered_remote_resource, queue_processed_for_detached, route_to_connection,
    undeliverable_iq_reply, DetachedQueueOutcome,
};
#[cfg(feature = "clustering")]
pub(crate) use routing::deliver_peer_to_full_at;
pub(crate) use routing::existing_ingress_delivery;
pub(crate) use routing::{
    close_call_setup_from_outcome, deliver_direct_to_full, FullJidDeliveryOutcome,
};
use routing::{
    deliver_peer_to_live_only, run_fanout_recipient_pass, run_headless_recipient_pass,
    FanoutPassResult,
};

#[cfg(feature = "clustering")]
pub use deps::OrderedRelayRouteOriginKind;
pub use deps::{
    Deps, HostOwnedResources, InterpretOutcome, OrderedRelayRouteOrigin, SmIngressAppendContext,
    TimerCommand,
};
pub(crate) use groupchat_archive::{project_groupchat_archive, push_inbox_update};
pub(crate) use notification_activity_ingest::{
    record_presence_available_activity_on_state, record_presence_unavailable_activity_on_state,
};

#[cfg(feature = "clustering")]
mod relay_plan;
#[cfg(feature = "clustering")]
pub(crate) use relay_plan::plan_muc_for_relay;

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod retraction_ingress_tests;

#[cfg(all(test, feature = "clustering"))]
pub(crate) use route_to_connection::{ControlledMucRelay, CONTROLLED_MUC_RELAY};

mod room_result;
pub(crate) use room_result::plan_room_result;

mod implementation;
#[cfg(test)]
use implementation::apply_archive_id_rewrites;
pub(crate) use implementation::broadcast_room_system_message;
pub(crate) use implementation::capture_serialized_error_reply;
pub(super) use implementation::emit_frame;
pub use implementation::interpret;
use implementation::interpret_with_depth;
pub(crate) use implementation::prototype_body;
pub(crate) use implementation::rewrite_message_archive_ids;
use implementation::rewrite_stanza_archive_ids;
pub(crate) use implementation::ArchiveIdRewrite;
pub(crate) use implementation::NotificationRecoverySweepOutcome;
use implementation::MAX_RECIPIENT_PASS_DEPTH;
