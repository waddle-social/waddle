//! Dormant supervised task boundary for the production-compiled connection owner.
//!
//! The task owns `ConnectionActorCore` and its `WsConnState`. Callers submit
//! typed work and observe operation IDs; canceling a waiter does not cancel the
//! handler or release its retained input. This module is not selected by `/ws`.

use super::connection_actor_core::{
    ConnectionActorCore, ConnectionActorDriverCompletion, ConnectionActorDriverError,
};
use super::frame::ResponseBatch;
use super::replay::DriveOutcome;
use super::{OutboundStanza, WebSocketState};
use crate::clustering::NodeAdmissionPermit;
use crate::server::routes::websocket::supervised_io::StopReason;
use jid::BareJid;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::{AbortHandle, JoinHandle};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::ConnectionRef;

const MAX_PENDING_OPERATIONS: usize = 256;
const DEFAULT_CONTROL_CAPACITY: usize = 8;
const DEFAULT_BYTE_CAPACITY: usize = 32 * 1024 * 1024;
const DEFAULT_RESULT_RESERVATION_BYTES: usize = 64 * 1024;

/// Bounded admission for one dark actor task. The byte value is admission
/// accounting: input bytes plus a result reservation. Handler outputs are
/// measured only after effects return; an overrun is retained and seals
/// admission, so this is not a hard memory bound or an activation guarantee.
#[derive(Debug, Clone, Copy)]
pub(super) struct ConnectionActorDriverTaskLimits {
    data_capacity: usize,
    control_capacity: usize,
    byte_capacity: usize,
    result_reservation_bytes: usize,
}

impl Default for ConnectionActorDriverTaskLimits {
    fn default() -> Self {
        Self {
            data_capacity: MAX_PENDING_OPERATIONS,
            control_capacity: DEFAULT_CONTROL_CAPACITY,
            byte_capacity: DEFAULT_BYTE_CAPACITY,
            result_reservation_bytes: DEFAULT_RESULT_RESERVATION_BYTES,
        }
    }
}

impl ConnectionActorDriverTaskLimits {
    #[cfg(test)]
    fn with_bounds(
        data_capacity: usize,
        control_capacity: usize,
        byte_capacity: usize,
        result_reservation_bytes: usize,
    ) -> Self {
        Self {
            data_capacity,
            control_capacity,
            byte_capacity,
            result_reservation_bytes,
        }
    }

    fn validate(self) -> Result<Self, ConnectionActorDriverTaskAdmissionError> {
        if self.data_capacity == 0
            || self.data_capacity > MAX_PENDING_OPERATIONS
            || self.control_capacity == 0
            || self.byte_capacity == 0
            || self.byte_capacity > u32::MAX as usize
            || self.result_reservation_bytes > self.byte_capacity
        {
            return Err(ConnectionActorDriverTaskAdmissionError::InvalidLimits);
        }
        Ok(self)
    }
}

/// Supported typed operation results retained until the caller acknowledges.
#[derive(Debug)]
pub(super) struct ConnectionActorDriverTaskResult {
    pub(super) value: ConnectionActorDriverTaskResultValue,
    _reservation: Arc<DriverAdmissionReservation>,
}

#[derive(Debug)]
pub(super) enum ConnectionActorDriverTaskResultValue {
    InboundFrame(
        Result<ConnectionActorDriverCompletion<ResponseBatch>, ConnectionActorDriverError>,
    ),
    PeerStanza(Result<ConnectionActorDriverCompletion<DriveOutcome>, ConnectionActorDriverError>),
    #[cfg(any(test, feature = "test-support"))]
    Paused(&'static str),
    #[cfg(any(test, feature = "test-support"))]
    Immediate,
    #[cfg(any(test, feature = "test-support"))]
    ImmediateResult(&'static str),
}

/// Why an operation does not have a returned typed handler result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnectionActorDriverTaskUnknownReason {
    WorkerAborted,
    WorkerStopped,
    ResultReservationExceeded,
}

/// Current state of one accepted operation. Accepted inputs stay in the
/// shared record until explicit acknowledgement, incl