//! XMPP over WebSocket (RFC 7395) — WebSocket-only C2S transport.
//!
//! Provides the single WebSocket XMPP endpoint on port 443.  Each accepted
//! connection runs through a typed [`ConnectionPhase`] lifecycle state machine
//! (Unauthenticated → Authenticated → Ready → Closing) with no TCP fallback
//! and no legacy dispatch paths.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
    routing::get,
    Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use futures::{Sink, SinkExt, StreamExt};
use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use quick_xml::{
    events::{BytesEnd, BytesStart, BytesText, Event},
    Writer,
};
use std::{str::FromStr, sync::Arc};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use waddle_xmpp::{
    auth::{parse_oauthbearer, OAuthBearerResult},
    commands::CommandRegistry,
    inbox::storage::InboxStorage,
    mam::MamStorage,
    muc::{
        room_actor::{LeaveByRealJid, RoomActor},
        room_registry_actor::RoomRegistryActor,
        RoomConfig,
    },
    protocol::{
        frame::{inject_client_ns_if_missing, InboundFrame, ParseError, MAX_FRAME_SIZE},
        Blocklist, ConnectionPhase, InboundEvent, OutboundEvent, ScramPendingState,
        StanzaDispatcher, XmppStateMachine,
    },
    registry::{ConnectionRegistry, DeliveryKind, OutboundStanza},
    stream_management::{
        InMemorySmSessionRegistry, SmEnable, SmResume, SmStanza, StreamManagementState, SM_NS,
    },
    xep::xep0421::OccupantIdSecret,
    Stanza,
};
use xmpp_parsers::minidom::Element;

use crate::server::routes::websocket::handlers::iq::errors::{
    feature_not_implemented_iq_error, not_authorized_iq_error,
};

#[cfg(test)]
use waddle_xmpp::mam::InMemoryMamStorage;

use waddle_extensions::ExtensionManager;

use super::auth::AuthState;
use crate::auth::{localpart_to_jid, NativeUserStore, Session};
use crate::server::AppState;
use waddle_xmpp::auth::ScramServer;
use waddle_xmpp::pubsub::PubSubStorage;

mod batch_write;
mod call_signaling_telemetry;
mod cleanup;
mod connection;
mod frame;
mod frame_backstop;
pub(crate) mod interpret_loop;
pub(crate) mod link_preview_refs;
pub(crate) mod link_preview_telemetry;
mod local_departures;
pub(crate) mod muc_call_sfu;
pub(crate) mod muc_invites;
mod outbound;
mod parse_errors;
mod registration;
mod replay;
mod resource_binding;
mod sasl;
mod send;
mod session_init;
mod state;
mod stream_management;
mod timers;
mod transport_xml;

pub mod handlers;

pub(crate) use cleanup::broadcast_unavailable_if_no_replacement;
#[cfg(not(feature = "clustering"))]
pub use cleanup::cleanup_muc_presence_for_jid;
#[cfg(feature = "clustering")]
pub use cleanup::cleanup_muc_presence_for_jid_with_origin;
pub(crate) use cleanup::echo_muc_self_unavailable;
pub(crate) use cleanup::redrive_local_muc_cleanup;
#[cfg(feature = "clustering")]
pub(crate) use cleanup::redrive_remote_muc_cleanup;
pub(crate) use cleanup::redrive_terminal_pending_rows_to_live_resource;
pub use cleanup::MucCleanupOutcome;

/// Upper bound on a single `LeaveByRealJid` ask from the WS/janitor side: a
/// wedged room must not stall disconnect cleanup, the departure janitor, an
/// administrative removal, or an explicit leave's wait-class bounce.
pub(crate) const LEAVE_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub(crate) use cleanup::TerminalRedriveOutcome;
pub(crate) use cleanup::{
    broadcast_muc_leave_to_remaining, broadcast_muc_muji_clear_to_remaining, maybe_evict_empty_room,
};
pub use connection::router;
pub(crate) use local_departures::{
    LocalDepartureItem, PendingLocalDeparture, PendingLocalMucDepartures,
};
pub(crate) use state::ResolvedPrincipal;
pub use state::{
    default_link_preview_resolve_permits, ActiveCallThread, DmCallThreadKey, DmPairKey, DmPinStore,
    PendingDmCallOffer, ProtocolServices, RemoteMucMemberships, ResolverAffiliationSyncSchedule,
    ResolverAffiliationSyncScheduler, ResolverAffiliationSyncWork, WebSocketDeps, WebSocketState,
    XmppServiceDomains,
};

pub(crate) use cleanup::{
    drain_destroy_completions, get_or_create_room_actor, get_room_actor, get_room_actor_result,
    is_muc_room_jid,
};
pub(crate) use muc_call_sfu::{
    note_participant_left_by_call_id, note_participant_left_from_webhook,
    observe_participant_sids_from_webhook,
};

pub(crate) use transport_xml::{
    build_iq_error_xml_typed, build_iq_error_xml_with_payload, build_iq_result_xml, element_to_xml,
    iq_to_xml, stanza_to_xml,
};
#[cfg(test)]
pub(crate) use waddle_xmpp::protocol::frame::parse_frame;
#[cfg(test)]
pub(crate) mod tests;
