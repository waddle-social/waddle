//! Bounded raw WebSocket transport for a future single connection owner.
//!
//! This boundary does not parse incoming XMPP. Raw frames must be parsed before
//! entering protocol actor messages. Outgoing data carries this transport's
//! exact identity and lifecycle authority to the physical sink enqueue.
//! The supervised adapter remains inactive. The production upgrade path uses
//! an `AppState`-bound admission as a capacity hold for the legacy loop. Its
//! authority-bound writes fence the source revision and lifecycle generation
//! at `start_send`, but it does not start or join these pumps.
//! Revocation after splitting but before supervised startup gets a bounded
//! Close attempt on the split writer. If startup is dropped during that window,
//! an independent bounded cleanup task retains its capacity reservation.
//!
//! `SinkExt::send` waits for the physical flush. A protocol owner must instead
//! keep a `submit` future and receipts selectable alongside inbound frames and
//! independent control, so a slow writer cannot prevent XEP-0198 ACK handling.
//!
//! Queue limits cover admitted and in-flight items. The reader can additionally
//! hold one frame while waiting for capacity; the output adapter can hold one
//! frame while it awaits admission. Each admitted frame must fit its directional
//! byte limit. The underlying WebSocket can allocate an oversized frame before
//! this boundary observes and rejects it; these limits do not bound that storage.
//! A received frame leaves the queue budget when yielded. The protocol owner
//! must separately bound any frames or parsed operations that it retains. A
//! terminal I/O error is one retained value; its internal allocation is not
//! measured by the WebSocket payload-byte budget.
//! One terminal slot retains a peer Close or read error behind prior inputs;
//! an oversized Close is reported as `FrameTooLarge` rather than retained.

use crate::clustering::{NodeAdmissionError, NodeAdmissionPermit, NodeLifecycle};
use crate::server::routes::websocket::supervised_io::{
    fixed_terminal_text, start_reserved_supervised_io, FixedTerminalText, OrderedInboundTerminal,
    QueueItem, ReservedIoStartConfig, ShutdownProtocol, StopSignal, WriteAuthority, WriteSender,
};
use axum::extract::ws::{Message, WebSocket};
use futures::future::{poll_fn, BoxFuture};
use futures::{Sink, SinkExt, Stream, StreamExt};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::{
    ConnectionRef, NodeIdentityStamp, NodeLeaseIdentity, SharedNodeIdentity,
};
use waddle_xmpp::registry::{ActorOutboundCustody, ActorOutboundId};

pub use crate::server::routes::websocket::supervised_io::{
    AdmissionError, CustodianInventoryError, CustodianState, CustodianSummary, InboundTerminal,
    IoCustodianHandle, IoCustodianInventory, IoCustodianObserver, IoCustodianReservation, Limits,
    LimitsError, OutboundTerminal, ReservedStartError, ReservedStartStage, StopReason,
    TransportLimits, WriteProgress, WriteReceipt,
};

/// A terminal read error. The original transport error is preserved.
#[derive(Debug, thiserror::Error)]
pub enum InputError<E> {
    #[error("WebSocket read failed: {0}")]
    Transport(E),
    #[error("WebSocket frame has {size} bytes, exceeding the {max_bytes}-byte inbound limit")]
    FrameTooLarge { size: usize, max_bytes: usize },
}

/// A write error. Transport errors retain their original concrete value.
#[derive(Debug, thiserror::Error)]
pub enum OutputError<E> {
    #[error("WebSocket admission failed: {0:?}")]
    Admission(AdmissionError),
    #[error("WebSocket write failed: {0}")]
    Transport(Arc<E>),
    #[error("WebSocket write ended with local status {0:?}")]
    Interrupted(WriteProgress),
    #[error("WebSocket output is closing")]
    Closing,
    #[error("WebSocket retirement did not finish cleanly")]
    RetirementFailed,
    #[error("frame b