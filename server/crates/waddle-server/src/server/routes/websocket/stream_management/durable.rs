//! The connection's side of the durable XEP-0198 session authority.
//!
//! When `WADDLE_XMPP_DURABLE_SM` selects `durable`, a resumable session's
//! lifecycle, owner fence, counters and retained frames belong to
//! [`crate::sm_authority::DurableSmStore`]. The connection keeps a
//! [`DurableSmLink`]: the handle it presents on every authority call. The
//! connection's `StreamManagementState` is then a projection of the
//! authority's window, keyed by the session key rather than by the resume
//! token the client holds.
//!
//! A non-resumable `<enable/>` has no resume token, is never detached and
//! leaves nothing to recover, so it does not create a durable session: it
//! keeps the in-memory counters on either selector.

use std::time::Duration;

use jid::FullJid;
use tracing::{error, info, warn};
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ownership::BindingRef;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::durable::OwnerFence;
use waddle_xmpp::stream_management::durable::{Exposure, HandledError, ResumeDenial};
use waddle_xmpp::stream_management::persistence::{SmClaimFence, SmReplayPayload};
use waddle_xmpp::stream_management::{
    SmFailed, SmFrameProvenance, SmIngressFrameReceipt, SmResume, SmResumed, StreamManagementState,
};
use waddle_xmpp::telemetry::attributes::{SmEnableOutcome, SmEvictionPath};
use waddle_xmpp::telemetry::authority::{record_sm_enable, record_sm_resume_replay};

use super::implementation::{max_resume_secs_from_env, SmCtx, SmEnableClaimGuard, SmEnableCommit};
use super::observe::{SmResumeOutcome, SmResumeTerminal};
use super::{ConnectionPhase, ResponseFrame, Session, WebSocketState};
use crate::sm_authority::{
    AdmissionAuthority, AdmissionError, AdmissionFrame, AdmissionRejection, AdmissionRequest,
    CommittedObligation, DurableSmStore, EnableError, EnableOperation, EnableRejection,
    EnableRequest, EnableResolution, EnabledCommit, FreshBindPermit, OwnerError, OwnerRejection,
    ResumeError, ResumeOperation, ResumeRejection, ResumeRejectionReason, ResumeRequest,
    ResumeResolution, ResumedCommit, RetainedFrame,
};

/// First wait before an operation with an unknown commit outcome is settled.
const SETTLEMENT_FIRST_DELAY: Duration = Duration::from_millis(100);
/// Longest wait between two settlement attempts.
const SETTLEMENT_MAX_DELAY: Duration = Duration::from_secs(5);
/// How often one frame is offered again while its commit outcome is unknown.
/// Admission is idempotent under the frame's operation id.
const ADMISSION_ATTEMPTS: usize = 3;

/// What a connection holds of the durable stream-management authority.
#[derive(Default)]
pub(in crate::server::routes::websocket) enum DurableSmLink {
    /// The connection owns no durable session.
    #[default]
    Unbound,
    /// An enable whose commit outcome is unknown. Nothing was sent to the
    /// client, and no other enable is accepted until it is settled.
    EnableUnresolved(Box<UnresolvedEnable>),
    /// A resume whose commit outcome is unknown. The connection reads no
    /// further client input until it is settled: only a known rejection
    /// lets the client bind a fresh resource.
    ResumeUnresolved(Box<UnresolvedResume>),
    /// A resume committed this binding as the next owner. The session is
    /// not published yet: it becomes live when the endpoint is registered
    /// and the resume is completed, and returns to detached otherwise.
    Hydrating(Box<HydratingResume>),
    /// This connection's binding owns the session's current generation.
    Owner(Box<DurableSmOwner>),
    /// The authority refused this binding's fence: a later generation owns
    /// the session, or the session ended. The connection writes nothing more
    /// for it and retires.
    Superseded,
}

/// The owner fence a connection presents on each authority call, with the
/// claim and principal the authority compares it against.
#[derive(Clone)]
pub(in crate::server::routes::websocket) struct DurableSmOwner {
    store: DurableSmStore,
    fence: OwnerFence,
    /// This node's claim on the session. Absent only where the legacy claim
    /// bookkeeping holds none; a clustered store then refuses each write.
    claim: Option<SmClaimFence>,
    principal: AuthenticatedPrincipalRef,
    /// Outbound counter value of the last frame this owner knows to be in
    /// the authority's custody. Projection entries above it were recorded
    /// without a transport and are admitted before the session detaches.
    admitted_through: u32,
    inbound: InboundHandled,
}

/// The nonwrapping count of handled client stanzas, advanced by the wire
/// counter's movement.
#[derive(Debug, Clone, Copy)]
struct InboundHandled {
    wire: u32,
    total: u64,
}

impl InboundHandled {
    fn advance_to(&mut self, wire: u32) -> u64 {
        let delta = wire.wrapping_sub(self.wire);
        // A value in the far half of the counter space is behind the last
        // report, not ahead of it.
        if delta < (1 << 31) {
            self.total += u64::from(delta);
            self.wire = wire;
        }
        self.total
    }
}

/// One counted outbound stanza offered to the authority.
pub(in crate::server::routes::websocket) struct CustodyFrame<'a> {
    pub(in crate::server::routes::websocket) xml: &'a str,
    pub(in crate::server::routes::websocket) original_receipt_at: chrono::DateTime<chrono::Utc>,
    pub(in crate::server::routes::websocket) provenance: &'a SmFrameProvenance,
    pub(in crate::server::routes::websocket) ingress_receipts: &'a [SmIngressFrameReceipt],
}

/// Why a frame is not in the authority's custody.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::routes::websocket) enum CustodyRefusal {
    /// Another generation owns the session, or the session ended.
    Superseded,
    /// The session's retained-frame bound is reached. The frame is
    /// undeliverable to this session until the peer acknowledges.
    WindowFull,
    /// The authority could not take the frame now: its store, claim or
    /// commit outcome is unavailable, or the frame is not a stanza.
    Unavailable,
}

/// The authority's answer for a frame about to be written to the transport.
#[must_use = "a refused frame must not be written, and a committed one is recorded under its sequence"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::routes::websocket) enum LiveCustody {
    /// The connection owns no durable session; the in-memory queue counts.
    Unmanaged,
    /// The frame is committed and exposed: write it, and record it in the
    /// projection under this outbound counter value.
    Exposed {
        sequence: u32,
    },
    /// The frame is committed but must not be written yet: a resume has not
    /// published the session, and the frame follows its replay.
    Retained,
    Refused(CustodyRefusal),
}

/// The authority's answer for a client `<a h=…/>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::routes::websocket) enum AckVerdict {
    /// The connection owns no durable session, or the authority settled the
    /// count. The projection applies it.
    Apply,
    /// The peer acknowledged a stanza that was never exposed to it. The
    /// stream is in error and nothing was released.
    HandledInvalid,
    /// Another generation owns the session; the connection retires.
    Superseded,
    /// The settlement could not be committed. Nothing is applied in memory.
    Unavailable,
}

