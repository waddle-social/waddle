//! Authorized arbitrary-node resume as one transaction.

use chrono::{DateTime, Utc};
use jid::FullJid;
use thiserror::Error;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ownership::{BindingRef, SmSessionKey};
use waddle_xmpp::stream_management::durable::{
    authorize_resume, transition, DurableSessionView, DurableSmEvent, DurableSmState,
    HandledDecision, OwnerFence, PrincipalStanding, ResumeAttempt, ResumeDenial, SmOperationId,
    SmOrdinal, SmResumeToken, SmResumeTokenError,
};
use waddle_xmpp::stream_management::persistence::{PersistedSession, SmClaimFence};
use waddle_xmpp::stream_management::SmResumed;

use super::custody::{
    load_frames, release_acknowledged, OwnerError, OwnerRejection, RetainedFrame,
};
use super::error::DurableSmError;
use super::operations::{self, OperationKind, OperationOutcome};
use super::record::{self, DurableSessionRecord, RecordedClaim};
use super::store::DurableSmStore;
use crate::db::Transaction;
use crate::sm_persistence::durable_rows;

/// Identity of one resume attempt: the operation id and the token the
/// client presented. Minted before the commit and kept until its outcome is
/// known.
#[derive(Debug, Clone)]
pub struct ResumeOperation {
    id: SmOperationId,
    token: SmResumeToken,
}

impl ResumeOperation {
    /// Start a resume for the `previd` of a `<resume/>`. A value that is not
    /// a token this server issues names no session.
    pub fn for_previd(previd: &str) -> Result<Self, SmResumeTokenError> {
        Ok(Self {
            id: SmOperationId::mint(),
            token: SmResumeToken::from_wire(previd)?,
        })
    }

    pub fn id(&self) -> SmOperationId {
        self.id
    }
}

/// The session a resume token names, read without locking. The caller
/// resolves `principal` against the session authority before resuming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeCandidate {
    pub session: SmSessionKey,
    pub full_jid: FullJid,
    /// The stored principal reference.
    pub principal: AuthenticatedPrincipalRef,
}

/// What the resuming connection presents.
#[derive(Debug, Clone, Copy)]
pub struct ResumeRequest<'a> {
    /// The resume attempt that will own the next generation.
    pub binding: &'a BindingRef,
    /// The resuming connection's authenticated principal reference.
    pub presented: &'a AuthenticatedPrincipalRef,
    /// The stored reference the caller resolved, from [`ResumeCandidate`].
    pub resolved: &'a AuthenticatedPrincipalRef,
    /// How `resolved` stands against the session authority.
    pub standing: PrincipalStanding,
    /// The `h` of `<resume/>`.
    pub h: u32,
    /// This node's claim on the session; required on a clustered store.
    pub claim: Option<&'a SmClaimFence>,
    pub now: DateTime<Utc>,
}

/// Proof that a resume committed the next generation to this binding.
/// `<resumed/>` is built from it, and it carries everything the new owner
/// hydrates from.
#[must_use = "the resume committed; complete or compensate it under this witness"]
#[derive(Debug)]
pub struct ResumedCommit {
    fence: OwnerFence,
    token: SmResumeToken,
    full_jid: FullJid,
    inbound_handled: u64,
    handled: HandledDecision,
    replay: Vec<RetainedFrame>,
    replay_through: SmOrdinal,
    resource: Option<PersistedSession>,
    superseded: Option<BindingRef>,
}

impl ResumedCommit {
    /// The owner fence of the new generation.
    pub fn fence(&self) -> &OwnerFence {
        &self.fence
    }

    pub fn full_jid(&self) -> &FullJid {
        &self.full_jid
    }

    /// The handled count of client stanzas the previous owner last recorded.
    /// The canonical ingress checkpoint may be ahead of it.
    pub fn inbound_handled(&self) -> u64 {
        self.inbound_handled
    }

    /// What the resume's own handled count settled.
    pub fn handled(&self) -> HandledDecision {
        self.handled
    }

    /// The frames to resend, in ordinal order, before any later frame.
    pub fn replay(&self) -> &[RetainedFrame] {
        &self.replay
    }

    /// The resource state stored when the session detached: presence,
    /// carbons, roster and blocklist interest and the occupancy session.
    /// Absent when the previous owner never detached.
    pub fn resource(&self) -> Option<&PersistedSession> {
        self.resource.as_ref()
    }

    /// The binding that owned the session when this resume took it over.
    /// Its transport may be closed as cleanup; it is already fenced.
    pub fn superseded_binding(&self) -> Option<&BindingRef> {
        self.superseded.as_ref()
    }

