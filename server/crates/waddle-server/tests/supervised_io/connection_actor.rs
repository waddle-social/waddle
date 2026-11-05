use axum::extract::{ws::WebSocketUpgrade, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::serve::Listener;
use axum::Router;
use futures::{Sink, SinkExt, StreamExt};
use kameo::actor::Spawn;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use waddle_server::clustering::NodeLifecycle;
use waddle_server::test_support::actor_local_endpoint::{
    ActorEndpointCleanupIndex, ActorLocalEndpointError, ActorLocalEndpointOwner,
    ActorLocalPublishError, ActorLocalWriteError, ActorPublicationProbe,
    ConnectionActorDriverTestGate, ConnectionActorDriverTestSnapshot,
    ConnectionActorDriverTestUnknownReason,
};
use waddle_server::test_support::connection_actor::{
    start_connection_actor, start_connection_actor_with_effect, start_connection_actor_with_hooks,
    CompleteHandledInbound, CompleteHandledInboundAtLifecycleGate,
    CompleteHandledInboundBeforeLifecycleGate, ConfirmBindingInstall, ConnectionActorError,
    ConnectionActorGateBarrier, ConnectionActorOperationId, ConnectionActorStartBarrier,
    ConnectionActorStartError, ConnectionActorStartHooks, ConnectionWireFrame,
    CountableEffectDisposition, CountableInboundEffect, InstallBinding, OutputAdmissionPause,
    PanicConnectionActor, PauseConnectionActor, PrepareBinding, RequestAcknowledgement,
    ReserveHandledInbound, RetireBinding, SeedInboundCounterForTest, SnapshotConnection,
    StopConnectionActor,
};
use waddle_server::test_support::supervised_io::{
    AdmissionError, CustodianInventoryError, CustodianState, IoCustodianInventory, Limits,
    OutboundTerminal, ReservedIoStartConfig, ShutdownProtocol, StopReason, TransportLimits,
};
use waddle_server::test_support::websocket_state;
use waddle_server::websocket_transport::{
    ActorWriteProbe, TestSocketAdmission, WebSocketInput, WebSocketOutput,
};
use waddle_xmpp::ownership::BindingRef;
use waddle_xmpp::ownership::{ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::protocol::frame::{parse_frame_with_metadata, InboundFrame, ParseError};
use waddle_xmpp::registry::user_actor::{GetResources, HoldUserActorForPublication, UserActor};
use waddle_xmpp::registry::{
    ActorOutboundStage, ActorSocketOutboundEndpoint, ArchiveDispatchRefusalReason, ConnectionEntry,
    ConnectionRegistry, LocalRetryDispatchOutcome, LocalRetryRefusal, OutboundStanza,
    OutboundWriteAcceptance, TrySendDirectWithCustody,
};
use waddle_xmpp::stream_management::{SmAck, SmRequest, SmStanza};
use xmpp_parsers::minidom::Element;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const HANDLED_OPERATION_DEADLINE: Duration = Duration::from_secs(30);

fn new_operation_id() -> ConnectionActorOperationId {
    ConnectionActorOperationId::fresh().expect("operation ID counter remains available")
}

fn reserve_request() -> ReserveHandledInbound {
    ReserveHandledInbound::new(new_operation_id())
}

struct FlushGate {
    released: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

struct ReleaseAdmissionBarrier(Arc<ConnectionActorGateBarrier>);

impl Drop for ReleaseAdmissionBarrier {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl FlushGate {
    fn new() -> Self {
        Self {
            released: AtomicBool::new(false),
            waker: Mutex::new(None),
        }
    }

    fn is_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
        if let Ok(mut waker) = self.waker.lock() {
            if let Some(waker) = waker.take() {
                waker.wake();
            }
        }
    }

    fn register(&self, waker: &Waker) {
        if let Ok(mut curre