/// An enable attempt between its commit and a known outcome.
pub(in crate::server::routes::websocket) struct UnresolvedEnable {
    operation: EnableOperation,
    binding: BindingRef,
    principal: AuthenticatedPrincipalRef,
    claim: Option<SmClaimFence>,
    /// Keeps the session claim published, and inventories its release if the
    /// enable turns out not to have committed.
    claim_guard: SmEnableClaimGuard,
    stream_id: SmSessionId,
    max_resume_secs: u32,
    retry: SettlementRetry,
}

/// A resume attempt between its commit and a known outcome.
pub(in crate::server::routes::websocket) struct UnresolvedResume {
    operation: ResumeOperation,
    binding: BindingRef,
    presented: AuthenticatedPrincipalRef,
    claim: Option<SmClaimFence>,
    /// The `h` of the client's `<resume/>`.
    h: u32,
    stream_id: SmSessionId,
    /// The canonical ingress checkpoint of the session's inbound counter.
    inbound_checkpoint: u32,
    retry: SettlementRetry,
}

/// A committed resume that has not published its session.
pub(in crate::server::routes::websocket) struct HydratingResume {
    store: DurableSmStore,
    commit: ResumedCommit,
    claim: Option<SmClaimFence>,
    presented: AuthenticatedPrincipalRef,
    h: u32,
}

/// The reply and replay floor of a completed resume.
pub(in crate::server::routes::websocket) struct DurableResumed {
    pub(in crate::server::routes::websocket) resumed: SmResumed,
    pub(in crate::server::routes::websocket) replay_after_h: u32,
}

/// Why a committed resume did not become live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::routes::websocket) enum HydrationFailure {
    /// The connection holds no committed resume.
    NotHydrating,
    /// A later resume took the session while this one hydrated.
    Superseded,
    /// The completion could not be committed.
    Unavailable,
}

/// When the next settlement attempt of an unresolved operation is due.
#[derive(Debug, Clone, Copy)]
pub(in crate::server::routes::websocket) struct SettlementRetry {
    due: tokio::time::Instant,
    delay: Duration,
}

impl SettlementRetry {
    fn first() -> Self {
        Self {
            due: tokio::time::Instant::now() + SETTLEMENT_FIRST_DELAY,
            delay: SETTLEMENT_FIRST_DELAY,
        }
    }

    /// Schedule the attempt after a failed one, doubling the wait up to
    /// [`SETTLEMENT_MAX_DELAY`].
    fn back_off(&mut self) {
        self.delay = (self.delay * 2).min(SETTLEMENT_MAX_DELAY);
        self.due = tokio::time::Instant::now() + self.delay;
    }
}

impl DurableSmLink {
    /// When the live loop must next try to settle an operation whose commit
    /// outcome is unknown.
    pub(in crate::server::routes::websocket) fn settlement_due(
        &self,
    ) -> Option<tokio::time::Instant> {
        match self {
            Self::EnableUnresolved(unresolved) => Some(unresolved.retry.due),
            Self::ResumeUnresolved(unresolved) => Some(unresolved.retry.due),
            Self::Unbound | Self::Hydrating(_) | Self::Owner(_) | Self::Superseded => None,
        }
    }

    /// Whether an `<enable/>` may start: the connection neither owns a
    /// durable session nor waits on an enable's outcome.
    pub(in crate::server::routes::websocket) fn admits_enable(&self) -> bool {
        matches!(self, Self::Unbound)
    }

    /// Whether the authority has refused this binding's fence.
    pub(in crate::server::routes::websocket) fn is_superseded(&self) -> bool {
        matches!(self, Self::Superseded)
    }

    /// Whether client input waits for a resume outcome. While it is
    /// unknown the client's next request, a bind included, stays unread.
    pub(in crate::server::routes::websocket) fn holds_inbound(&self) -> bool {
        matches!(self, Self::ResumeUnresolved(_))
    }

    pub(in crate::server::routes::websocket) fn is_hydrating(&self) -> bool {
        matches!(self, Self::Hydrating(_))
    }

    /// Release the hold of a resume that is known not to own the session.
    /// The permit is the evidence: an unknown outcome yields none.
    fn release_resume_hold(&mut self, _known: FreshBindPermit) {
        if matches!(self, Self::ResumeUnresolved(_)) {
            *self = Self::Unbound;
        }
    }

    /// Publish a committed resume as live after its endpoint is registered,
    /// and make this binding the session's owner.
    ///
    /// The frames admitted while the resume hydrated join the projection
    /// after the replay range, so the caller's replay writes the witness's
    /// frames, then this tail, before any live frame.
    pub(in crate::server::routes::websocket) async fn complete_hydration(
        &mut self,
        sm_state: &mut StreamManagementState,
    ) -> Result<DurableResumed, HydrationFailure> {
        let Self::Hydrating(hydrating) = self else {
            return Err(HydrationFailure::NotHydrating);
        };
        let mut attempts = 0;
        let tail = loop {
            attempts += 1;
            let completed = hydrating
                .store
                .complete_resume(
                    &hydrating.commit,
                    hydrating.claim.as_ref(),
                    chrono::Utc::now(),
                )
                .await;
            match completed {
                Ok(tail) => break tail,
                // Completion is idempotent under the same fence.
                Err(OwnerError::Indeterminate(_)) if attempts < ADMISSION_ATTEMPTS => {}
                Err(error) => {
                    warn!(
                        session = ?hydrating.commit.fence().session,
                        %error,
                        "durable SM resume could not be completed"
                    );
                    return Err(
                        if matches!(
                            error,
                            OwnerError::Rejected(
                                OwnerRejection::Fence(_)
                                    | OwnerRejection::NotOwned(_)
                                    | OwnerRejection::UnknownSession
                            )
                        ) {
                            HydrationFailure::Superseded
                        } else {
                            HydrationFailure::Unavailable
                        },
                    );
                }
            }
        };
        project_retained_frames(sm_state, &tail.frames);
        let Self::Hydrating(hydrating) = std::mem::take(self) else {
            return Err(HydrationFailure::NotHydrating);
        };
        let HydratingResume {
            store,
            commit,
            claim,
            presented,
            h,
        } = *hydrating;
        record_sm_resume_replay(commit.replay().len() + tail.frames.len());
        let floor = commit.inbound_handled();
        let mut inbound = InboundHandled {
            wire: floor as u32,
            total: floor,
        };
        inbound.advance_to(sm_state.inbound_count());
        let resumed = commit.resumed_reply(sm_state.inbound_count());
        *self = Self::Owner(Box::new(DurableSmOwner {
            store,
            fence: commit.fence().clone(),
            claim,
            principal: presented,
            admitted_through: sm_state.outbound_count,
            inbound,
        }));
        Ok(DurableResumed {
            resumed,
            replay_after_h: h,
        })
    }

