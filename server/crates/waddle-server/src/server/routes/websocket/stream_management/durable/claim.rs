//! The session claim a durable resume commits under.
//!
//! A clustered authority asserts the exact claim row of the session on every
//! write, so a resume commits only on the node that holds the claim. The
//! claim stays with the node that enabled or last resumed the session until
//! the session is terminal: a detach keeps it. A resume that arrives on
//! another node therefore moves the claim first.
//!
//! When the claim may move is decided from what the authority's row and the
//! claim row say together:
//!
//! - **The holder's node lease is committed as expired.** The holder cannot
//!   write. The claim moves by the owner-stale swap and the session's owner
//!   is recorded as lost.
//! - **The row is `Detached`.** No connection owns the session and its
//!   resource snapshot is stored. The claim moves by the identity-bound
//!   resume swap.
//! - **The row has an owner committed under a claim that is no longer on
//!   file.** That owner is fenced already. The claim moves by the resume
//!   swap and the owner is recorded as lost.
//! - **The row has an owner under the claim on file.** A connection on the
//!   holder may own the session. The holder is asked to detach it, so that
//!   it stores the resource snapshot and admits the frames it recorded
//!   without a transport. The claim moves once the row is `Detached`.
//!
//! A holder that does not answer inside the resume-handshake bound, or that
//! answers without the row becoming `Detached`, keeps its claim unless its
//! lapsed heartbeat lets its node lease be committed as expired. The resume
//! is then refused and the client binds afresh: nothing was written, and the
//! session stays resumable where it is.
//!
//! The claim swap is bound to the claim epoch observed before the holder was
//! asked, and is issued once. A detach or a resume on the holder leaves that
//! epoch unchanged, so a session resumed on the holder between this node's
//! last read of the row and its swap is taken from a live owner. The
//! authority covers that case as it covers every superseded owner: the
//! owner's next write fails its claim, and its connection retires.

use std::time::Duration;

use jid::BareJid;
use tracing::{info, warn};
use waddle_xmpp::ownership::{CurrentNodeIdentityGuard, SmSessionKey};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::durable::DurableSmState;
use waddle_xmpp::stream_management::persistence::SmClaimFence;
use waddle_xmpp::stream_management::{
    ForeignSessionClaim, ObservedSessionClaim, RemoteResumeAskOutcome, SessionClaimEvidence,
    SessionClaimTakeover,
};

use super::super::WebSocketState;
use crate::sm_authority::{
    DurableSessionRecord, DurableSmStore, LifecycleError, LifecycleRejection, RecordedClaim,
};

/// First wait between two looks at a holder that has not released the
/// session.
const FIRST_RETRY_DELAY: Duration = Duration::from_millis(100);
/// Longest wait between two looks.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(2);

/// The claim a resume holds while it commits.
pub(super) struct ResumeClaim {
    /// Keeps this node's identity from rotating until the commit is issued.
    _publication: CurrentNodeIdentityGuard,
    /// The fence the commit asserts. Absent only where the claim
    /// bookkeeping holds none; a clustered store then refuses the commit.
    pub(super) fence: Option<SmClaimFence>,
}

/// Why a resume has no claim to commit under. Nothing was committed to the
/// session in any of these cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClaimRefusal {
    /// Another node holds the claim and did not release the session inside
    /// the bound, or another resume moved the claim first.
    HeldElsewhere,
    /// The resuming account is not the session's.
    NotAuthorized,
    /// The session is terminal or unknown to the authority.
    SessionGone,
    /// The claim store or the authority could not be read or written.
    Unavailable,
    /// This node is shutting down.
    ShutdownAbandoned,
}

/// The session a resume needs the claim of, and who asks.
pub(super) struct ResumeClaimRequest<'a> {
    pub(super) store: &'a DurableSmStore,
    pub(super) session: SmSessionKey,
    pub(super) stream_id: &'a SmSessionId,
    /// The account the resuming connection authenticated as.
    pub(super) requester: &'a BareJid,
    /// The account the session is bound to.
    pub(super) session_account: &'a BareJid,
}

/// What the wait for a foreign claim ended with.
enum Transfer {
    /// No other node holds the claim any more.
    Free,
    /// The claim may be moved on this evidence.
    Move(ForeignSessionClaim, SessionClaimEvidence),
}

