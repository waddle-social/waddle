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
use waddle_xmpp::stream_management::durable::{Exposure, HandledError};
use waddle_xmpp::stream_management::persistence::{SmClaimFence, SmReplayPayload};
use waddle_xmpp::stream_management::{
    SmFailed, SmFrameProvenance, SmIngressFrameReceipt, StreamManagementState,
};
use waddle_xmpp::telemetry::attributes::SmEnableOutcome;
use waddle_xmpp::telemetry::authority::record_sm_enable;

use super::implementation::{SmEnableClaimGuard, SmEnableCommit};
use super::{ResponseFrame, Session, WebSocketState};
use crate::sm_authority::{
    AdmissionAuthority, AdmissionError, AdmissionFrame, AdmissionRejection, AdmissionRequest,
    CommittedObligation, DurableSmStore, EnableError, EnableOperation, EnableRejection,
    EnableRequest, EnableResolution, EnabledCommit, OwnerError, OwnerRejection,
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
            Self::Unbound | Self::Owner(_) | Self::Superseded => None,
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