    /// Return a committed resume that will not be published to `Detached`.
    /// The session keeps its stored resource snapshot and first detach time,
    /// so a later resume still finds it. Returns whether the session is
    /// known not to be owned by this binding afterwards.
    pub(in crate::server::routes::websocket) async fn abandon_hydration(&mut self) -> bool {
        if !self.is_hydrating() {
            return true;
        }
        let Self::Hydrating(hydrating) = std::mem::take(self) else {
            return true;
        };
        let mut attempts = 0;
        loop {
            attempts += 1;
            let compensated = hydrating
                .store
                .compensate_resume(
                    &hydrating.commit,
                    hydrating.claim.as_ref(),
                    chrono::Utc::now(),
                )
                .await;
            match compensated {
                Ok(()) => return true,
                // A fence refusal means another generation owns the session
                // already; this binding owns nothing either way.
                Err(OwnerError::Rejected(
                    OwnerRejection::Fence(_)
                    | OwnerRejection::NotOwned(_)
                    | OwnerRejection::UnknownSession,
                )) => return true,
                Err(OwnerError::Indeterminate(_)) if attempts < ADMISSION_ATTEMPTS => {}
                Err(error) => {
                    error!(
                        session = ?hydrating.commit.fence().session,
                        %error,
                        "an unpublished durable SM resume could not be compensated"
                    );
                    return false;
                }
            }
        }
    }

    /// Take custody of a counted frame before it is written.
    ///
    /// Frames the projection recorded without a transport are admitted
    /// first, so the authority's ordinals follow the projection's order.
    pub(in crate::server::routes::websocket) async fn take_live_custody(
        &mut self,
        sm_state: &StreamManagementState,
        frame: CustodyFrame<'_>,
    ) -> LiveCustody {
        let owner = match self {
            Self::Unbound | Self::EnableUnresolved(_) => return LiveCustody::Unmanaged,
            // No frame is counted for a session this binding does not own
            // yet; the projection is not enabled in either state.
            Self::ResumeUnresolved(_) | Self::Hydrating(_) => {
                return LiveCustody::Refused(CustodyRefusal::Unavailable)
            }
            Self::Superseded => return LiveCustody::Refused(CustodyRefusal::Superseded),
            Self::Owner(owner) => owner,
        };
        let admitted = match owner.admit_projection_tail(sm_state).await {
            Ok(()) => owner.admit(&frame).await,
            Err(refusal) => Err(refusal),
        };
        match admitted {
            Ok(committed) => match committed.exposure() {
                Exposure::Exposed => {
                    owner.admitted_through = committed.sequence();
                    LiveCustody::Exposed {
                        sequence: committed.sequence(),
                    }
                }
                Exposure::Retained => LiveCustody::Retained,
            },
            Err(refusal) => {
                if refusal == CustodyRefusal::Superseded {
                    *self = Self::Superseded;
                }
                LiveCustody::Refused(refusal)
            }
        }
    }

    /// Settle a client `<a h=…/>` against the authority's exposure ceiling.
    pub(in crate::server::routes::websocket) async fn settle_ack(&mut self, h: u32) -> AckVerdict {
        let Self::Owner(owner) = self else {
            return match self {
                Self::Superseded => AckVerdict::Superseded,
                _ => AckVerdict::Apply,
            };
        };
        let settled = owner
            .store
            .settle_ack(&owner.fence, owner.claim.as_ref(), h, chrono::Utc::now())
            .await;
        match settled {
            Ok(_) => AckVerdict::Apply,
            Err(OwnerError::Rejected(OwnerRejection::Handled(error))) => {
                match error {
                    HandledError::TooHigh { .. } | HandledError::Ambiguous { .. } => {}
                    HandledError::Behind { .. } => {
                        // Only a resume reports a count behind the floor.
                        error!(session = ?owner.fence.session, %error, "live SM ack classified as a resume count");
                    }
                }
                AckVerdict::HandledInvalid
            }
            Err(OwnerError::Rejected(
                rejection @ (OwnerRejection::Fence(_)
                | OwnerRejection::NotOwned(_)
                | OwnerRejection::UnknownSession),
            )) => {
                info!(
                    session = ?owner.fence.session,
                    error = %rejection,
                    "durable SM session is owned elsewhere; retiring this binding"
                );
                *self = Self::Superseded;
                AckVerdict::Superseded
            }
            Err(error) => {
                warn!(session = ?owner.fence.session, %error, "durable SM ack could not be settled");
                AckVerdict::Unavailable
            }
        }
    }

    /// Report the handled count of client stanzas the connection is about to
    /// acknowledge. The value is the floor a later resume starts from; the
    /// ingress checkpoint stays the authority for stanzas that entered
    /// canonical ingress, so a failed report does not withhold the answer.
    pub(in crate::server::routes::websocket) async fn report_inbound_handled(&mut self, wire: u32) {
        let Self::Owner(owner) = self else {
            return;
        };
        let handled = owner.inbound.advance_to(wire);
        let reported = owner
            .store
            .record_inbound_handled(
                &owner.fence,
                owner.claim.as_ref(),
                handled,
                chrono::Utc::now(),
            )
            .await;
        match reported {
            Ok(()) => {}
            Err(OwnerError::Rejected(
                rejection @ (OwnerRejection::Fence(_)
                | OwnerRejection::NotOwned(_)
                | OwnerRejection::UnknownSession),
            )) => {
                info!(
                    session = ?owner.fence.session,
                    error = %rejection,
                    "durable SM session is owned elsewhere; retiring this binding"
                );
                *self = Self::Superseded;
            }
            Err(error) => {
                warn!(session = ?owner.fence.session, %error, "durable SM handled count was not recorded");
            }
        }
    }
}