/// Hold the session's claim on this node for a resume, moving it from
/// another node when the rules in the module documentation allow.
///
/// Only the wait for the holder is raced against this node's shutdown: it
/// writes nothing here. The swap and the owner-loss record run to
/// completion.
pub(super) async fn claim_for_resume(
    state: &WebSocketState,
    request: ResumeClaimRequest<'_>,
) -> Result<ResumeClaim, ClaimRefusal> {
    let registry = &state.deps.protocol.sm_session_registry;
    let stream_id = request.stream_id.as_str();
    if let Some(publication) = registry.ensure_session_claim(stream_id).await {
        return Ok(ResumeClaim {
            _publication: publication,
            fence: registry.current_sm_claim_fence(stream_id),
        });
    }

    let shutdown = state.deps.shutdown.stop_token();
    let transfer = tokio::select! {
        biased;
        _ = shutdown.cancelled() => return Err(ClaimRefusal::ShutdownAbandoned),
        transfer = await_transferable(state, &request) => transfer?,
    };
    let (claim, evidence) = match transfer {
        Transfer::Free => {
            let Some(publication) = registry.ensure_session_claim(stream_id).await else {
                return Err(ClaimRefusal::HeldElsewhere);
            };
            return Ok(ResumeClaim {
                _publication: publication,
                fence: registry.current_sm_claim_fence(stream_id),
            });
        }
        Transfer::Move(claim, evidence) => (claim, evidence),
    };

    let takeover = registry
        .take_session_claim(&claim, request.requester, request.session_account, evidence)
        .await;
    let (publication, fence) = match takeover {
        SessionClaimTakeover::Taken { publication, fence } => (publication, fence),
        SessionClaimTakeover::NotAuthorized => return Err(ClaimRefusal::NotAuthorized),
        SessionClaimTakeover::Lost => {
            info!(%stream_id, "durable SM resume lost the session claim to another taker");
            return Err(ClaimRefusal::HeldElsewhere);
        }
        SessionClaimTakeover::Unavailable => return Err(ClaimRefusal::Unavailable),
    };
    info!(
        %stream_id,
        from_node = %claim.owner().node_id,
        ?evidence,
        "durable SM session claim moved to this node for a resume"
    );
    // The claim stays recorded on this node whatever follows: a session
    // whose resume is refused below is resumed or expired under it later.
    record_previous_owner_lost(&request, &fence).await?;
    Ok(ResumeClaim {
        _publication: publication,
        fence: Some(fence),
    })
}

