//! XMPP over WebSocket (RFC 7395) — WebSocket-only C2S transport.
//!
//! Provides the single WebSocket XMPP endpoint on port 443.  Each accepted
//! connection runs through a typed [`ConnectionPhase`] lifecycle state machine
//! (Unauthenticated → Authenticated → Ready → Closing) with no TCP fallback
//! and no legacy dispatch paths.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use futures::{Sink, SinkExt, StreamExt};
use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::Writer;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use waddle_xmpp::auth::{parse_oauthbearer, OAuthBearerResult};
use waddle_xmpp::commands::CommandRegistry;
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::mam::MamStorage;
use waddle_xmpp::muc::room_actor::{LeaveByRealJid, RoomActor};
use waddle_xmpp::muc::room_registry_actor::RoomRegistryActor;
use waddle_xmpp::muc::RoomConfig;
use waddle_xmpp::protocol::frame::{
    inject_client_ns_if_missing, InboundFrame, ParseError, MAX_FRAME_SIZE,
};
use waddle_xmpp::protocol::{
    Blocklist, ConnectionPhase, InboundEvent, OutboundEvent, ScramPendingState, StanzaDispatcher,
    XmppStateMachine,
};
use waddle_xmpp::registry::{ConnectionRegistry, DeliveryKind, OutboundStanza};
use waddle_xmpp::stream_management::{
    InMemorySmSessionRegistry, SmEnable, SmResume, SmStanza, StreamManagementState, SM_NS,
};
use waddle_xmpp::xep::xep0421::OccupantIdSecret;
use waddle_xmpp::Stanza;
use xmpp_parsers::minidom::Element;

use crate::server::routes::websocket::handlers::iq::errors::{
    feature_not_implemented_iq_error, not_authorized_iq_error,
};

use waddle_extensions::ExtensionManager;

use super::auth::AuthState;
use crate::auth::{localpart_to_jid, NativeUserStore, Session};
use crate::server::AppState;
use waddle_xmpp::auth::ScramServer;
use waddle_xmpp::pubsub::PubSubStorage;

pub(crate) mod actor_local_endpoint;
mod batch_write;
mod call_signaling_telemetry;
#[cfg(feature = "test-support")]
mod characterization_seams;
mod cleanup;
mod connection;
mod drain_append;
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
#[cfg(feature = "clustering")]
pub(crate) use registration::complete_bind_retirements;
// The private owner is compiled now, but startup remains on the live loop.
// The feature-gated fixture is its only caller until that migration is ready.
#[cfg_attr(
    not(feature = "test-support"),
    expect(
        dead_code,
        reason = "the connection actor core is not wired into startup yet"
    )
)]
mod connection_actor_core;
#[cfg_attr(
    not(feature = "test-support"),
    expect(
        dead_code,
        reason = "the supervised actor driver remains dormant until the /ws cutover"
    )
)]
mod connection_actor_driver_task;
#[cfg(feature = "test-support")]
pub use connection_actor_core::fixture as connection_actor;
#[cfg(feature = "test-support")]
pub use connection_actor_driver_task::{
    ConnectionActorDriverTestAdmissionError, ConnectionActorDriverTestGate,
    ConnectionActorDriverTestHandle, ConnectionActorDriverTestSnapshot,
    ConnectionActorDriverTestTicket, ConnectionActorDriverTestUnknownReason,
};
#[cfg(feature = "clustering")]
mod remote_muc_retry;
pub(crate) mod replay;
mod resource_binding;
mod resume_replay;
mod sasl;
mod send;
mod session_init;
mod state;
mod stream_management;
pub mod supervised_io;
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod test_state;
mod timers;
mod transport_xml;

pub mod handlers;

pub(crate) use cleanup::broadcast_unavailable_if_no_replacement;
#[cfg(not(feature = 