impl DurableSmOwner {
    /// Offer one frame on this owner's fence. An unknown commit outcome is
    /// settled by offering the same operation again.
    async fn admit(&self, frame: &CustodyFrame<'_>) -> Result<CommittedObligation, CustodyRefusal> {
        let payload = SmReplayPayload::from_wire(frame.xml.to_owned()).map_err(|error| {
            warn!(session = ?self.fence.session, %error, "counted frame is not a stanza the authority can retain");
            CustodyRefusal::Unavailable
        })?;
        let admission = AdmissionFrame {
            operation: waddle_xmpp::stream_management::durable::SmOperationId::mint(),
            payload,
            original_receipt_at: frame.original_receipt_at,
            ingress_receipts: frame.ingress_receipts.to_vec(),
            provenance: frame.provenance.clone(),
        };
        let mut attempts = 0;
        loop {
            attempts += 1;
            let admitted = self
                .store
                .admit(
                    AdmissionRequest {
                        authority: AdmissionAuthority::Owner {
                            fence: &self.fence,
                            principal: &self.principal,
                        },
                        claim: self.claim.as_ref(),
                        now: chrono::Utc::now(),
                    },
                    &admission,
                )
                .await;
            match admitted {
                Ok(committed) => return Ok(committed),
                Err(AdmissionError::Indeterminate(cause)) if attempts < ADMISSION_ATTEMPTS => {
                    warn!(session = ?self.fence.session, error = %cause, "durable SM admission outcome unknown; offering the frame again");
                }
                Err(AdmissionError::Indeterminate(cause)) => {
                    warn!(session = ?self.fence.session, error = %cause, "durable SM admission outcome is still unknown");
                    return Err(CustodyRefusal::Unavailable);
                }
                Err(AdmissionError::Rejected(rejection)) => {
                    return Err(admission_refusal(&self.fence, &rejection));
                }
            }
        }
    }

    /// Admit the projection entries above [`Self::admitted_through`], in
    /// order. They were recorded while no transport could carry them.
    async fn admit_projection_tail(
        &mut self,
        sm_state: &StreamManagementState,
    ) -> Result<(), CustodyRefusal> {
        if sm_state.outbound_count == self.admitted_through {
            return Ok(());
        }
        for entry in sm_state.stanzas_to_resend(self.admitted_through) {
            let committed = self
                .admit(&CustodyFrame {
                    xml: &entry.stanza_xml,
                    original_receipt_at: entry.original_receipt_at,
                    provenance: &entry.provenance,
                    ingress_receipts: &entry.ingress_receipts,
                })
                .await?;
            let expected = self.admitted_through.wrapping_add(1);
            if committed.sequence() != expected {
                error!(
                    session = ?self.fence.session,
                    committed = committed.sequence(),
                    expected,
                    "durable SM ordinal diverged from the connection's projection"
                );
                return Err(CustodyRefusal::Unavailable);
            }
            self.admitted_through = expected;
        }
        Ok(())
    }
}

fn admission_refusal(fence: &OwnerFence, rejection: &AdmissionRejection) -> CustodyRefusal {
    match rejection {
        AdmissionRejection::UnknownSession
        | AdmissionRejection::Fence(_)
        | AdmissionRejection::NotAdmitting(_) => {
            info!(
                session = ?fence.session,
                error = %rejection,
                "durable SM session is owned elsewhere; retiring this binding"
            );
            CustodyRefusal::Superseded
        }
        AdmissionRejection::Bound(_) => {
            warn!(session = ?fence.session, error = %rejection, "durable SM retained-frame bound reached");
            CustodyRefusal::WindowFull
        }
        AdmissionRejection::PrincipalMismatch
        | AdmissionRejection::ClaimFenceMissing
        | AdmissionRejection::OperationReused
        | AdmissionRejection::Store(_) => {
            warn!(session = ?fence.session, error = %rejection, "durable SM admission refused");
            CustodyRefusal::Unavailable
        }
    }
}

/// The durable store, when the durable authority is selected.
pub(in crate::server::routes::websocket) fn selected_store(
    state: &WebSocketState,
) -> Option<&DurableSmStore> {
    state.deps.app_state.durable_sm().store()
}

/// What a resumable `<enable/>` presents to the durable authority.
pub(super) struct DurableEnable<'a> {
    pub(super) link: &'a mut DurableSmLink,
    pub(super) binding: Option<&'a BindingRef>,
    pub(super) session: Option<&'a Session>,
    pub(super) full_jid: Option<&'a FullJid>,
    pub(super) max_resume_secs: u32,
    pub(super) pending_commit: &'a mut Option<SmEnableCommit>,
}

fn failed(condition: &str) -> Vec<ResponseFrame> {
    vec![ResponseFrame::from(
        SmFailed::with_condition(condition).to_element(),
    )]
}

/// Commit a resumable session to the durable authority and answer the
/// `<enable/>` from the commit's witness.
///
/// The session claim is acquired for the minted session key before the
/// commit, so a clustered store finds the claim row the commit asserts. An
/// unknown commit outcome answers nothing: the operation stays on the
/// connection and the live loop settles it.
///
/// The caller awaits this to completion; nothing drops it between the commit
/// and the recorded outcome except the loss of the whole connection task.
pub(super) async fn enable(
    store: &DurableSmStore,
    state: &WebSocketState,
    request: DurableEnable<'_>,
) -> Vec<ResponseFrame> {
    let DurableEnable {
        link,
        binding,
        session,
        full_jid,
        max_resume_secs,
        pending_commit,
    } = request;
    let store = store.clone();
    let (Some(binding), Some(session), Some(full_jid)) = (binding, session, full_jid) else {
        record_sm_enable(SmEnableOutcome::Refused);
        return failed("unexpected-request");
    };
    let principal = match session.authenticated_principal_ref() {
        Ok(principal) => principal,
        Err(error) => {
            warn!(%error, "SM enable refused: the session has no durable principal reference");
            record_sm_enable(SmEnableOutcome::Failed);
            return failed("internal-server-error");
        }
    };
    let operation = EnableOperation::mint();
    let stream_id = SmSessionId::new(operation.session().as_uuid().to_string());
    let registry = &state.deps.protocol.sm_session_registry;
    let Some(publication) = registry.ensure_session_claim(stream_id.as_str()).await else {
        warn!(%stream_id, "SM enable rejected because exact claim admission did not complete");
        record_sm_enable(SmEnableOutcome::Refused);
        return failed("resource-constraint");
    };
    let claim = registry.current_sm_claim_fence(stream_id.as_str());
    let claim_guard = SmEnableClaimGuard::new(registry.clone(), stream_id.clone(), publication);
    if state
        .deps
        .protocol
        .ingress
        .enroll_stream(&stream_id)
        .await
        .is_err()
    {
        record_sm_enable(SmEnableOutcome::Failed);
        return failed("resource-constraint");
    }
    let committed = store
        .enable(
            &operation,
            EnableRequest {
                binding,
                principal: &principal,
                full_jid,
                max_resume: Duration::from_secs(u64::from(max_resume_secs)),
                claim: claim.as_ref(),
                now: chrono::Utc::now(),
            },
        )
        .await;
    let unresolved = UnresolvedEnable {
        operation,
        binding: binding.clone(),
        principal,
        claim,
        claim_guard,
        stream_id,
        max_resume_secs,
        retry: SettlementRetry::first(),
    };
    match committed {
        Ok(commit) => vec![adopt_committed_enable(
            store,
            link,
            pending_commit,
            unresolved,
            commit,
        )],
        Err(EnableError::Rejected(rejection)) => {
            warn!(
                stream_id = %unresolved.stream_id,
                error = %rejection,
                "durable SM enable rejected"
            );
            let condition = rejection_condition(&rejection);
            abandon_enable(state, unresolved).await;
            record_sm_enable(match rejection {
                EnableRejection::Store(_) => SmEnableOutcome::Failed,
                _ => SmEnableOutcome::Refused,
            });
            failed(condition)
        }
        Err(EnableError::Indeterminate(cause)) => {
            warn!(
                stream_id = %unresolved.stream_id,
                error = %cause,
                "durable SM enable commit outcome is unknown; holding the reply"
            );
            *link = DurableSmLink::EnableUnresolved(Box::new(unresolved));
            Vec::new()
        }
    }
}