/// Wait, inside the resume-handshake bound, until the claim another node
/// holds may be moved.
async fn await_transferable(
    state: &WebSocketState,
    request: &ResumeClaimRequest<'_>,
) -> Result<Transfer, ClaimRefusal> {
    let registry = &state.deps.protocol.sm_session_registry;
    let stream_id = request.stream_id.as_str();
    let budget = state
        .deps
        .app_state
        .clustering_claims
        .resume_handshake_timeout()
        .unwrap_or(Duration::ZERO);
    let deadline = tokio::time::Instant::now() + budget;
    let mut delay = FIRST_RETRY_DELAY;
    // The epoch a takeover binds is the first one observed. A claim that
    // moves while the holder is asked belongs to whoever moved it.
    let mut first_observed: Option<ForeignSessionClaim> = None;
    loop {
        let observed = registry
            .observe_session_claim(stream_id)
            .await
            .map_err(|error| {
                warn!(%stream_id, %error, "durable SM resume could not read the session claim");
                ClaimRefusal::Unavailable
            })?;
        let current = match observed {
            ObservedSessionClaim::Unclaimed | ObservedSessionClaim::HeldHere => {
                return Ok(Transfer::Free);
            }
            ObservedSessionClaim::HeldElsewhere(current) => current,
        };
        let claim = first_observed
            .get_or_insert_with(|| current.clone())
            .clone();
        if current.owner() != claim.owner() || current.observed_epoch() != claim.observed_epoch() {
            info!(%stream_id, "durable SM resume refused: the session claim moved to another taker");
            return Err(ClaimRefusal::HeldElsewhere);
        }
        if !current.owner_lease_fresh() {
            return Ok(Transfer::Move(
                claim,
                SessionClaimEvidence::OwnerLeaseExpired,
            ));
        }

        let record = match request.store.session(request.session).await {
            Ok(Some(record)) => record,
            Ok(None) => return Err(ClaimRefusal::SessionGone),
            Err(error) => {
                warn!(%stream_id, %error, "durable SM resume could not read the session");
                return Err(ClaimRefusal::Unavailable);
            }
        };
        match record.state {
            // `Enabling` is never stored; no token names such a session.
            DurableSmState::Terminal(_) | DurableSmState::Enabling => {
                return Err(ClaimRefusal::SessionGone);
            }
            DurableSmState::Detached => {
                return Ok(Transfer::Move(claim, SessionClaimEvidence::OwnerReleased));
            }
            DurableSmState::Live | DurableSmState::TransferHydrating => {}
        }
        if !owner_committed_under(&record, &claim) {
            return Ok(Transfer::Move(claim, SessionClaimEvidence::OwnerReleased));
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let asked = tokio::time::timeout(
            remaining,
            registry.ask_session_owner_to_detach(&claim, request.requester),
        )
        .await;
        match asked {
            // No route to the holder is wired on this node.
            Ok(None) => break,
            Ok(Some(RemoteResumeAskOutcome::IdentityMismatch)) => {
                return Err(ClaimRefusal::NotAuthorized);
            }
            // The holder's connection detached: the row is read again now.
            Ok(Some(RemoteResumeAskOutcome::Detached)) => continue,
            // The holder found no connection for the session. One that is
            // still publishing itself, or that is detaching on its own,
            // shows on a later look.
            Ok(Some(RemoteResumeAskOutcome::NotLiveRemotely)) => {}
            Ok(Some(RemoteResumeAskOutcome::Unreachable)) | Err(_) => {
                if expire_lapsed_holder(state, &claim).await {
                    continue;
                }
            }
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(delay.min(remaining)).await;
        delay = (delay * 2).min(MAX_RETRY_DELAY);
    }
    warn!(
        %stream_id,
        "durable SM resume refused: the node holding the session claim did not release the session"
    );
    Err(ClaimRefusal::HeldElsewhere)
}

/// Whether the session's stored owner was committed under exactly `claim`.
/// An owner committed under any other claim row lost that row when the
/// claim moved, and can no longer write.
fn owner_committed_under(record: &DurableSessionRecord, claim: &ForeignSessionClaim) -> bool {
    record.claim.as_ref()
        == Some(&RecordedClaim {
            node_id: claim.owner().node_id.clone(),
            node_epoch: claim.owner().node_epoch.clone(),
            claim_epoch: claim.observed_epoch().0,
        })
}

/// Commit the holder's node lease as expired if its heartbeat has lapsed
/// past the lease TTL. This is the transition the stale-node watchdog
/// makes; a holder that still heartbeats is left as it is. Returns whether
/// the lease is now committed as expired.
#[cfg(feature = "clustering")]
async fn expire_lapsed_holder(state: &WebSocketState, claim: &ForeignSessionClaim) -> bool {
    let clustering = &state.deps.app_state.clustering_claims;
    let (Some(node_lease), Some(lease_ttl)) =
        (clustering.node_lease.as_ref(), clustering.lease_ttl)
    else {
        return false;
    };
    match node_lease.expire(claim.owner(), lease_ttl).await {
        Ok(expired) => expired,
        Err(error) => {
            warn!(
                node_id = %claim.owner().node_id,
                %error,
                "durable SM resume could not check the claim holder's node lease"
            );
            false
        }
    }
}

#[cfg(not(feature = "clustering"))]
async fn expire_lapsed_holder(_state: &WebSocketState, _claim: &ForeignSessionClaim) -> bool {
    false
}

/// Record the loss of the owner the claim was taken from, if the session
/// still has one, so that the session is `Detached` under this node's claim
/// whether or not the resume that follows commits.
async fn record_previous_owner_lost(
    request: &ResumeClaimRequest<'_>,
    fence: &SmClaimFence,
) -> Result<(), ClaimRefusal> {
    let stream_id = request.stream_id;
    let record = match request.store.session(request.session).await {
        Ok(Some(record)) => record,
        Ok(None) => return Err(ClaimRefusal::SessionGone),
        Err(error) => {
            warn!(%stream_id, %error, "durable SM resume could not read the session it claimed");
            return Err(ClaimRefusal::Unavailable);
        }
    };
    if !record.state.has_owner() {
        return Ok(());
    }
    match request
        .store
        .record_owner_lost(
            request.session,
            record.generation,
            fence,
            chrono::Utc::now(),
        )
        .await
    {
        // A session that detached or ended meanwhile has no owner to lose.
        Ok(())
        | Err(LifecycleError::Rejected(
            LifecycleRejection::Transition(_) | LifecycleRejection::UnknownSession,
        )) => Ok(()),
        Err(error) => {
            warn!(%stream_id, %error, "durable SM owner loss could not be recorded for a resume");
            Err(ClaimRefusal::Unavailable)
        }
    }
}
