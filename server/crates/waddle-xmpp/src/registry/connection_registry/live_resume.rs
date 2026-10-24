//! Owner-serviced resume preflight, including while the owner is blocked on socket I/O.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::task::AtomicWaker;
use jid::{BareJid, FullJid};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{ForceDetachOrigin, ForceDetachOutcome, ForceDetachRequest};
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
    interruption: CancellationToken,
    binding: Mutex<Option<Binding>>,
    requests: Mutex<Requests>,
    outstanding: Arc<AtomicUsize>,
    waker: AtomicWaker,
}

impl LiveResumeControl {
    pub fn new(checkpoint: SmResumeCheckpointHandle, interruption: CancellationToken) -> Arc<Self> {
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
            force_detach,
        });
        true
    }

    /// Queue a bounded request without touching the old connection's I/O.
    /// Dropping this future never rolls back cleanup already accepted by the owner.
    pub async fn request(
        &self,
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
            if requests.closed
                || self
                    .binding
                    .lock()
                    .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                    .is_none()
            {
                return LocalResumeReply::NotPersisted;
            }
            requests.pending.push_back(PendingResume {
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
            let (ack, receiver) = oneshot::channel();
            if binding
                .force_detach
                .try_send(ForceDetachRequest {
                    origin: ForceDetachOrigin::CrossNodeResume,
                    requester_bare_jid: request.requester,
                    ack,
                })
                .is_err()
            {
                let _ = request.response.send(LocalResumeReply::NotPersisted);
                continue;
            }
            // The detach is queued before cancellation, so the owner can always
            // recover the typed request after the interrupted write yields.
            self.interruption.cancel();
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
    use crate::registry::ConnectionEntry;
    use crate::stream_management::StreamManagementState;
    use crate::telemetry::attributes::SmEvictionPath;
    use futures::{poll, task::noop_waker};

    fn fixture() -> (
        StreamManagementState,
        Arc<LiveResumeControl>,
        mpsc::Receiver<ForceDetachRequest>,
    ) {
        let mut state = StreamManagementState::new();
        state.enable("live-resume".into(), true, None);
        state.increment_outbound();
        let control = LiveResumeControl::new(state.resume_checkpoint(), CancellationToken::new());
        let (sender, _receiver) = mpsc::channel(1);
        let entry = ConnectionEntry::new(sender);
        entry.set_sm_stream_id(Some(SmSessionId::new("live-resume")));
        assert!(entry.wire_resume_control(
            "user@example.test/device".parse().expect("jid"),
            Arc::clone(&control)
        ));
        let force_receiver = entry.take_force_detach_rx().expect("owner receiver");
        (state, control, force_receiver)
    }

    fn owner_poll(control: &LiveResumeControl) {
        let waker = noop_waker();
        assert!(control.poll(&mut Context::from_waker(&waker)).is_ready());
    }

    fn owner() -> BareJid {
        "user@example.test".parse().expect("jid")
    }

    #[tokio::test]
    async fn live_resume_reset_keeps_checkpoint_and_rebinds_current_entry() {
        let (mut state, control, mut old_force) = fixture();
        let stale_request = control.request(SmSessionId::new("live-resume"), owner(), 0);
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

        let (sender, _receiver) = mpsc::channel(1);
        let entry = ConnectionEntry::new(sender);
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

        let request = control.request(SmSessionId::new("replacement-stream"), owner(), 1);
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
        let (mut state, control, mut force) = fixture();
        let request = control.request(SmSessionId::new("live-resume"), owner(), 0);
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
        let (_state, control, mut force) = fixture();
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
            let request = control.request(stream_id, requester, h);
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
        let (_state, control, mut force) = fixture();
        let request = control.request(SmSessionId::new("live-resume"), owner(), 0);
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
                .request(SmSessionId::new("live-resume"), owner(), 0)
                .await,
            LocalResumeReply::NotPersisted
        );
    }

    #[tokio::test]
    async fn live_resume_closed_request_is_inert_and_finish_releases_pending() {
        let (_state, control, mut force) = fixture();
        {
            let request = control.request(SmSessionId::new("live-resume"), owner(), 0);
            tokio::pin!(request);
            assert!(poll!(&mut request).is_pending());
        }
        owner_poll(&control);
        assert!(!control.interruption.is_cancelled());
        assert!(matches!(
            force.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let request = control.request(SmSessionId::new("live-resume"), owner(), 0);
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
        let control = LiveResumeControl::new(state.resume_checkpoint(), CancellationToken::new());
        let (sender, _receiver) = mpsc::channel(1);
        let entry = ConnectionEntry::new(sender);
        entry.set_sm_stream_id(Some(SmSessionId::new("live-resume")));
        assert!(entry.wire_resume_control(
            "user@example.test/device".parse().expect("jid"),
            Arc::clone(&control)
        ));
        let mut force = entry.take_force_detach_rx().expect("receiver");
        let request = control.request(SmSessionId::new("live-resume"), owner(), 0);
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