/// The `<failed/>` condition of a known enable rejection.
fn rejection_condition(rejection: &EnableRejection) -> &'static str {
    match rejection {
        EnableRejection::ResourceMismatch => "not-authorized",
        EnableRejection::ClaimFenceMissing => "resource-constraint",
        EnableRejection::ResumeTokenCollision
        | EnableRejection::Abandoned
        | EnableRejection::OperationReused
        | EnableRejection::Store(_) => "internal-server-error",
    }
}

/// Make the connection the owner of a committed session and stage the
/// `<enabled/>` reply built from the commit's witness. The projection in
/// `StreamManagementState` starts when that reply has been written, as it
/// does for an in-memory session.
fn adopt_committed_enable(
    store: DurableSmStore,
    link: &mut DurableSmLink,
    pending_commit: &mut Option<SmEnableCommit>,
    enable: UnresolvedEnable,
    commit: EnabledCommit,
) -> ResponseFrame {
    let UnresolvedEnable {
        principal,
        claim,
        claim_guard,
        stream_id,
        max_resume_secs,
        ..
    } = enable;
    let reply = commit.enabled_reply();
    info!(%stream_id, "durable SM session committed");
    *link = DurableSmLink::Owner(Box::new(DurableSmOwner {
        store,
        fence: commit.fence().clone(),
        claim: claim.clone(),
        principal,
        admitted_through: 0,
        inbound: InboundHandled { wire: 0, total: 0 },
    }));
    *pending_commit = Some(SmEnableCommit::new(
        Some(claim_guard),
        claim,
        stream_id,
        true,
        reply.max.unwrap_or(max_resume_secs),
    ));
    record_sm_enable(SmEnableOutcome::Enabled);
    ResponseFrame::SmEnabled(reply.to_element())
}

/// Release what an enable acquired once it is known not to have committed:
/// the ingress stream it enrolled and, through the guard, its session claim.
async fn abandon_enable(state: &WebSocketState, enable: UnresolvedEnable) {
    if let Err(error) = state
        .deps
        .protocol
        .ingress
        .forget_stream(&enable.stream_id)
        .await
    {
        warn!(
            stream_id = %enable.stream_id,
            %error,
            "failed to retire the ingress stream of an uncommitted SM enable"
        );
    }
    drop(enable.claim_guard);
}

/// What settling an unresolved enable produced.
pub(in crate::server::routes::websocket) enum EnableSettlement {
    /// Nothing was unresolved, or the outcome is still unknown and another
    /// attempt is scheduled.
    Pending,
    /// The enable committed; the frame is its `<enabled/>` reply.
    Enabled(ResponseFrame),
    /// The enable is known not to own a session. The `<failed/>` answers the
    /// client's request.
    Failed(ResponseFrame),
}

/// Settle an enable whose commit outcome is unknown. Safe to drop and call
/// again: the resolution is idempotent under the operation id.
pub(in crate::server::routes::websocket) async fn settle_unresolved_enable(
    state: &WebSocketState,
    link: &mut DurableSmLink,
    pending_commit: &mut Option<SmEnableCommit>,
) -> EnableSettlement {
    let DurableSmLink::EnableUnresolved(unresolved) = link else {
        return EnableSettlement::Pending;
    };
    let Some(store) = selected_store(state) else {
        return EnableSettlement::Pending;
    };
    let resolution = store
        .resolve_enable(
            &unresolved.operation,
            &unresolved.binding,
            chrono::Utc::now(),
        )
        .await;
    let resolution = match resolution {
        Ok(resolution) => resolution,
        Err(cause) => {
            unresolved.retry.back_off();
            warn!(
                stream_id = %unresolved.stream_id,
                error = %cause,
                "durable SM enable is still unresolved"
            );
            return EnableSettlement::Pending;
        }
    };
    let DurableSmLink::EnableUnresolved(unresolved) = std::mem::take(link) else {
        return EnableSettlement::Pending;
    };
    let superseded = matches!(resolution, EnableResolution::Superseded);
    match resolution {
        EnableResolution::Committed(commit) => EnableSettlement::Enabled(adopt_committed_enable(
            store.clone(),
            link,
            pending_commit,
            *unresolved,
            commit,
        )),
        EnableResolution::Superseded | EnableResolution::NotCommitted => {
            if superseded {
                error!(
                    stream_id = %unresolved.stream_id,
                    "durable SM enable committed and was superseded before its reply"
                );
            }
            abandon_enable(state, *unresolved).await;
            record_sm_enable(SmEnableOutcome::Failed);
            EnableSettlement::Failed(ResponseFrame::from(
                SmFailed::with_condition("internal-server-error").to_element(),
            ))
        }
    }
}

/// End a committed session whose `<enabled/>` was never written. The client
/// holds no resume token for it, so nothing can resume it; closing it lets
/// the janitor release its row instead of leaving a live owner that no
/// longer exists.
pub(in crate::server::routes::websocket) async fn close_unannounced_session(
    link: &mut DurableSmLink,
) {
    if !matches!(link, DurableSmLink::Owner(_)) {
        return;
    }
    let DurableSmLink::Owner(owner) = std::mem::take(link) else {
        return;
    };
    if let Err(error) = owner
        .store
        .close(&owner.fence, owner.claim.as_ref(), chrono::Utc::now())
        .await
    {
        warn!(
            session = ?owner.fence.session,
            %error,
            "an unannounced durable SM session could not be closed"
        );
    }
}

/// Record retained frames in the projection under their committed outbound
/// counter values, with the obligations each frame carries.
fn project_retained_frames(sm_state: &mut StreamManagementState, frames: &[RetainedFrame]) {
    for retained in frames {
        // The request cadence starts again with the first live frame.
        let _ = sm_state.record_committed_outbound(
            retained.frame.sequence,
            retained.frame.stanza.wire().to_owned(),
            retained.frame.original_receipt_at,
            SmEvictionPath::ReplayTail,
            retained.frame.provenance.clone(),
        );
        sm_state.attach_ingress_receipts(retained.frame.ingress_receipts.clone());
    }
}

