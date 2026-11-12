//! The retirement protocol of one hosted connection.
//!
//! A connection retires exactly once, for one typed cause. The first cause
//! wins and every later request reports it without acting. Retirement is
//! fenced on the connection's [`ConnectionRef`]: once a generation has
//! retired, an ordinary socket write or a late effect that names it is
//! refused with a typed error, and nothing reopens it. A client that comes
//! back is admitted as a new connection, with a new admission permit and a
//! new `ConnectionRef`, and authenticates again.
//!
//! Retirement has two halves. The live loop's last turn latches the cause and
//! writes whatever close the cause still owes the peer. Settlement then drops
//! both socket halves, runs the connection-owned cleanup and, for a transfer
//! to another binding on this node, waits for the pumps to give up their
//! halves before anything acknowledges the transfer.

use crate::clustering::{CriticalNodeFailure, NodeLifecycle};
use crate::websocket_transport::StopReason;
use std::sync::OnceLock;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::ConnectionRef;
use waddle_xmpp::registry::{LocalIoRelinquishment, LocalIoRelinquishmentOutcome};
use waddle_xmpp::telemetry::attributes::ActorRetirementCause;

/// How long a same-node transfer waits, from the moment the retiring owner
/// drops its socket halves, for the pumps to drop theirs.
///
/// The pumps are stopped by cancellation when the owner's halves drop, so a
/// healthy pump gives up its half at once and the wait overlaps the cleanup
/// that follows. The bound comes from the callers that wait for the
/// acknowledgement: the shortest of them gives a local force-detach two
/// seconds, stale-actor retirement four, and a replacing bind or a relayed
/// resume ten. An acknowledgement later than the shortest budget cannot
/// complete any transfer, it can only delay the terminal fence past the point
/// where a waiter has given up and may act on its own. Half the shortest
/// budget leaves the other half for cleanup's own persistence and for the
/// acknowledgement to arrive.
pub(in crate::server::routes::websocket) const TRANSFER_JOIN_DEADLINE: Duration =
    Duration::from_secs(1);

/// Why a hosted connection retired. Closed: a new way to end a connection
/// needs a variant here, so every retirement has a typed, countable cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::routes::websocket) enum RetirementCause {
    /// The peer closed the WebSocket.
    PeerClose,
    /// A protocol handler ended the XMPP stream: the RFC 7395 close
    /// handshake, a stream error, or a failed session initialisation. The
    /// handlers do not tell these apart for the loop.
    StreamClosed,
    /// The process is stopping.
    Shutdown,
    /// The node identity that admitted the connection is no longer current.
    IdentityRevoked,
    /// The node lifecycle generation that admitted the connection ended.
    PermitRevoked,
    /// An identity-matched force-detach request displaced the binding.
    ForceDetach,
    /// A newer binding of the same full JID replaced this one in the
    /// registry.
    Replaced,
    /// A socket write failed or was refused outside any revocation.
    WriteFailure,
    /// A transport pump ended for a reason other than a peer close or a
    /// failed write: a read error, an overloaded or closed queue, a pump that
    /// returned on its own, or the custodian's retirement deadline.
    PumpExit,
    /// The pumps were stopped because the node lost its lease.
    LeaseLost,
    /// The upgrade callback's waiter went away.
    OwnerCancelled,
    /// The peer stopped answering: the keepalive policy or the send-window
    /// pause deadline gave up on it.
    PeerUnresponsive,
    /// A stanza handler was cancelled at its deadline before the server took
    /// responsibility for the stanza, so the stream cannot continue without
    /// acknowledging a stanza it did not handle.
    DispatchTimeout,
    /// The hosting actor panicked or was killed. Nothing settled.
    OwnerPanicked,
}

impl RetirementCause {
    /// The cause a pump stop reports to the loop.
    pub(in crate::server::routes::websocket) fn from_pump_stop(reason: StopReason) -> Self {
        match reason {
            StopReason::PeerClosed => Self::PeerClose,
            StopReason::OutboundIoFailure => Self::WriteFailure,
            StopReason::LeaseLost => Self::LeaseLost,
            StopReason::Requested
            | StopReason::InboundOverload
            | StopReason::OutboundOverload
            | StopReason::InboundQueueClosed
            | StopReason::OutboundQueueClosed
            | StopReason::InboundIoFailure
            | StopReason::PumpExited
            | StopReason::ShutdownDeadline => Self::PumpExit,
        }
    }

    /// Whether this cause hands the full JID to another binding on this
    /// node, which may start writing as soon as the transfer is acknowledged.
    pub(in crate::server::routes::websocket) fn is_transfer(self) -> bool {
        matches!(self, Self::ForceDetach | Self::Replaced)
    }

    /// The bounded metric attribute for this cause.
    pub(in crate::server::routes::websocket) fn metric(self) -> ActorRetirementCause {
        match self {
            Self::PeerClose | Self::StreamClosed => ActorRetirementCause::ClientClose,
            Self::Shutdown | Self::IdentityRevoked | Self::PermitRevoked | Self::LeaseLost => {
                ActorRetirementCause::ServerShutdown
            }
            Self::ForceDetach | Self::Replaced => ActorRetirementCause::Replaced,
            Self::WriteFailure | Self::PumpExit => ActorRetirementCause::TransportError,
            Self::OwnerCancelled => ActorRetirementCause::Abandoned,
            Self::PeerUnresponsive | Self::DispatchTimeout => ActorRetirementCause::Deadline,
            Self::OwnerPanicked => ActorRetirementCause::Panic,
        }
    }
}

