//! Owner-serviced resume preflight, including while the owner is blocked on socket I/O.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::task::AtomicWaker;
use jid::{BareJid, FullJid};
use tokio::sync::{mpsc, oneshot};

use super::{ForceDetachOrigin, ForceDetachOutcome, ForceDetachRequest, SocketWriteInterruption};
use crate::ownership::BindingRef;
use crate::pending_delivery::SmSessionId;
use crate::stream_management::{SmResumeCheckpointHandle, SmResumeRejection};

const MAX_LIVE_RESUME_REQUESTS: usize = 8;

/// Result of an owner-validated local takeover request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalResumeReply {
    Detached,
    IdentityMismatch,
    NotPersisted,
    Rejected(SmResumeRejection),
}

#[derive(Debug)]
struct Binding {
    owner: FullJid,
    stream_id: Arc<Mutex<Option<SmSessionId>>>,
    binding_ref: Option<BindingRef>,
    force_detach: mpsc::Sender<ForceDetachRequest>,
}

#[derive(Debug)]
struct RequestPermit(Arc<AtomicUsize>);

impl Drop for RequestPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct PendingResume {
    binding_ref: BindingRef,
    stream_id: SmSessionId,
    requester: BareJid,
    h: u32,
    response: oneshot::Sender<LocalResumeReply>,
    permit: RequestPermit,
}

#[derive(Debug, Default)]
struct Requests {
    closed: bool,
    pending: VecDeque<PendingResume>,
}

/// Requesters enqueue inert requests. Only `poll`, called by the connection
/// owner, validates the current frontier and interrupts the owning transport.
#[derive(Debug)]
pub struct LiveResumeControl {
    checkpoint: SmResumeCheckpointHandle,
    interruption: SocketWriteInterruption,
    binding: Mutex<Option<Binding>>,
    requests: Mutex<Requests>,
    outstanding: Arc<AtomicUsize>,
    waker: AtomicWaker,
}

impl LiveResumeControl {
    pub fn new(
        checkpoint: SmResumeCheckpointHandle,
        interruption: SocketWriteInterruption,
    ) -> Arc<Self> {
        Arc::new(Self {
            checkpoint,
            interruption,
            binding: Mutex::new(None),
            requests: Mutex::new(Requests::default()),
            outstanding: Arc::new(AtomicUsize::new(0)),
            waker: AtomicWaker::new(),
        })
    }

    pub(super) fn bind(
        &self,
        owner: FullJid,
        stream_id: Arc<Mutex<Option<SmSessionId>>>,
        binding_ref: Option<BindingRef>,
        force_detach: mpsc::Sender<ForceDetachRequest>,
    ) -> bool {
        let requests = self
            .requests
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if requests.closed {
            return false;
        }
        *self
            .binding
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = Some(Binding {
            owner,
            stream_id,
            binding_ref,
            force_detach,
        });
        true
    }

    /// Run a synchronous decision only while this control still names the
    /// socket owner's exact full JID and binding attempt. The callback must
    /// not re-enter this control or invoke a receiver wake.
    pub(super) fn with_exact_binding<T>(
        &self,
        owner: &FullJid,
        binding_ref: &BindingRef,
        operation: impl FnOnce() -> T,
    ) -> Option<T> {
        let binding = self
            .binding
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        let current = binding.as_ref()?;
        if &current.owner != owner || current.binding_ref.as_ref() != Some(binding_ref) {
            return None;
        }
        Some(operation())
    }

    /// Serialize a resumable enable's synchronous start with control rebind
    /// and exact-binding revocation. The callback must not await or re-enter
    /// the control, permit, or registry; it acquires the socket gate last.
    pub fn start_sm_enable_if_current<T>(
        &self,
        permit: &super::LocalBindingPermit,
        operation: impl FnOnce() -> T,
    ) -> Option<T> {
        self.with_exact_binding(permit.jid(), permit.binding(), || {
            permit.start_if_current(operation)
        })
        .flatten()
    }

    #[cfg(test)]
    pub(super) fn requests_locked_for_test(&self) -> bool {
        self.requests.try_lock().is_err()
    }

