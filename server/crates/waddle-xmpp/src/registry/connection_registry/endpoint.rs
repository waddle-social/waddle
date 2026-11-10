//! Opaque outbound queue ownership and per-binding admission.

use super::actor_outbound::{ActorOutboundCustody, ActorOutboundId, ActorOutboundReceiver};
use super::outbound::LocalConnectionBindingId;
use super::{mpsc, OutboundStanza};
use crate::ownership::{BindingRef, ConnectionRef};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct OutboundQueue {
    sender: QueueSender,
    receiver_identity: Option<Arc<()>>,
}

#[derive(Debug)]
enum QueueSender {
    Legacy(mpsc::Sender<OutboundStanza>),
    Actor {
        sender: mpsc::Sender<ActorOutboundId>,
        custody: Arc<ActorOutboundCustody>,
    },
}

/// Admission state for one published socket binding.
///
/// Production registration constructors create [`Self::Active`] entries.
/// The provisional constructor and exact-binding transitions are a dark
/// foundation for a later lifecycle cutover. Registry replacement does not
/// currently seal existing Active handles, so this is not a replacement-
/// sealing or final admission guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingAdmissionState {
    Provisional,
    Active,
    Sealed,
}

/// Why an endpoint proved that a frame was not enqueued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::registry) enum EndpointEnqueueRejection {
    Provisional,
    Sealed,
    Full,
    ChannelClosed,
    BindingMismatch,
}

/// Result of using the opaque outbound sink. Only a completed pre-enqueue
/// refusal returns the original envelope; acceptance is not a delivery proof.
#[derive(Debug)]
pub(super) enum EndpointEnqueueOutcome {
    AcceptedOrUnknown,
    ProvenNotEnqueued {
        reason: EndpointEnqueueRejection,
        outbound: Box<OutboundStanza>,
    },
}

impl EndpointEnqueueOutcome {
    fn refused(reason: EndpointEnqueueRejection, outbound: OutboundStanza) -> Self {
        Self::ProvenNotEnqueued {
            reason,
            outbound: Box::new(outbound),
        }
    }
}

/// Owns the sending half of one socket's outbound queue.
///
/// The socket task creates this together with the receiver using
/// [`Self::channel`]. It is consumed by a connection entry and never exposes
/// the underlying Tokio sender. `From<mpsc::Sender<_>>` exists only in test
/// builds so existing channel fixtures can exercise the production sink.
#[derive(Debug)]
pub struct SocketOutboundEndpoint {
    queue: Arc<OutboundQueue>,
}

/// The sole receiving half of a socket queue, paired with its endpoint.
///
/// While this wrapper exists, callers cannot replace the receiver used to
/// prove that an exact sealed endpoint has no accepted queued frames.
pub struct SocketOutboundReceiver {
    receiver: mpsc::Receiver<OutboundStanza>,
    identity: Arc<()>,
}

impl SocketOutboundReceiver {
    /// Receive one outbound envelope on the connection's owning task.
    pub async fn recv(&mut self) -> Option<OutboundStanza> {
        self.receiver.recv().await
    }

    /// Give shutdown cleanup the receiver after live registration has ended.
    pub fn into_receiver(self) -> mpsc::Receiver<OutboundStanza> {
        self.receiver
    }
}

/// Only this opaque endpoint can enter actor-owned provisional publication.
/// It has no conversion into the legacy Active entry constructors.
#[derive(Debug)]
pub struct ActorSocketOutboundEndpoint(SocketOutboundEndpoint);

impl ActorSocketOutboundEndpoint {
    pub fn channel(
        connection: ConnectionRef,
        max_count: usize,
        max_payload_bytes: usize,
    ) -> Option<(Self, ActorOutboundReceiver, Arc<ActorOutboundCustody>)> {
        let (endpoint, receiver, custody) =
            SocketOutboundEndpoint::actor_channel(connection, max_count, max_payload_bytes)?;
        Some((Self(endpoint), receiver, custody))
    }

    pub(super) fn into_inner(self) -> SocketOutboundEndpoint {
        self.0
    }
}

impl SocketOutboundEndpoint {
    pub(super) fn actor_custody(&self) -> Option<Arc<ActorOutboundCustody>> {
        match &self.queue.sender {
            QueueSender::Actor { custody, .. } => Some(Arc::clone(custody)),
            QueueSender::Legacy(_) => None,
        }
    }