/// The connection fields a committed resume restores.
pub(super) struct ResumeTargets<'a> {
    pub(super) durable_sm: &'a mut DurableSmLink,
    pub(super) binding_ref: &'a mut Option<BindingRef>,
    pub(super) phase: &'a mut ConnectionPhase,
    pub(super) sm_state: &'a mut StreamManagementState,
    pub(super) sm_ingress_fence: &'a mut Option<SmClaimFence>,
    pub(super) occupancy_session: &'a mut waddle_xmpp_core::OccupancySessionGeneration,
    pub(super) carbons_enabled: &'a mut bool,
    pub(super) roster_interested: &'a mut bool,
    pub(super) blocklist_interested: &'a mut bool,
    pub(super) presence_available: &'a mut bool,
    pub(super) presence_show: &'a mut Option<xmpp_parsers::presence::Show>,
    pub(super) presence_status: &'a mut Option<String>,
    pub(super) presence_priority: &'a mut i8,
    pub(super) presence_payloads: &'a mut Vec<minidom::Element>,
    pub(super) pending_subscribes_flushed: &'a mut bool,
    pub(super) pending_resume_stream_id: &'a mut Option<String>,
    pub(super) pending_resume_h: &'a mut Option<u32>,
    pub(super) suppress_sm_record_next_batch: &'a mut bool,
}

/// Hydrate the connection from a committed resume and stage it for
/// publication. Registration then publishes the endpoint and completes the
/// resume; until that the session stays unpublished in the authority.
fn adopt_committed_resume(
    store: DurableSmStore,
    targets: ResumeTargets<'_>,
    resume: UnresolvedResume,
    commit: ResumedCommit,
) {
    let UnresolvedResume {
        binding,
        presented,
        claim,
        h,
        stream_id,
        inbound_checkpoint,
        ..
    } = resume;
    let sm_state = targets.sm_state;
    sm_state.reset();
    sm_state.enable(
        stream_id.to_string(),
        true,
        Some(
            commit
                .resource()
                .and_then(|resource| resource.max_resume_time)
                .unwrap_or_else(max_resume_secs_from_env),
        ),
    );
    sm_state.inbound_count = waddle_xmpp::stream_management::sequence::max_in_window(
        commit.inbound_handled() as u32,
        inbound_checkpoint,
    );
    // A valid resume count is the acknowledged floor after the commit
    // settled it: either the floor itself or the value it advanced to.
    sm_state.outbound_count = h;
    sm_state.acknowledge(h);
    project_retained_frames(sm_state, commit.replay());

    if let Some(resource) = commit.resource() {
        *targets.occupancy_session = resource.occupancy_session;
        *targets.carbons_enabled = resource.carbons_enabled;
        *targets.roster_interested = resource.roster_interested;
        *targets.blocklist_interested = resource.blocklist_interested;
        *targets.presence_available = resource.presence_available;
        *targets.presence_show = resource.presence_show.clone();
        *targets.presence_status = resource.presence_status.clone();
        *targets.presence_priority = resource.presence_priority;
        *targets.presence_payloads = resource.presence_payloads.clone();
        // The snapshot does not record the once-per-session subscription
        // flush. A resource that was available has consumed it.
        *targets.pending_subscribes_flushed = resource.presence_available;
    }
    *targets.sm_ingress_fence = claim.clone();
    *targets.pending_resume_stream_id = Some(stream_id.to_string());
    *targets.pending_resume_h = Some(h);
    *targets.binding_ref = Some(binding);
    *targets.phase = ConnectionPhase::ready(commit.full_jid().clone(), true);
    // The replay is written from the projection, which already holds each
    // frame under its committed sequence.
    *targets.suppress_sm_record_next_batch = true;
    info!(%stream_id, replay = commit.replay().len(), "durable SM resume committed");
    *targets.durable_sm = DurableSmLink::Hydrating(Box::new(HydratingResume {
        store,
        commit,
        claim,
        presented,
        h,
    }));
}

/// Ask a binding the resume superseded to end, as cleanup. Its transport is
/// already fenced by the committed generation; this only stops a local
/// connection from running until its next authority call fails.
fn close_superseded_local_binding(
    state: &WebSocketState,
    commit: &ResumedCommit,
    requester: &jid::BareJid,
) {
    let Some(superseded) = commit.superseded_binding() else {
        return;
    };
    let Some(entry) = state
        .deps
        .protocol
        .connection_registry
        .entry(commit.full_jid())
    else {
        return;
    };
    if entry.binding_ref() != Some(superseded) {
        return;
    }
    let (ack, _ack_rx) = tokio::sync::oneshot::channel();
    let request = waddle_xmpp::registry::ForceDetachRequest {
        origin: waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
        target_binding: Some(superseded.clone()),
        requester_bare_jid: requester.clone(),
        ack,
    };
    // A full or closed channel means the binding is already ending.
    let _ = entry.force_detach_sender().try_send(request);
}

/// How one step of a durable resume ended.
pub(super) enum DurableResumeStep {
    /// The resume committed and the connection is staged for publication,
    /// or its outcome is unknown and the connection holds. Nothing is
    /// answered in this step.
    Staged,
    /// The resume is known not to have happened.
    Failed(SmResumeTerminal),
}

/// The terminal a known resume rejection owes the client.
fn rejection_terminal(
    stream_id: SmSessionId,
    full_jid: Option<&FullJid>,
    client_h: u32,
    rejection: &ResumeRejection,
) -> SmResumeTerminal {
    let failed = |outcome| SmResumeTerminal::failed(stream_id.clone(), outcome);
    match rejection.reason() {
        ResumeRejectionReason::UnknownSession
        | ResumeRejectionReason::Abandoned
        | ResumeRejectionReason::Superseded
        | ResumeRejectionReason::Denied(ResumeDenial::Terminal(_) | ResumeDenial::Expired) => {
            failed(SmResumeOutcome::NotFound)
        }
        ResumeRejectionReason::Denied(ResumeDenial::PrincipalUnavailable) => {
            failed(SmResumeOutcome::PrincipalUnavailable)
        }
        ResumeRejectionReason::PrincipalChanged
        | ResumeRejectionReason::Denied(
            ResumeDenial::PrincipalDiverged | ResumeDenial::PrincipalMismatch,
        ) => failed(SmResumeOutcome::IdentityMismatch),
        ResumeRejectionReason::Denied(ResumeDenial::Handled(HandledError::TooHigh {
            h,
            ceiling,
        })) => SmResumeTerminal::handled_too_high(stream_id.clone(), *h, *ceiling),
        // A count behind the floor, or one that is neither ahead nor behind,
        // cannot be replayed from: the client is told the server's count and
        // binds afresh.
        ResumeRejectionReason::Denied(ResumeDenial::Handled(
            HandledError::Behind { .. } | HandledError::Ambiguous { .. },
        )) => match full_jid {
            Some(full_jid) => SmResumeTerminal::replay_gap(
                stream_id.clone(),
                rejection.handled_floor().unwrap_or_default(),
                full_jid.clone(),
                client_h,
                None,
            ),
            None => failed(SmResumeOutcome::ReplayGap),
        },
        ResumeRejectionReason::ClaimFenceMissing
        | ResumeRejectionReason::Denied(ResumeDenial::GenerationExhausted) => {
            failed(SmResumeOutcome::OwnerUnreachable)
        }
        ResumeRejectionReason::PrincipalStandingUnavailable | ResumeRejectionReason::Store(_) => {
            failed(SmResumeOutcome::Storage)
        }
        ResumeRejectionReason::OperationReused => failed(SmResumeOutcome::Internal),
    }
}

