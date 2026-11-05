//! Dormant physical owner for one actor-side local endpoint.
//!
//! This owner is release compiled but is not selected by `/ws`. The cleanup
//! index retains accepted envelopes after the actor or startup caller exits
//! and until both physical pumps join and a terminal owner settles them.

use super::connection_actor_driver_task::{
    ConnectionActorDriverTask, ConnectionActorDriverTaskAckError, ConnectionActorDriverTaskCleanup,
    ConnectionActorDriverTicket,
};
#[cfg(feature = "test-support")]
use super::connection_actor_driver_task::{
    ConnectionActorDriverTestHandle, ConnectionActorDriverTestTicket,
};
use super::supervised_io::{CustodianState, IoCustodianHandle, StopReason, TransportLimits};
use crate::websocket_transport::{
    ActorSocketPublicationAuthority, OutputError, SocketAdmission, SocketAdmissionError,
    WebSocketInput, WebSocketOutput,
};
use axum::extract::ws::{Message, WebSocket};
#[cfg(feature = "test-support")]
use futures::FutureExt;
use futures::{future::poll_fn, Sink, SinkExt};
use jid::FullJid;
use kameo::actor::ActorRef;
use kameo::error::SendError;
use std::collections::HashMap;
#[cfg(feature = "test-support")]
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(feature = "test-support")]
use tokio::sync::oneshot;
use waddle_xmpp::ownership::ConnectionRef;
#[cfg(feature = "test-support")]
use waddle_xmpp::registry::user_actor::{
    ConfirmProvisionalConnection, RegisterProvisionalConnectionIfExpected,
};
use waddle_xmpp::registry::user_actor::{UnregisterConnectionAndReportEmpty, UserActor};
use waddle_xmpp::registry::{
    ActorOutboundCustody, ActorOutboundId, ActorOutboundReceiver, ActorOutboundStage,
    ActorProvisionalConnectionEntry, ActorSocketOutboundEndpoint, ConnectionRegistry, DeliveryKind,
    LocalConnectionBindingId, OutboundStanza,
};

#[derive(Debug, thiserror::Error)]
pub enum ActorLocalEndpointError {
    #[error(transparent)]
    Socket(#[from] SocketAdmissionError),
    #[error("actor terminal inventory is full or already contains this socket")]
    InventoryFull,
    #[error("actor endpoint bounds must be positive")]
    InvalidBounds,
    #[error("the prior binding still owns an active endpoint or retained work")]
    PriorBindingActive,
    #[error("a driver task is already attached to this connection")]
    DriverAlreadyAttached,
    #[error("endpoint cleanup has started and cannot accept a driver task")]
    DriverCleanupStarted,
    #[error("driver task belongs to a different physical connection")]
    DriverConnectionMismatch,
    #[error("the connection driver task could not be started")]
    DriverTaskUnavailable,
}

#[derive(Debug, thiserror::Error)]
pub enum ActorLocalPublishError {
    #[error(transparent)]
    Authority(#[from] SocketAdmissionError),
    #[error("local registry predecessor or binding did not match")]
    RegistryConflict,
    #[error("user actor mirror refused the expected predecessor")]
    MirrorConflict,
    #[error("user actor mirror outcome was not confirmed before the deadline")]
    MirrorUncertain,
    #[error("provisional activation lost exact ownership")]
    ActivationLost,
    #[error("startup caller cancelled before activation")]
    CallerCancelled,
    #[error("independent publication owner exited")]
    PublicationOwnerLost,
}

#[derive(Debug, thiserror::Error)]
pub enum ActorLocalWriteError {
    #[error("outbound ID or physical owner does not match")]
    WrongOwner,
    #[error("accepted outbound ID is absent from custody")]
    MissingCustody,
    #[error("peer stanza requires the actor's recipient pass")]
    RecipientPassRequired,
    #[error("outbound XML serialization failed: {0}")]
    Serialize(#[from] xmpp_parsers::minidom::Error),
    #[error("outbound XML was not UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error(transparent)]
    Output(#[from] OutputError<axum::Error>),
}

struct CleanupRecord {
    custodian: IoCustodianHandle,
    custodies: Mutex<Vec<Arc<ActorOutboundCustody>>>,
    publications: Mutex<PublicationState>,