    /// The `<resumed/>` reply, carrying `h` as the server's handled count.
    pub fn resumed_reply(&self, h: u32) -> SmResumed {
        SmResumed::new(self.token.to_wire(), h)
    }
}

/// Why a resume is known not to have happened.
#[derive(Debug, Error)]
pub enum ResumeRejectionReason {
    #[error("the resume token names no session")]
    UnknownSession,
    #[error(transparent)]
    Denied(#[from] ResumeDenial),
    #[error("the stored principal changed after the caller resolved it")]
    PrincipalChanged,
    /// The session authority could not be read, so the stored principal's
    /// standing is unknown and the resume transaction was not run.
    #[error("the stored principal's standing could not be resolved")]
    PrincipalStandingUnavailable,
    #[error("this node does not hold the session claim it named")]
    ClaimFenceMissing,
    #[error("the operation was resolved as abandoned")]
    Abandoned,
    #[error("the operation id already belongs to another operation or owner")]
    OperationReused,
    #[error("the resume committed and a later resume superseded it")]
    Superseded,
    #[error(transparent)]
    Store(#[from] DurableSmError),
}

/// A known rejection. Only this value yields the permit for a fresh bind.
#[derive(Debug, Error)]
#[error("resume rejected: {reason}")]
pub struct ResumeRejection {
    reason: ResumeRejectionReason,
    handled_floor: Option<u32>,
}

impl ResumeRejection {
    /// A rejection decided without a locked session, so no handled count is
    /// known.
    pub(super) fn new(reason: impl Into<ResumeRejectionReason>) -> Self {
        Self {
            reason: reason.into(),
            handled_floor: None,
        }
    }

    fn for_session(
        reason: impl Into<ResumeRejectionReason>,
        stored: &DurableSessionRecord,
    ) -> Self {
        Self {
            reason: reason.into(),
            handled_floor: Some(stored.inbound_handled as u32),
        }
    }

    pub fn reason(&self) -> &ResumeRejectionReason {
        &self.reason
    }

    /// The server's handled count for the `h` of `<failed/>`, when the
    /// session is known.
    pub fn handled_floor(&self) -> Option<u32> {
        self.handled_floor
    }

    /// The resume is known not to own the session, so the connection may
    /// bind a fresh resource.
    pub fn fresh_bind_permit(&self) -> FreshBindPermit {
        FreshBindPermit { _known: () }
    }
}

/// Evidence that a resume was rejected with a known outcome. An
/// indeterminate resume cannot produce one.
#[derive(Debug)]
pub struct FreshBindPermit {
    _known: (),
}

#[derive(Debug, Error)]
pub enum ResumeError {
    #[error(transparent)]
    Rejected(#[from] ResumeRejection),
    /// The commit outcome is unknown. The caller keeps its operation and
    /// settles it with [`DurableSmStore::resolve_resume`]. Until then it has
    /// no reply to send, no fence, and no permit for a fresh bind.
    #[error("the resume commit outcome is unknown")]
    Indeterminate(#[source] DurableSmError),
}

/// The known outcome of a previously indeterminate resume.
#[derive(Debug)]
pub enum ResumeResolution {
    Committed(Box<ResumedCommit>),
    Rejected(ResumeRejection),
}

/// Frames admitted while the resume hydrated, to write after the replay.
#[derive(Debug)]
pub struct HydrationTail {
    pub frames: Vec<RetainedFrame>,
}

impl DurableSmStore {
    /// Look up the session a resume token names.
    pub async fn resume_candidate(
        &self,
        operation: &ResumeOperation,
    ) -> Result<Option<ResumeCandidate>, DurableSmError> {
        let mut tx = self.db.begin().await?;
        let stored = record::load_by_token(&mut tx, &operation.token.digest(), false).await?;
        tx.rollback().await?;
        Ok(stored.map(|stored| ResumeCandidate {
            session: stored.session,
            full_jid: stored.full_jid,
            principal: stored.principal,
        }))
    }

    /// Commit the next generation of the session to `request.binding`.
    ///
    /// One transaction locks the session, authorizes the principal,
    /// validates and settles the handled count against the exposure
    /// ceiling, stores the new generation, binding, principal and claim,
    /// and moves the session to `TransferHydrating`. From that commit the
    /// previous owner's fence no longer matches, whether or not its
    /// transport is ever closed.
    pub async fn resume(
        &self,
        operation: &ResumeOperation,
        request: ResumeRequest<'_>,
    ) -> Result<ResumedCommit, ResumeError> {
        let claim = self
            .write_claim(request.claim)
            .map_err(ResumeRejection::new)?;
        let mut tx = self.begin_write().await.map_err(ResumeRejection::new)?;
        let mut stored = record::load_by_token(&mut tx, &operation.token.digest(), true)
            .await
            .map_err(ResumeRejection::new)?
            .ok_or_else(|| ResumeRejection::new(ResumeRejectionReason::UnknownSession))?;
        if let Some(claim) = claim {
            if !self
                .claim_is_held(&mut tx, stored.session, claim)
                .await
                .map_err(|error| ResumeRejection::for_session(error, &stored))?
            {
                return Err(ResumeRejection::for_session(
                    ResumeRejectionReason::ClaimFenceMissing,
                    &stored,
                )
                .into());
            }
        }
        if let Some(existing) = operations::find(&mut tx, operation.id)
            .await
            .map_err(|error| ResumeRejection::for_session(error, &stored))?
        {
            let outcome = committed_resume(&mut tx, existing, &stored, operation, request.binding)
                .await
                .map_err(|error| ResumeRejection::for_session(error, &stored))?;
            tx.rollback().await.map_err(|error| {
                ResumeRejection::for_session(DurableSmError::from(error), &stored)
            })?;
            return match outcome {
                ResumeResolution::Committed(witness) => Ok(*witness),
                ResumeResolution::Rejected(rejection) => Err(rejection.into()),
            };
        }
        if *request.resolved != stored.principal {
            return Err(ResumeRejection::for_session(
                ResumeRejectionReason::PrincipalChanged,
                &stored,
            )
            .into());
        }
        let grant = authorize_resume(
            &DurableSessionView {
                state: stored.state,
                generation: stored.generation,
                principal: &stored.principal,
                window: stored.window,
                detached_at: stored.detached_at,
                max_resume: stored.max_resume,
            },
            &ResumeAttempt {
                presented: request.presented,
                stored_standing: request.standing,
                h: request.h,
                now: request.now,
            },
        )
        .map_err(|denial| ResumeRejection::for_session(denial, &stored))?;

        if let HandledDecision::Advanced { through, .. } = grant.handled {
            release_acknowledged(&mut tx, &mut stored, grant.window, through)
                .await
                .map_err(|error| ResumeRejection::for_session(error, &stored))?;
        }
        let superseded = stored.state.has_owner().then(|| stored.binding.clone());
        stored.state = grant.state;
        stored.generation = grant.generation;
        stored.binding = request.binding.clone();
        stored.principal = request.presented.clone();
        stored.claim = claim.map(RecordedClaim::from_fence);
        // A session taken from a live owner had no detach time. Recording one
        // lets a hydration that is never completed expire like any other
        // detached session.
        stored.detached_at.get_or_insert(request.now);
        stored.last_operation = operation.id;
        let hydration = async {
            record::update(&mut tx, &stored, request.now).await?;
            operations::record_committed(
                &mut tx,
                operation.id,
                stored.session,
                OperationKind::Resume,
                stored.generation,
                request.now,
            )
            .await?;
            hydrate(&mut tx, &stored).await
        };
        let (replay, resource) = hydration
            .await
            .map_err(|error| ResumeRejection::for_session(error, &stored))?;
        match tx.commit().await {
            Ok(()) => Ok(ResumedCommit {
                fence: stored.fence(),
                token: operation.token.clone(),
                full_jid: stored.full_jid,
                inbound_handled: stored.inbound_handled,
                handled: grant.handled,
                replay,
                replay_through: stored.window.appended(),
                resource,
                superseded,
            }),
            Err(cause) => Err(ResumeError::Indeterminate(cause.into())),
        }
    }

    /// Settle a resume whose commit outcome is unknown, by the same
    /// abandonment marker that settles an enable. A committed resume returns
    /// its witness rebuilt from the stored session. An error means the
    /// outcome is still unknown; resolve again.
    pub async fn resolve_resume(
        &self,
        operation: &ResumeOperation,
        binding: &BindingRef,
        now: DateTime<Utc>,
    ) -> Result<ResumeResolution, DurableSmError> {
        let mut tx = self.begin_write().await?;
        let Some(stored) = record::load_by_token(&mut tx, &operation.token.digest(), true).await?
        else {
            // A resume commits only against a stored session, and a session
            // row outlives every operation that names it.
            tx.rollback().await?;
            return Ok(ResumeResolution::Rejected(ResumeRejection::new(
                ResumeRejectionReason::UnknownSession,
            )));
        };
        let row = operations::abandon(
            &mut tx,
            operation.id,
            stored.session,
            OperationKind::Resume,
            stored.generation,
            now,
        )
        .await?;
        let resolution = committed_resume(&mut tx, row, &stored, operation, binding).await?;
        // The marker must be durable before a rejection is reported.
        tx.commit().await?;
        Ok(resolution)
    }

    /// Publish the resumed session as `Live`.
    ///
    /// The exposure ceiling rises over every retained frame in the same
    /// commit, so it is durable before the replay is written. The returned
    /// tail holds the frames admitted during hydration; the caller writes
    /// the witness's replay, then the tail, then accepts live frames. The
    /// detached resource snapshot is consumed.
    pub async fn complete_resume(
        &self,
        resumed: &ResumedCommit,
        claim: Option<&SmClaimFence>,
        now: DateTime<Utc>,
    ) -> Result<HydrationTail, OwnerError> {
        let (mut tx, mut stored) = self.lock_owned(&resumed.fence, claim).await?;
        let tail = async {
            let frames = load_frames(&mut tx, stored.session, stored.window.replay()).await?;
            Ok::<_, DurableSmError>(
                frames
                    .into_iter()
                    .filter(|frame| frame.ordinal > resumed.replay_through)
                    .collect::<Vec<_>>(),
            )
        }
        .await
        .map_err(OwnerRejection::from)?;
        if stored.state == DurableSmState::Live {
            // A repeated completion after an unknown commit.
            tx.rollback()
                .await
                .map_err(|error| OwnerRejection::Store(error.into()))?;
            return Ok(HydrationTail { frames: tail });
        }
        stored.state = transition(stored.state, DurableSmEvent::ResumeCompleted)
            .map_err(|_| OwnerRejection::NotOwned(stored.state))?;
        stored.window = stored.window.expose_all();
        stored.detached_at = None;
        let publish = async {
            durable_rows::delete_resource_snapshot(&mut tx, &record::stream_id(stored.session))
                .await?;
            record::update(&mut tx, &stored, now).await
        };
        publish.await.map_err(OwnerRejection::from)?;
        tx.commit()
            .await
            .map_err(|error| OwnerError::Indeterminate(error.into()))?;
        Ok(HydrationTail { frames: tail })
    }
}

/// The replay frames and the detached resource snapshot of a session.
async fn hydrate(
    tx: &mut Transaction<'_>,
    stored: &DurableSessionRecord,
) -> Result<(Vec<RetainedFrame>, Option<PersistedSession>), DurableSmError> {
    let replay = load_frames(tx, stored.session, stored.window.replay()).await?;
    let resource =
        durable_rows::load_resource_snapshot(tx, &record::stream_id(stored.session)).await?;
    Ok((replay, resource))
}

/// Interpret an operation row for a resume by `binding`.
async fn committed_resume(
    tx: &mut Transaction<'_>,
    row: operations::OperationRow,
    stored: &DurableSessionRecord,
    operation: &ResumeOperation,
    binding: &BindingRef,
) -> Result<ResumeResolution, DurableSmError> {
    if row.kind != OperationKind::Resume || row.session != stored.session {
        return Ok(ResumeResolution::Rejected(ResumeRejection::for_session(
            ResumeRejectionReason::OperationReused,
            stored,
        )));
    }
    if row.outcome == OperationOutcome::Abandoned {
        return Ok(ResumeResolution::Rejected(ResumeRejection::for_session(
            ResumeRejectionReason::Abandoned,
            stored,
        )));
    }
    let still_owner = stored.generation == row.generation
        && stored.binding == *binding
        && stored.state.has_owner();
    if !still_owner {
        return Ok(ResumeResolution::Rejected(ResumeRejection::for_session(
            ResumeRejectionReason::Superseded,
            stored,
        )));
    }
    let (replay, resource) = hydrate(tx, stored).await?;
    Ok(ResumeResolution::Committed(Box::new(ResumedCommit {
        fence: stored.fence(),
        token: operation.token.clone(),
        full_jid: stored.full_jid.clone(),
        inbound_handled: stored.inbound_handled,
        // The settlement happened in the original commit; the stored window
        // already reflects it.
        handled: HandledDecision::Unchanged,
        replay_through: stored.window.appended(),
        replay,
        resource,
        // The superseded binding is cleanup evidence only and is not stored.
        superseded: None,
    })))
}
