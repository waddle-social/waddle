//! Opaque outbound queue ownership and per-binding admission.

use super::outbound::LocalConnectionBindingId;
use super::{mpsc, OutboundStanza};
use crate::ownership::BindingRef;
use std::sync::Arc;

#[derive(Debug)]
struct OutboundQueue {
    sender: mpsc::Sender<OutboundStanza>,
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

impl SocketOutboundEndpoint {
    /// Create a bounded socket queue without exposing its sending half.
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<OutboundStanza>) {
        let (sender, receiver) = mpsc::channel(capacity);
        (
            Self {
                queue: Arc::new(OutboundQueue { sender }),
            },
            receiver,
        )
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
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl From<mpsc::Sender<OutboundStanza>> for SocketOutboundEndpoint {
    fn from(sender: mpsc::Sender<OutboundStanza>) -> Self {
        Self {
            queue: Arc::new(OutboundQueue { sender }),
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
}

impl BindingOutboundEndpoint {
    pub(super) fn try_send(&self, outbound: OutboundStanza) -> EndpointEnqueueOutcome {
        match self.admit() {
            Ok(sink) => sink.try_send(outbound),
            Err(reason) => EndpointEnqueueOutcome::refused(reason, outbound),
        }
    }

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
        self.admission
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .seal(owner, binding_ref)
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
        match self.queue.sender.clone().reserve_owned().await {
            Ok(permit) => Ok(EndpointPermit {
                permit,
                endpoint: self.clone(),
            }),
            Err(_) => Err(EndpointEnqueueRejection::ChannelClosed),
        }
    }

    pub(super) fn try_reserve_owned(&self) -> Result<EndpointPermit, EndpointEnqueueRejection> {
        self.check_active()?;
        match self.queue.sender.clone().try_reserve_owned() {
            Ok(permit) => Ok(EndpointPermit {
                permit,
                endpoint: self.clone(),
            }),
            Err(mpsc::error::TrySendError::Full(_)) => Err(EndpointEnqueueRejection::Full),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(EndpointEnqueueRejection::ChannelClosed)
            }
        }
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
        if sink.queue.sender.is_closed() {
            return EndpointEnqueueOutcome::refused(
                EndpointEnqueueRejection::ChannelClosed,
                outbound,
            );
        }
        // The receiver may close between this check and send. A consumed
        // permit is therefore acceptance or unknown, never a retry proof.
        permit.permit.send(outbound);
        drop(sink);
        EndpointEnqueueOutcome::AcceptedOrUnknown
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

    fn check_active(&self) -> Result<(), EndpointEnqueueRejection> {
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        admission.rejection().map_or(Ok(()), Err)
    }

    pub(super) fn is_closed(&self) -> bool {
        self.queue.sender.is_closed()
    }

    pub(super) fn same_binding(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.queue, &other.queue) && Arc::ptr_eq(&self.admission, &other.admission)
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
        self.queue.sender.capacity()
    }

    #[cfg(feature = "test-utils")]
    pub(super) fn max_capacity(&self) -> usize {
        self.queue.sender.max_capacity()
    }
}

pub(super) struct AdmittedOutboundEndpoint<'a> {
    queue: &'a OutboundQueue,
    _admission: std::sync::MutexGuard<'a, BindingAdmission>,
}

impl AdmittedOutboundEndpoint<'_> {
    pub(super) fn try_send(&self, outbound: OutboundStanza) -> EndpointEnqueueOutcome {
        match self.queue.sender.try_send(outbound) {
            Ok(()) => EndpointEnqueueOutcome::AcceptedOrUnknown,
            Err(mpsc::error::TrySendError::Full(outbound)) => {
                EndpointEnqueueOutcome::refused(EndpointEnqueueRejection::Full, outbound)
            }
            Err(mpsc::error::TrySendError::Closed(outbound)) => {
                EndpointEnqueueOutcome::refused(EndpointEnqueueRejection::ChannelClosed, outbound)
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct EndpointPermit {
    permit: mpsc::OwnedPermit<OutboundStanza>,
    endpoint: BindingOutboundEndpoint,
}

#[cfg(test)]
mod tests;
