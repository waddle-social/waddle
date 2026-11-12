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
use waddle_xmpp::ownership::BindingRef;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::durable::OwnerFence;
use waddle_xmpp::stream_management::persistence::SmClaimFence;
use waddle_xmpp::stream_management::SmFailed;
use waddle_xmpp::telemetry::attributes::SmEnableOutcome;
use waddle_xmpp::telemetry::authority::record_sm_enable;

use super::implementation::{SmEnableClaimGuard, SmEnableCommit};
use super::{ResponseFrame, Session, WebSocketState};
use crate::sm_authority::{
    DurableSmStore, EnableError, EnableOperation, EnableRejection, EnableRequest, EnableResolution,
    EnabledCommit,
};

/// First wait before an operation with an unknown commit outcome is settled.
const SETTLEMENT_FIRST_DELAY: Duration = Duration::from_millis(100);
/// Longest wait between two settlement attempts.
const SETTLEMENT_MAX_DELAY: Duration = Duration::from_secs(5);

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
}

/// The owner fence a connection presents on each authority call, with the
/// claim the authority compares it against.
#[derive(Clone)]
pub(in crate::server::routes::websocket) struct DurableSmOwner {
    store: DurableSmStore,
    fence: OwnerFence,
    /// This node's claim on the session. Absent only where the legacy claim
    /// bookkeeping holds none; a clustered store then refuses each write.
    claim: Option<SmClaimFence>,
}

/// An enable attempt between its commit and a known outcome.
pub(in crate::server::routes::websocket) struct UnresolvedEnable {
    operation: EnableOperation,
    binding: BindingRef,
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
            Self::Unbound | Self::Owner(_) => None,
        }
    }

    /// Whether an `<enable/>` may start: the connection neither owns a
    /// durable session nor waits on an enable's outcome.
    pub(in crate::server::routes::websocket) fn admits_enable(&self) -> bool {
        matches!(self, Self::Unbound)
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