    /// Create a bounded socket queue without exposing its sending half.
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<OutboundStanza>) {
        let (sender, receiver) = mpsc::channel(capacity);
        (
            Self {
                queue: Arc::new(OutboundQueue {
                    sender: QueueSender::Legacy(sender),
                    receiver_identity: None,
                }),
            },
            receiver,
        )
    }

    /// Create a queue whose sole receiver can prove its identity and emptiness
    /// to the socket owner during a sealed-binding handback.
    pub fn channel_with_socket_receiver(capacity: usize) -> (Self, SocketOutboundReceiver) {
        let (sender, receiver) = mpsc::channel(capacity);
        let identity = Arc::new(());
        (
            Self {
                queue: Arc::new(OutboundQueue {
                    sender: QueueSender::Legacy(sender),
                    receiver_identity: Some(identity.clone()),
                }),
            },
            SocketOutboundReceiver { receiver, identity },
        )
    }

    /// An inactive actor endpoint keeps full envelopes in process-local
    /// custody and places only exact IDs in the bounded receiver channel.
    /// The caller must establish an independent cleanup owner before exposing
    /// this endpoint to any publication path.
    fn actor_channel(
        connection: ConnectionRef,
        max_count: usize,
        max_payload_bytes: usize,
    ) -> Option<(Self, ActorOutboundReceiver, Arc<ActorOutboundCustody>)> {
        let custody = ActorOutboundCustody::new(connection, max_count, max_payload_bytes)?;
        let (sender, receiver) = mpsc::channel(max_count);
        let endpoint = Self {
            queue: Arc::new(OutboundQueue {
                sender: QueueSender::Actor {
                    sender,
                    custody: custody.clone(),
                },
                receiver_identity: None,
            }),
        };
        Some((
            endpoint,
            ActorOutboundReceiver::new(receiver, custody.clone()),
            custody,
        ))
    }

    pub(super) fn bind(
        self,
        owner: LocalConnectionBindingId,
        binding_ref: Option<BindingRef>,
        state: BindingAdmissionState,
    ) -> BindingOutboundEndpoint {
        BindingOutboundEndpoint {
            queue: self.queue,
            admission: Arc::new(std::sync::Mutex::new(BindingAdmission {
                owner,
                binding_ref,
                state,
            })),
            sealed: CancellationToken::new(),
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl From<mpsc::Sender<OutboundStanza>> for SocketOutboundEndpoint {
    fn from(sender: mpsc::Sender<OutboundStanza>) -> Self {
        Self {
            queue: Arc::new(OutboundQueue {
                sender: QueueSender::Legacy(sender),
                receiver_identity: None,
            }),
        }
    }
}

#[derive(Debug)]
pub(super) struct BindingAdmission {
    owner: LocalConnectionBindingId,
    binding_ref: Option<BindingRef>,
    state: BindingAdmissionState,
}

impl BindingAdmission {
    pub(super) fn matches(
        &self,
        owner: LocalConnectionBindingId,
        binding_ref: Option<&BindingRef>,
    ) -> bool {
        self.owner == owner && self.binding_ref.as_ref() == binding_ref
    }

    #[cfg(any(test, feature = "test-utils"))]
    fn activate(&mut self, owner: LocalConnectionBindingId, binding_ref: &BindingRef) -> bool {
        if !self.matches(owner, Some(binding_ref)) {
            return false;
        }
        match self.state {
            BindingAdmissionState::Provisional | BindingAdmissionState::Active => {
                self.state = BindingAdmissionState::Active;
                true
            }
            BindingAdmissionState::Sealed => false,
        }
    }

    fn seal(&mut self, owner: LocalConnectionBindingId, binding_ref: &BindingRef) -> bool {
        if !self.matches(owner, Some(binding_ref)) {
            return false;
        }
        self.state = BindingAdmissionState::Sealed;
        true
    }

    fn rejection(&self) -> Option<EndpointEnqueueRejection> {
        match self.state {
            BindingAdmissionState::Provisional => Some(EndpointEnqueueRejection::Provisional),
            BindingAdmissionState::Active => None,
            BindingAdmissionState::Sealed => Some(EndpointEnqueueRejection::Sealed),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct BindingOutboundEndpoint {
    queue: Arc<OutboundQueue>,
    pub(super) admission: Arc<std::sync::Mutex<BindingAdmission>>,
    sealed: CancellationToken,
}

impl BindingOutboundEndpoint {
    pub(super) fn try_send(&self, outbound: OutboundStanza) -> EndpointEnqueueOutcome {
        match self.admit() {
            Ok(sink) => sink.try_send(outbound),
            Err(reason) => EndpointEnqueueOutcome::refused(reason, outbound),
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(super) fn activate(
        &self,
        owner: LocalConnectionBindingId,
        binding_ref: &BindingRef,
    ) -> bool {
        self.admission
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .activate(owner, binding_ref)
    }

    pub(super) fn seal(&self, owner: LocalConnectionBindingId, binding_ref: &BindingRef) -> bool {
        let sealed = self
            .admission
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .seal(owner, binding_ref);
        if sealed {
            if let QueueSender::Actor { custody, .. } = &self.queue.sender {
                custody.seal();
            }
        }
        sealed
    }

    pub(super) fn pending_flush_seal_signal(&self) -> CancellationToken {
        self.sealed.clone()
    }

    /// Recover a sealed socket queue only after proving it contains no frames.
    /// Without the paired receiver, full capacity is the conservative proof.
    pub(super) fn into_socket_endpoint_if_empty(
        self,
        owner: LocalConnectionBindingId,
        binding_ref: &BindingRef,
        receiver: Option<&SocketOutboundReceiver>,
    ) -> Result<SocketOutboundEndpoint, Self> {
        let sealed_for_binding = {
            let admission = self
                .admission
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            admission.matches(owner, Some(binding_ref))
                && admission.state == BindingAdmissionState::Sealed
        };
        let queue_empty = match &self.queue.sender {
            QueueSender::Legacy(sender) => {
                !sender.is_closed()
                    && match receiver {
                        Some(receiver) => {
                            self.queue
                                .receiver_identity
                                .as_ref()
                                .is_some_and(|identity| Arc::ptr_eq(identity, &receiver.identity))
                                && !receiver.receiver.is_closed()
                                && receiver.receiver.is_empty()
                        }
                        None => sender.capacity() == sender.max_capacity(),
                    }
            }
            // A sealed actor custody table cannot be reopened for another
            // binding. The physical owner creates a fresh actor channel.
            QueueSender::Actor { .. } => false,
        };
        if !sealed_for_binding || !queue_empty {
            return Err(self);
        }

        Ok(SocketOutboundEndpoint { queue: self.queue })
    }

    /// Cancellation while waiting for capacity drops the owned envelope. A
    /// seal does not wake a capacity waiter; commit rechecks admission once
    /// capacity becomes available.
    pub(super) async fn send(&self, outbound: OutboundStanza) -> EndpointEnqueueOutcome {
        match self.reserve_owned().await {
            Ok(permit) => self.commit_reserved(permit, outbound),
            Err(reason) => EndpointEnqueueOutcome::refused(reason, outbound),
        }
    }

    pub(super) async fn reserve_owned(&self) -> Result<EndpointPermit, EndpointEnqueueRejection> {
        self.check_active()?;
        let permit = match &self.queue.sender {
            QueueSender::Legacy(sender) => EndpointQueuePermit::Legacy(
                sender
                    .clone()
                    .reserve_owned()
                    .await
                    .map_err(|_| EndpointEnqueueRejection::ChannelClosed)?,
            ),
            QueueSender::Actor { sender, .. } => EndpointQueuePermit::Actor(
                sender
                    .clone()
                    .reserve_owned()
                    .await
                    .map_err(|_| EndpointEnqueueRejection::ChannelClosed)?,
            ),
        };
        Ok(EndpointPermit {
            permit,
            endpoint: self.clone(),
        })
    }

    /// Pending flushes may abandon capacity waits before a durable offer. The
    /// sticky signal belongs to this binding, not the underlying socket.
    pub(super) async fn reserve_pending_flush(
        &self,
    ) -> Result<EndpointPermit, EndpointEnqueueRejection> {
        self.check_active()?;
        self.reserve_pending_flush_after_check().await
    }

    async fn reserve_pending_flush_after_check(
        &self,
    ) -> Result<EndpointPermit, EndpointEnqueueRejection> {
        tokio::select! {
            biased;
            _ = self.sealed.cancelled() => Err(EndpointEnqueueRejection::Sealed),
            result = self.reserve_owned() => {
                let permit = result?;
                // A seal may have won after capacity became available but
                // before its deferred notification ran. Drop any permit on
                // refusal; commit makes the final admission decision later.
                self.check_active()?;
                Ok(permit)
            }
        }
    }

    pub(super) fn try_reserve_owned(&self) -> Result<EndpointPermit, EndpointEnqueueRejection> {
        self.check_active()?;
        let permit = match &self.queue.sender {
            QueueSender::Legacy(sender) => EndpointQueuePermit::Legacy(
                sender
                    .clone()
                    .try_reserve_owned()
                    .map_err(map_reserve_error)?,
            ),
            QueueSender::Actor { sender, .. } => EndpointQueuePermit::Actor(
                sender
                    .clone()
                    .try_reserve_owned()
                    .map_err(map_reserve_error)?,
            ),
        };
        Ok(EndpointPermit {
            permit,
            endpoint: self.clone(),
        })
    }

    pub(super) fn try_reserve_pending_flush(
        &self,
    ) -> Result<EndpointPermit, EndpointEnqueueRejection> {
        let permit = self.try_reserve_owned()?;
        self.check_active()?;
        Ok(permit)
    }

    pub(super) fn commit_reserved(
        &self,
        permit: EndpointPermit,
        outbound: OutboundStanza,
    ) -> EndpointEnqueueOutcome {
        if !self.same_binding(&permit.endpoint) {
            return EndpointEnqueueOutcome::refused(
                EndpointEnqueueRejection::BindingMismatch,
                outbound,
            );
        }
        let sink = match self.admit() {
            Ok(sink) => sink,
            Err(reason) => return EndpointEnqueueOutcome::refused(reason, outbound),
        };
        if sink.queue.is_closed() {
            return EndpointEnqueueOutcome::refused(
                EndpointEnqueueRejection::ChannelClosed,
                outbound,
            );
        }
        // The receiver may close between this check and send. A consumed
        // permit is therefore acceptance or unknown, never a retry proof.
        let outcome = match (permit.permit, &sink.queue.sender) {
            (EndpointQueuePermit::Legacy(permit), QueueSender::Legacy(_)) => {
                permit.send(outbound);
                EndpointEnqueueOutcome::AcceptedOrUnknown
            }
            (EndpointQueuePermit::Actor(permit), QueueSender::Actor { custody, .. }) => {
                let Some(binding) = sink._admission.binding_ref.as_ref() else {
                    return EndpointEnqueueOutcome::refused(
                        EndpointEnqueueRejection::BindingMismatch,
                        outbound,
                    );
                };
                if binding.connection_ref() != custody.connection() {
                    return EndpointEnqueueOutcome::refused(
                        EndpointEnqueueRejection::BindingMismatch,
                        outbound,
                    );
                }
                match custody.insert(sink._admission.owner, binding, outbound) {
                    Ok(id) => {
                        permit.send(id);
                        EndpointEnqueueOutcome::AcceptedOrUnknown
                    }
                    Err(outbound) => {
                        EndpointEnqueueOutcome::refused(EndpointEnqueueRejection::Full, outbound)
                    }
                }
            }
            (_, _) => unreachable!("permit and queue type are tied to the same endpoint"),
        };
        drop(sink);
        outcome
    }

    /// Hold binding admission while a sink operation and any associated
    /// synchronous bookkeeping are performed. Callers that also take the
    /// archive frontier lock must use this first, establishing admission →
    /// archive lock order. The guard never exposes or clones the raw sender.
    pub(super) fn with_active<T>(
        &self,
        operation: impl FnOnce(AdmittedOutboundEndpoint<'_>) -> T,
    ) -> Result<T, EndpointEnqueueRejection> {
        self.admit().map(operation)
    }

    /// Hold binding admission while an operation owns an outbound envelope.
    /// If admission refuses before the closure runs, return that same envelope
    /// to the caller instead of dropping the closure and its captured value.
    pub(super) fn with_active_outbound<T, E>(
        &self,
        outbound: OutboundStanza,
        refused: impl FnOnce(EndpointEnqueueRejection, OutboundStanza) -> E,
        operation: impl FnOnce(AdmittedOutboundEndpoint<'_>, OutboundStanza) -> Result<T, E>,
    ) -> Result<T, E> {
        let admitted = match self.admit() {
            Ok(admitted) => admitted,
            Err(reason) => return Err(refused(reason, outbound)),
        };
        operation(admitted, outbound)
    }

    fn admit(&self) -> Result<AdmittedOutboundEndpoint<'_>, EndpointEnqueueRejection> {
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if let Some(rejection) = admission.rejection() {
            return Err(rejection);
        }
        Ok(AdmittedOutboundEndpoint {
            queue: &self.queue,
            _admission: admission,
        })
    }

    pub(super) fn check_active(&self) -> Result<(), EndpointEnqueueRejection> {
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        admission.rejection().map_or(Ok(()), Err)
    }

    pub(super) fn is_closed(&self) -> bool {
        self.queue.is_closed()
    }

    pub(super) fn same_binding(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.queue, &other.queue) && Arc::ptr_eq(&self.admission, &other.admission)
    }

    pub(super) fn is_actor_owned(&self) -> bool {
        matches!(&self.queue.sender, QueueSender::Actor { .. })
    }

    pub(super) fn actor_custody(&self) -> Option<Arc<ActorOutboundCustody>> {
        match &self.queue.sender {
            QueueSender::Actor { custody, .. } => Some(Arc::clone(custody)),
            QueueSender::Legacy(_) => None,
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(super) fn is_provisional(&self) -> bool {
        self.admission
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .state
            == BindingAdmissionState::Provisional
    }

    pub(super) fn seal_if_actor(&self, owner: LocalConnectionBindingId, binding_ref: &BindingRef) {
        if self.is_actor_owned() {
            let _ = self.seal(owner, binding_ref);
        }
    }

    pub(super) fn matches_owner(
        &self,
        owner: LocalConnectionBindingId,
        binding_ref: Option<&BindingRef>,
    ) -> bool {
        self.admission
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .matches(owner, binding_ref)
    }

    #[cfg(feature = "test-utils")]
    pub(super) fn capacity(&self) -> usize {
        self.queue.capacity()
    }

    #[cfg(feature = "test-utils")]
    pub(super) fn max_capacity(&self) -> usize {
        self.queue.max_capacity()
    }
}

pub(super) struct AdmittedOutboundEndpoint<'a> {
    queue: &'a OutboundQueue,
    _admission: std::sync::MutexGuard<'a, BindingAdmission>,
}

impl AdmittedOutboundEndpoint<'_> {
    pub(super) fn try_send(&self, outbound: OutboundStanza) -> EndpointEnqueueOutcome {
        match &self.queue.sender {
            QueueSender::Legacy(sender) => match sender.try_send(outbound) {
                Ok(()) => EndpointEnqueueOutcome::AcceptedOrUnknown,
                Err(mpsc::error::TrySendError::Full(outbound)) => {
                    EndpointEnqueueOutcome::refused(EndpointEnqueueRejection::Full, outbound)
                }
                Err(mpsc::error::TrySendError::Closed(outbound)) => {
                    EndpointEnqueueOutcome::refused(
                        EndpointEnqueueRejection::ChannelClosed,
                        outbound,
                    )
                }
            },
            QueueSender::Actor { sender, custody } => {
                let permit = match sender.clone().try_reserve_owned() {
                    Ok(permit) => permit,
                    Err(error) => {
                        return EndpointEnqueueOutcome::refused(map_reserve_error(error), outbound)
                    }
                };
                let Some(binding) = self._admission.binding_ref.as_ref() else {
                    return EndpointEnqueueOutcome::refused(
                        EndpointEnqueueRejection::BindingMismatch,
                        outbound,
                    );
                };
                if binding.connection_ref() != custody.connection() {
                    return EndpointEnqueueOutcome::refused(
                        EndpointEnqueueRejection::BindingMismatch,
                        outbound,
                    );
                }
                match custody.insert(self._admission.owner, binding, outbound) {
                    Ok(id) => {
                        permit.send(id);
                        EndpointEnqueueOutcome::AcceptedOrUnknown
                    }
                    Err(outbound) => {
                        EndpointEnqueueOutcome::refused(EndpointEnqueueRejection::Full, outbound)
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct EndpointPermit {
    permit: EndpointQueuePermit,
    endpoint: BindingOutboundEndpoint,
}

#[derive(Debug)]
enum EndpointQueuePermit {
    Legacy(mpsc::OwnedPermit<OutboundStanza>),
    Actor(mpsc::OwnedPermit<ActorOutboundId>),
}

fn map_reserve_error<T>(error: mpsc::error::TrySendError<T>) -> EndpointEnqueueRejection {
    match error {
        mpsc::error::TrySendError::Full(_) => EndpointEnqueueRejection::Full,
        mpsc::error::TrySendError::Closed(_) => EndpointEnqueueRejection::ChannelClosed,
    }
}

impl OutboundQueue {
    fn is_closed(&self) -> bool {
        match &self.sender {
            QueueSender::Legacy(sender) => sender.is_closed(),
            QueueSender::Actor { sender, .. } => sender.is_closed(),
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    fn capacity(&self) -> usize {
        match &self.sender {
            QueueSender::Legacy(sender) => sender.capacity(),
            QueueSender::Actor { sender, .. } => sender.capacity(),
        }
    }

    #[cfg(feature = "test-utils")]
    fn max_capacity(&self) -> usize {
        match &self.sender {
            QueueSender::Legacy(sender) => sender.max_capacity(),
            QueueSender::Actor { sender, .. } => sender.max_capacity(),
        }
    }
}

#[cfg(test)]
mod tests;