    /// Queue a bounded request for the caller's captured binding without
    /// touching the old connection's I/O. Dropping this future never rolls
    /// back cleanup already accepted by the owner.
    pub async fn request(
        &self,
        binding_ref: BindingRef,
        stream_id: SmSessionId,
        requester: BareJid,
        h: u32,
    ) -> LocalResumeReply {
        if self
            .outstanding
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_LIVE_RESUME_REQUESTS).then_some(count + 1)
            })
            .is_err()
        {
            return LocalResumeReply::NotPersisted;
        }
        let permit = RequestPermit(Arc::clone(&self.outstanding));
        let (response, receiver) = oneshot::channel();
        {
            let mut requests = self
                .requests
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            let binding = self
                .binding
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            if requests.closed
                || binding
                    .as_ref()
                    .is_none_or(|current| current.binding_ref.as_ref() != Some(&binding_ref))
            {
                return LocalResumeReply::NotPersisted;
            }
            requests.pending.push_back(PendingResume {
                binding_ref,
                stream_id,
                requester,
                h,
                response,
                permit,
            });
        }
        self.waker.wake();
        receiver.await.unwrap_or(LocalResumeReply::NotPersisted)
    }

    /// Service preflight on the connection owner's task. Returns ready after
    /// servicing requests so a select loop can poll its force-detach receiver.
    pub fn poll(&self, cx: &mut Context<'_>) -> Poll<()> {
        self.waker.register(cx.waker());
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        let mut serviced = false;
        while let Some(request) = requests.pending.pop_front() {
            serviced = true;
            if request.response.is_closed() {
                continue;
            }
            let binding = self
                .binding
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            let Some(binding) = binding.as_ref() else {
                let _ = request.response.send(LocalResumeReply::NotPersisted);
                continue;
            };
            if binding.binding_ref.as_ref() != Some(&request.binding_ref) {
                let _ = request.response.send(LocalResumeReply::NotPersisted);
                continue;
            }
            if binding.owner.to_bare() != request.requester {
                let _ = request.response.send(LocalResumeReply::IdentityMismatch);
                continue;
            }
            let stream_id = binding
                .stream_id
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            if stream_id.as_ref() != Some(&request.stream_id) {
                let _ = request.response.send(LocalResumeReply::NotPersisted);
                continue;
            }
            if let Err(rejection) = self.checkpoint.validate(request.h) {
                let _ = request.response.send(LocalResumeReply::Rejected(rejection));
                continue;
            }
            let request_binding = request.binding_ref.clone();
            let (ack, receiver) = oneshot::channel();
            if binding
                .force_detach
                .try_send(ForceDetachRequest {
                    origin: ForceDetachOrigin::CrossNodeResume,
                    target_binding: Some(request_binding.clone()),
                    requester_bare_jid: request.requester,
                    ack,
                })
                .is_err()
            {
                let _ = request.response.send(LocalResumeReply::NotPersisted);
                continue;
            }
            // The detach is queued before the stop transition, so the owner can
            // recover the typed request after the interrupted write yields. The
            // queue offer alone does not fence a concurrent ordinary write.
            let _ = self.interruption.cancel_if_binding(&request_binding);
            tokio::spawn(async move {
                let _permit = request.permit;
                let outcome = match receiver.await {
                    Ok(ForceDetachOutcome::Detached) => LocalResumeReply::Detached,
                    Ok(ForceDetachOutcome::IdentityMismatch) => LocalResumeReply::IdentityMismatch,
                    Ok(ForceDetachOutcome::NotPersisted) | Err(_) => LocalResumeReply::NotPersisted,
                };
                let _ = request.response.send(outcome);
            });
        }
        if serviced {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    /// Close preflight after ordinary cleanup. Accepted detach acknowledgments
    /// retain their own lifetime and still report the persistence outcome.
    pub fn finish(&self) {
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        requests.closed = true;
        for request in requests.pending.drain(..) {
            let _ = request.response.send(LocalResumeReply::NotPersisted);
        }
        self.waker.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
    use crate::registry::ConnectionEntry;
    use crate::stream_management::StreamManagementState;
    use crate::telemetry::attributes::SmEvictionPath;
    use futures::poll;
    use futures::task::noop_waker;

    fn fixture() -> (
        StreamManagementState,
        Arc<LiveResumeControl>,
        mpsc::Receiver<ForceDetachRequest>,
        BindingRef,
    ) {
        let mut state = StreamManagementState::new();
        state.enable("live-resume".into(), true, None);
        state.increment_outbound();
        let binding_ref =
            BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()));
        let (sender, _receiver) = mpsc::channel(1);
        let entry = ConnectionEntry::new_for_binding(sender, binding_ref.clone());
        let control =
            LiveResumeControl::new(state.resume_checkpoint(), entry.write_interruption.clone());
        entry.set_sm_stream_id(Some(SmSessionId::new("live-resume")));
        assert!(entry.wire_resume_control(
            "user@example.test/device".parse().expect("jid"),
            Arc::clone(&control)
        ));
        let force_receiver = entry.take_force_detach_rx().expect("owner receiver");
        (state, control, force_receiver, binding_ref)
    }

    fn owner_poll(control: &LiveResumeControl) {
        let waker = noop_waker();
        assert!(control.poll(&mut Context::from_waker(&waker)).is_ready());
    }

    fn owner() -> BareJid {
        "user@example.test".parse().expect("jid")
    }

    #[tokio::test]
    async fn accepted_live_resume_request_carries_its_exact_binding() {
        let mut state = StreamManagementState::new();
        state.enable("live-resume".into(), true, None);
        state.increment_outbound();
        let binding_ref =
            BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()));
        let (sender, _receiver) = mpsc::channel(1);
        let entry = ConnectionEntry::new_for_binding(sender, binding_ref.clone());
        let control =
            LiveResumeControl::new(state.resume_checkpoint(), entry.write_interruption.clone());
        entry.set_sm_stream_id(Some(SmSessionId::new("live-resume")));
        assert!(entry.wire_resume_control(
            "user@example.test/device".parse().expect("jid"),
            Arc::clone(&control),
        ));
        let mut force_receiver = entry.take_force_detach_rx().expect("owner receiver");

        let resume = control.request(
            binding_ref.clone(),
            SmSessionId::new("live-resume"),
            owner(),
            0,
        );
        tokio::pin!(resume);
        assert!(poll!(&mut resume).is_pending());
        owner_poll(&control);

        let detach = force_receiver.try_recv().expect("queued detach");
        assert_eq!(detach.target_binding, Some(binding_ref));
        assert!(control.interruption.is_cancelled());
        assert!(detach.ack.send(ForceDetachOutcome::NotPersisted).is_ok());
        assert_eq!(resume.await, LocalResumeReply::NotPersisted);
    }

    #[tokio::test]
    async fn stale_live_resume_request_does_not_stop_a_successor_binding() {
        let mut state = StreamManagementState::new();
        state.enable("live-resume".into(), true, None);
        state.increment_outbound();
        let connection = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
        let old_binding = BindingRef::fresh(connection.clone());
        let successor = BindingRef::fresh(connection);
        let (sender, _receiver) = mpsc::channel(1);
        let entry = ConnectionEntry::new_for_binding(sender, old_binding.clone());
        let interruption = entry.write_interruption.clone();
        let control = LiveResumeControl::new(state.resume_checkpoint(), interruption.clone());
        entry.set_sm_stream_id(Some(SmSessionId::new("live-resume")));
        assert!(entry.wire_resume_control(
            "user@example.test/device".parse().expect("jid"),
            Arc::clone(&control),
        ));
        let mut force_receiver = entry.take_force_detach_rx().expect("owner receiver");

        let resume = control.request(
            old_binding.clone(),
            SmSessionId::new("live-resume"),
            owner(),
            0,
        );
        tokio::pin!(resume);
        assert!(poll!(&mut resume).is_pending());
        interruption.retire_binding(&old_binding);
        interruption
            .publish_binding(&successor, || ())
            .expect("publish successor");
        let (successor_tx, _successor_rx) = mpsc::channel(1);
        let mut successor_entry = ConnectionEntry::new_for_binding(successor_tx, successor.clone());
        successor_entry.write_interruption = interruption.clone();
        successor_entry.set_sm_stream_id(Some(SmSessionId::new("live-resume")));
        assert!(successor_entry.wire_resume_control(
            "user@example.test/device".parse().expect("jid"),
            Arc::clone(&control),
        ));
        let mut successor_force_receiver = successor_entry
            .take_force_detach_rx()
            .expect("successor owner receiver");
        owner_poll(&control);

        assert!(matches!(
            force_receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            successor_force_receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(!interruption.is_cancelled());
        assert_eq!(resume.await, LocalResumeReply::NotPersisted);
        assert_eq!(
            control
                .request(old_binding, SmSessionId::new("live-resume"), owner(), 0)
                .await,
            LocalResumeReply::NotPersisted
        );
    }

    #[tokio::test]
    async fn live_resume_reset_keeps_checkpoint_and_rebinds_current_entry() {
        let (mut state, control, mut old_force, old_binding) = fixture();
        let stale_request = control.request(
            old_binding.clone(),
            SmSessionId::new("live-resume"),
            owner(),
            0,
        );
        tokio::pin!(stale_request);
        assert!(poll!(&mut stale_request).is_pending());

        state.reset();
        assert_eq!(
            control.checkpoint.validate(0),
            Err(SmResumeRejection::NotResumable)
        );
        state.enable("replacement-stream".into(), true, None);
        state.increment_outbound();
        assert_eq!(control.checkpoint.validate(0), Ok(()));
        assert_eq!(
            control.checkpoint.validate(2),
            Err(SmResumeRejection::HandledTooHigh { outbound_count: 1 })
        );

        let successor = BindingRef::fresh(old_binding.connection_ref().clone());
        control.interruption.retire_binding(&old_binding);
        control
            .interruption
            .publish_binding(&successor, || ())
            .expect("publish successor");
        let (sender, _receiver) = mpsc::channel(1);
        let mut entry = ConnectionEntry::new_for_binding(sender, successor.clone());
        entry.write_interruption = control.interruption.clone();
        entry.set_sm_stream_id(Some(SmSessionId::new("replacement-stream")));
        assert!(entry.wire_resume_control(
            "user@example.test/rebound".parse().expect("jid"),
            Arc::clone(&control),
        ));
        let mut new_force = entry.take_force_detach_rx().expect("new owner receiver");
        owner_poll(&control);
        assert_eq!(stale_request.await, LocalResumeReply::NotPersisted);
        assert!(!control.interruption.is_cancelled());
        assert!(old_force.try_recv().is_err());
        assert!(matches!(
            new_force.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        let request = control.request(
            successor.clone(),
            SmSessionId::new("replacement-stream"),
            owner(),
            1,
        );
        tokio::pin!(request);
        assert!(poll!(&mut request).is_pending());
        owner_poll(&control);
        assert!(control.interruption.is_cancelled());
        let detach = new_force.try_recv().expect("new entry received detach");
        assert!(detach.ack.send(ForceDetachOutcome::Detached).is_ok());
        assert_eq!(request.await, LocalResumeReply::Detached);
        control.finish();
        let (sender, _receiver) = mpsc::channel(1);
        let finished_entry = ConnectionEntry::new(sender);
        assert!(!finished_entry.wire_resume_control(
            "user@example.test/closed".parse().expect("jid"),
            Arc::clone(&control),
        ));
    }

    #[tokio::test]
    async fn live_resume_rejects_current_watermark_before_interrupting() {
        let (mut state, control, mut force, binding_ref) = fixture();
        let request = control.request(
            binding_ref.clone(),
            SmSessionId::new("live-resume"),
            owner(),
            0,
        );
        tokio::pin!(request);
        assert!(poll!(&mut request).is_pending());
        assert!(!control.interruption.is_cancelled(), "enqueue is inert");
        // The owner advances while busy before servicing the queued preflight.
        state.acknowledge(1);
        owner_poll(&control);
        assert_eq!(
            request.await,
            LocalResumeReply::Rejected(SmResumeRejection::ReplayGap {
                inbound_count: 0,
                replay_gap_through: None
            })
        );
        assert!(!control.interruption.is_cancelled());
        assert!(matches!(
            force.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn live_resume_invalid_identity_stream_and_counter_leave_owner_inert() {
        let (_state, control, mut force, binding_ref) = fixture();
        for (stream_id, requester, h, expected) in [
            (
                SmSessionId::new("live-resume"),
                "other@example.test".parse().expect("jid"),
                0,
                LocalResumeReply::IdentityMismatch,
            ),
            (
                SmSessionId::new("other-stream"),
                owner(),
                0,
                LocalResumeReply::NotPersisted,
            ),
            (
                SmSessionId::new("live-resume"),
                owner(),
                2,
                LocalResumeReply::Rejected(SmResumeRejection::HandledTooHigh { outbound_count: 1 }),
            ),
        ] {
            let request = control.request(binding_ref.clone(), stream_id, requester, h);
            tokio::pin!(request);
            assert!(poll!(&mut request).is_pending());
            owner_poll(&control);
            assert_eq!(request.await, expected);
            assert!(!control.interruption.is_cancelled());
            assert!(matches!(
                force.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
        }
    }

    #[tokio::test]
    async fn live_resume_owner_queues_detach_before_interrupt_and_preserves_ack_after_finish() {
        let (_state, control, mut force, binding_ref) = fixture();
        let request = control.request(
            binding_ref.clone(),
            SmSessionId::new("live-resume"),
            owner(),
            0,
        );
        tokio::pin!(request);
        assert!(poll!(&mut request).is_pending());
        assert!(!control.interruption.is_cancelled());
        owner_poll(&control);
        assert!(control.interruption.is_cancelled());
        let detach = force.try_recv().expect("queued before interruption");
        assert_eq!(detach.origin, ForceDetachOrigin::CrossNodeResume);
        assert_eq!(detach.requester_bare_jid, owner());
        control.finish();
        assert!(detach.ack.send(ForceDetachOutcome::Detached).is_ok());
        assert_eq!(request.await, LocalResumeReply::Detached);
        assert_eq!(
            control
                .request(
                    binding_ref.clone(),
                    SmSessionId::new("live-resume"),
                    owner(),
                    0
                )
                .await,
            LocalResumeReply::NotPersisted
        );
    }

    #[tokio::test]
    async fn live_resume_closed_request_is_inert_and_finish_releases_pending() {
        let (_state, control, mut force, binding_ref) = fixture();
        {
            let request = control.request(
                binding_ref.clone(),
                SmSessionId::new("live-resume"),
                owner(),
                0,
            );
            tokio::pin!(request);
            assert!(poll!(&mut request).is_pending());
        }
        owner_poll(&control);
        assert!(!control.interruption.is_cancelled());
        assert!(matches!(
            force.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let request = control.request(
            binding_ref.clone(),
            SmSessionId::new("live-resume"),
            owner(),
            0,
        );
        tokio::pin!(request);
        assert!(poll!(&mut request).is_pending());
        control.finish();
        assert_eq!(request.await, LocalResumeReply::NotPersisted);
        assert!(!control.interruption.is_cancelled());
    }

    #[tokio::test]
    async fn live_resume_gap_created_while_busy_is_rejected_by_owner() {
        let mut state = StreamManagementState::with_config(1, 1);
        state.enable("live-resume".into(), true, None);
        let binding_ref =
            BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()));
        let (sender, _receiver) = mpsc::channel(1);
        let entry = ConnectionEntry::new_for_binding(sender, binding_ref.clone());
        let control =
            LiveResumeControl::new(state.resume_checkpoint(), entry.write_interruption.clone());
        entry.set_sm_stream_id(Some(SmSessionId::new("live-resume")));
        assert!(entry.wire_resume_control(
            "user@example.test/device".parse().expect("jid"),
            Arc::clone(&control)
        ));
        let mut force = entry.take_force_detach_rx().expect("receiver");
        let request = control.request(binding_ref, SmSessionId::new("live-resume"), owner(), 0);
        tokio::pin!(request);
        assert!(poll!(&mut request).is_pending());
        let _ = state.record_outbound(String::new(), SmEvictionPath::DirectOutbound);
        let _ = state.record_outbound(String::new(), SmEvictionPath::DirectOutbound);
        owner_poll(&control);
        assert_eq!(
            request.await,
            LocalResumeReply::Rejected(SmResumeRejection::ReplayGap {
                inbound_count: 0,
                replay_gap_through: Some(1)
            })
        );
        assert!(!control.interruption.is_cancelled());
        assert!(matches!(
            force.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }
}