/// The session a failed attempt is reported under when its token named no
/// session. The presented value is never logged: it may be a live token of
/// another account.
fn unnamed_session() -> SmSessionId {
    SmSessionId::new("unknown-durable-session")
}

/// Resume a durable session on this connection.
///
/// The token is looked up, the stored principal's standing is resolved, and
/// the resume commits the next generation to a binding identity minted for
/// this attempt. A local connection that still owns the session is asked to
/// detach first so its resource snapshot exists; the commit does not depend
/// on the answer. From the commit the connection is staged for publication.
pub(super) async fn resume(
    store: &DurableSmStore,
    resume: SmResume,
    state: &WebSocketState,
    ctx: SmCtx<'_>,
) -> DurableResumeStep {
    let SmCtx {
        binding_ref,
        bindings,
        #[cfg(test)]
        resume_binding_attempts,
        phase,
        resume_endpoint_handoff_blocked,
        sm_state,
        sm_ingress_fence,
        authenticated_session,
        occupancy_session,
        carbons_enabled,
        presence_available,
        presence_show,
        presence_status,
        presence_priority,
        presence_payloads,
        pending_subscribes_flushed,
        pending_resume_stream_id,
        pending_resume_h,
        pending_resume_claim: _,
        #[cfg(test)]
            pre_final_principal_recheck_test_hook: _,
        suppress_sm_record_next_batch,
        roster_interested,
        blocklist_interested,
        pending_sm_enable_commit: _,
        sm_inbound_completion: _,
        durable_sm,
    } = ctx;
    let failed = |stream_id: SmSessionId, outcome| {
        DurableResumeStep::Failed(SmResumeTerminal::failed(stream_id, outcome))
    };
    let ConnectionPhase::Authenticated { bare_jid } = &*phase else {
        return failed(unnamed_session(), SmResumeOutcome::UnexpectedRequest);
    };
    let bare_jid = bare_jid.clone();
    if resume_endpoint_handoff_blocked || !durable_sm.admits_enable() {
        return failed(unnamed_session(), SmResumeOutcome::UnexpectedRequest);
    }
    let presented = match authenticated_session
        .as_ref()
        .map(Session::authenticated_principal_ref)
    {
        Some(Ok(presented)) => presented,
        Some(Err(_)) | None => {
            return failed(unnamed_session(), SmResumeOutcome::PrincipalUnavailable);
        }
    };

    // One identity per attempt, minted before any asynchronous work. A failed
    // attempt drops it; neither case reuses it later.
    *binding_ref = None;
    let attempt_binding = match bindings.prepare_next() {
        Ok(prepared) => prepared.attempt.binding_ref,
        Err(refusal) => {
            error!(%refusal, "resume attempt could not mint a binding identity");
            return failed(unnamed_session(), SmResumeOutcome::Internal);
        }
    };
    #[cfg(test)]
    resume_binding_attempts.push(attempt_binding.clone());

    let Ok(operation) = ResumeOperation::for_previd(&resume.previd) else {
        return failed(unnamed_session(), SmResumeOutcome::NotFound);
    };
    let candidate = match store.resume_candidate(&operation).await {
        Ok(Some(candidate)) => candidate,
        Ok(None) => return failed(unnamed_session(), SmResumeOutcome::NotFound),
        Err(error) => {
            warn!(%error, "durable SM resume candidate could not be read");
            return failed(unnamed_session(), SmResumeOutcome::Storage);
        }
    };
    let stream_id = SmSessionId::new(candidate.session.as_uuid().to_string());
    if candidate.full_jid.to_bare() != bare_jid {
        return DurableResumeStep::Failed(SmResumeTerminal::identity_mismatch(
            stream_id,
            candidate.full_jid,
        ));
    }

    // Cleanup, not the fence: a connection on this node that still owns the
    // session detaches so its resource snapshot is stored. Whatever it
    // answers, the commit below decides ownership.
    {
        use crate::clustering::resume_bridge::{
            request_validated_local_resume, LocalForcedDetachOutcome,
        };
        let shutdown = state.deps.shutdown.stop_token();
        let outcome = tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                return failed(stream_id, SmResumeOutcome::ShutdownAbandoned);
            }
            outcome = request_validated_local_resume(
                &state.deps.protocol.connection_registry,
                &stream_id,
                &bare_jid,
                resume.h,
            ) => outcome,
        };
        if matches!(outcome, Ok(LocalForcedDetachOutcome::IdentityMismatch)) {
            return failed(stream_id, SmResumeOutcome::IdentityMismatch);
        }
    }

    // The session claim this node must hold for the commit. A claim another
    // node holds is not taken over here: the resume is refused and the
    // client binds afresh.
    let registry = &state.deps.protocol.sm_session_registry;
    let Some(_claim_publication) = registry.ensure_session_claim(stream_id.as_str()).await else {
        warn!(%stream_id, "durable SM resume refused: the session claim is held elsewhere");
        return DurableResumeStep::Failed(SmResumeTerminal::failed_cross_node(
            stream_id,
            SmResumeOutcome::OwnerUnreachable,
        ));
    };
    let claim = registry.current_sm_claim_fence(stream_id.as_str());

    let inbound_checkpoint = match state
        .deps
        .protocol
        .ingress
        .load_resume_checkpoint(&stream_id)
        .await
    {
        Ok(Some(checkpoint)) => checkpoint.to_storage(),
        Ok(None) | Err(_) => return failed(stream_id, SmResumeOutcome::Storage),
    };
    let resolution = state
        .deps
        .auth_state
        .session_manager
        .resolve_principal(&candidate.principal)
        .await;
    let Some(standing) = crate::sm_authority::principal_standing(&resolution) else {
        return failed(stream_id, SmResumeOutcome::Storage);
    };

    // The attempt holds the connection from here until its outcome is known.
    *durable_sm = DurableSmLink::ResumeUnresolved(Box::new(UnresolvedResume {
        operation,
        binding: attempt_binding,
        presented,
        claim,
        h: resume.h,
        stream_id: stream_id.clone(),
        inbound_checkpoint,
        retry: SettlementRetry::first(),
    }));
    let DurableSmLink::ResumeUnresolved(pending) = &*durable_sm else {
        return failed(stream_id, SmResumeOutcome::Internal);
    };
    let committed = store
        .resume(
            &pending.operation,
            ResumeRequest {
                binding: &pending.binding,
                presented: &pending.presented,
                resolved: &candidate.principal,
                standing,
                h: pending.h,
                claim: pending.claim.as_ref(),
                now: chrono::Utc::now(),
            },
        )
        .await;
    match committed {
        Ok(commit) => {
            let DurableSmLink::ResumeUnresolved(pending) = std::mem::take(durable_sm) else {
                return failed(stream_id, SmResumeOutcome::Internal);
            };
            close_superseded_local_binding(state, &commit, &bare_jid);
            adopt_committed_resume(
                store.clone(),
                ResumeTargets {
                    durable_sm,
                    binding_ref,
                    phase,
                    sm_state,
                    sm_ingress_fence,
                    occupancy_session,
                    carbons_enabled,
                    roster_interested,
                    blocklist_interested,
                    presence_available,
                    presence_show,
                    presence_status,
                    presence_priority,
                    presence_payloads,
                    pending_subscribes_flushed,
                    pending_resume_stream_id,
                    pending_resume_h,
                    suppress_sm_record_next_batch,
                },
                *pending,
                commit,
            );
            DurableResumeStep::Staged
        }
        Err(ResumeError::Rejected(rejection)) => {
            info!(%stream_id, error = %rejection, "durable SM resume rejected");
            let terminal =
                rejection_terminal(stream_id, Some(&candidate.full_jid), resume.h, &rejection);
            durable_sm.release_resume_hold(rejection.fresh_bind_permit());
            if matches!(terminal.outcome(), SmResumeOutcome::HandledTooHigh) {
                *phase = ConnectionPhase::closing(None);
            }
            DurableResumeStep::Failed(terminal)
        }
        Err(ResumeError::Indeterminate(cause)) => {
            warn!(
                %stream_id,
                error = %cause,
                "durable SM resume commit outcome is unknown; holding the connection"
            );
            DurableResumeStep::Staged
        }
    }
}