/// The close a retirement still owes the peer when the loop hands it over.
/// A handler that ended the stream has already written its own close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OwedClose {
    /// Nothing is owed: the peer is gone, the handler closed the stream, or
    /// the cause has no close of its own.
    None,
    /// The system-shutdown stream error and framing close on a committed
    /// stream, then the WebSocket close.
    NodeUnavailable,
    /// The bounded conflict close of a displaced binding.
    Conflict,
    /// The WebSocket close alone.
    Transport,
}

/// What ends the live loop: the cause, and the close it still owes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Retire {
    pub(super) cause: RetirementCause,
    pub(super) close: OwedClose,
}

impl Retire {
    pub(super) fn settled(cause: RetirementCause) -> Self {
        Self {
            cause,
            close: OwedClose::None,
        }
    }

    pub(super) fn owing(cause: RetirementCause, close: OwedClose) -> Self {
        Self { cause, close }
    }
}

/// Why the fence refused an effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(in crate::server::routes::websocket) enum RetirementRefusal {
    /// The named generation has retired. The effect arrived too late.
    #[error("connection generation retired ({0:?})")]
    Retired(RetirementCause),
    /// The effect names a different physical connection than this fence.
    #[error("effect names a different connection generation")]
    WrongGeneration,
}

/// Whether a retirement request was the one that retired the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::routes::websocket) enum RetirementEntry {
    /// This request retired the connection.
    First(RetirementCause),
    /// The connection had already retired for the carried cause.
    Already(RetirementCause),
}

/// The once-only retirement latch of one physical connection, shared by its
/// live state, its write sink and the task that hosts the actor.
#[derive(Debug)]
pub(in crate::server::routes::websocket) struct RetirementFence {
    connection_ref: ConnectionRef,
    cause: OnceLock<RetirementCause>,
    retired: CancellationToken,
}

impl RetirementFence {
    pub(in crate::server::routes::websocket) fn new(connection_ref: ConnectionRef) -> Self {
        Self {
            connection_ref,
            cause: OnceLock::new(),
            retired: CancellationToken::new(),
        }
    }

    /// Retire the connection for `cause`, unless it has retired already.
    pub(in crate::server::routes::websocket) fn retire(
        &self,
        cause: RetirementCause,
    ) -> RetirementEntry {
        let mut first = false;
        let recorded = *self.cause.get_or_init(|| {
            first = true;
            cause
        });
        if first {
            self.retired.cancel();
            RetirementEntry::First(recorded)
        } else {
            RetirementEntry::Already(recorded)
        }
    }

    /// The cause the connection retired for, once it has.
    pub(in crate::server::routes::websocket) fn cause(&self) -> Option<RetirementCause> {
        self.cause.get().copied()
    }

    /// Admit an effect that names `generation`, or say why it is refused.
    pub(in crate::server::routes::websocket) fn admit(
        &self,
        generation: &ConnectionRef,
    ) -> Result<(), RetirementRefusal> {
        if generation != &self.connection_ref {
            return Err(RetirementRefusal::WrongGeneration);
        }
        match self.cause() {
            Some(cause) => Err(RetirementRefusal::Retired(cause)),
            None => Ok(()),
        }
    }
}

/// Whether the pumps were seen to give up the socket before a deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::routes::websocket) enum PumpTermination {
    /// Both pump-owned halves were dropped: the old socket cannot be written.
    Observed,
    /// The deadline passed, or the observation was lost, first.
    Missed,
    /// The connection has no pump witness: its owner holds the halves itself
    /// and dropped them before settlement began.
    NotApplicable,
}

/// Wait until `deadline` for the pumps to drop their halves of the socket.
pub(in crate::server::routes::websocket) async fn observe_pump_termination(
    witness: Option<&LocalIoRelinquishment>,
    deadline: tokio::time::Instant,
) -> PumpTermination {
    let Some(witness) = witness else {
        return PumpTermination::NotApplicable;
    };
    match tokio::time::timeout_at(deadline, witness.wait()).await {
        Ok(LocalIoRelinquishmentOutcome::Relinquished) => PumpTermination::Observed,
        Ok(LocalIoRelinquishmentOutcome::Unobserved) | Err(_) => PumpTermination::Missed,
    }
}

/// Take the node's terminal failure path because a transferred binding's
/// pumps were not seen to stop.
///
/// The recoverable fence would mint a new serving generation and admit
/// connections again while the unobserved pump could still write to the old
/// socket. A failed node admits nothing more in this process, and its failed
/// generation fences every remaining socket enqueue.
pub(in crate::server::routes::websocket) fn fence_unjoined_transfer(
    lifecycle: &NodeLifecycle,
    stop: &CancellationToken,
) {
    lifecycle.fatal_fence_token().cancel();
    stop.cancel();
    lifecycle.fail(CriticalNodeFailure::WebSocketPumpUnjoined);
}