/// Settle a resume whose commit outcome is unknown. A committed resume
/// stages the connection for publication exactly as one that committed in
/// its own turn; a known rejection releases the hold. Safe to drop and call
/// again: the resolution is idempotent under the operation id.
pub(in crate::server::routes::websocket) async fn settle_unresolved_resume(
    state: &WebSocketState,
    conn: &mut super::super::state::WsConnState,
) -> Option<DurableResumeSettlement> {
    let DurableSmLink::ResumeUnresolved(unresolved) = &mut conn.durable_sm else {
        return None;
    };
    let store = selected_store(state)?.clone();
    let resolution = store
        .resolve_resume(
            &unresolved.operation,
            &unresolved.binding,
            chrono::Utc::now(),
        )
        .await;
    let resolution = match resolution {
        Ok(resolution) => resolution,
        Err(cause) => {
            unresolved.retry.back_off();
            warn!(
                stream_id = %unresolved.stream_id,
                error = %cause,
                "durable SM resume is still unresolved"
            );
            return None;
        }
    };
    let DurableSmLink::ResumeUnresolved(unresolved) = std::mem::take(&mut conn.durable_sm) else {
        return None;
    };
    match resolution {
        ResumeResolution::Committed(commit) => {
            adopt_committed_resume(
                store,
                ResumeTargets {
                    durable_sm: &mut conn.durable_sm,
                    binding_ref: &mut conn.binding_ref,
                    phase: &mut conn.phase,
                    sm_state: &mut conn.sm_state,
                    sm_ingress_fence: &mut conn.sm_ingress_fence,
                    occupancy_session: &mut conn.occupancy_session,
                    carbons_enabled: &mut conn.carbons_enabled,
                    roster_interested: &mut conn.roster_interested,
                    blocklist_interested: &mut conn.blocklist_interested,
                    presence_available: &mut conn.presence_available,
                    presence_show: &mut conn.presence_show,
                    presence_status: &mut conn.presence_status,
                    presence_priority: &mut conn.presence_priority,
                    presence_payloads: &mut conn.presence_payloads,
                    pending_subscribes_flushed: &mut conn.pending_subscribes_flushed,
                    pending_resume_stream_id: &mut conn.pending_resume_stream_id,
                    pending_resume_h: &mut conn.pending_resume_h,
                    suppress_sm_record_next_batch: &mut conn.suppress_sm_record_next_batch,
                },
                *unresolved,
                *commit,
            );
            Some(DurableResumeSettlement::Staged)
        }
        ResumeResolution::Rejected(rejection) => {
            let terminal =
                rejection_terminal(unresolved.stream_id.clone(), None, unresolved.h, &rejection);
            // The hold ended with the link above; the permit records why the
            // client may now bind.
            let _known: FreshBindPermit = rejection.fresh_bind_permit();
            if matches!(terminal.outcome(), SmResumeOutcome::HandledTooHigh) {
                conn.phase = ConnectionPhase::closing(None);
            }
            super::observe::observe_sm_resume(&terminal);
            conn.pending_finalized_resume_outcome = Some(terminal.outcome());
            Some(DurableResumeSettlement::Failed(terminal.into_frames()))
        }
    }
}

/// What settling an unresolved resume produced.
pub(in crate::server::routes::websocket) enum DurableResumeSettlement {
    /// The resume committed; the connection is staged for publication.
    Staged,
    /// The resume is known not to have happened; the frames answer it.
    Failed(Vec<ResponseFrame>),
}

/// At teardown, settle a resume whose outcome is still unknown and return a
/// session it turns out to own to `Detached`.
pub(in crate::server::routes::websocket) async fn release_unresolved_resume(
    state: &WebSocketState,
    link: &mut DurableSmLink,
) {
    if !matches!(link, DurableSmLink::ResumeUnresolved(_)) {
        return;
    }
    let DurableSmLink::ResumeUnresolved(unresolved) = std::mem::take(link) else {
        return;
    };
    let Some(store) = selected_store(state) else {
        return;
    };
    match store
        .resolve_resume(
            &unresolved.operation,
            &unresolved.binding,
            chrono::Utc::now(),
        )
        .await
    {
        Ok(ResumeResolution::Committed(commit)) => {
            *link = DurableSmLink::Hydrating(Box::new(HydratingResume {
                store: store.clone(),
                commit: *commit,
                claim: unresolved.claim,
                presented: unresolved.presented,
                h: unresolved.h,
            }));
            link.abandon_hydration().await;
        }
        Ok(ResumeResolution::Rejected(_)) => {}
        Err(error) => {
            error!(
                stream_id = %unresolved.stream_id,
                %error,
                "a durable SM resume ended with its commit outcome unknown"
            );
        }
    }
}
