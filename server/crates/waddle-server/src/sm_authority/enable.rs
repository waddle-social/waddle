//! Authoritative enablement: the session row commits before `<enabled/>`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use jid::FullJid;
use thiserror::Error;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ownership::{BindingRef, SessionGeneration, SmSessionKey};
use waddle_xmpp::stream_management::durable::{
    DurableSmState, OutboundWindow, OwnerFence, SmOperationId, SmResumeToken,
};
use waddle_xmpp::stream_management::persistence::SmClaimFence;
use waddle_xmpp::stream_management::SmEnabled;

use super::error::DurableSmError;
use super::operations::{self, OperationKind, OperationOutcome};
use super::record::{self, DurableSessionRecord, RecordedClaim};
use super::store::DurableSmStore;

/// Identity of one enable attempt, minted before the commit.
///
/// It fixes the session key and the resume token the commit will store, so a
/// commit whose outcome is unknown can be resolved, and if it committed, the
/// reply still carries the token that was stored.
#[derive(Debug, Clone)]
pub struct EnableOperation {
    id: SmOperationId,
    session: SmSessionKey,
    token: SmResumeToken,
}

impl EnableOperation {
    pub fn mint() -> Self {
        Self {
            id: SmOperationId::mint(),
            session: SmSessionKey::fresh(),
            token: SmResumeToken::mint(),
        }
    }

    pub fn id(&self) -> SmOperationId {
        self.id
    }

    pub fn session(&self) -> SmSessionKey {
        self.session
    }
}

/// What the binding that sent `<enable/>` presents.
#[derive(Debug, Clone, Copy)]
pub struct EnableRequest<'a> {
    /// The bind attempt that will own generation one.
    pub binding: &'a BindingRef,
    /// The connection's authenticated principal reference.
    pub principal: &'a AuthenticatedPrincipalRef,
    pub full_jid: &'a FullJid,
    pub max_resume: Duration,
    /// This node's claim on the session; required on a clustered store.
    pub claim: Option<&'a SmClaimFence>,
    pub now: DateTime<Utc>,
}

/// Proof that the session row committed. `<enabled/>` is built from it and
/// from nothing else, so the reply cannot precede the commit.
#[must_use = "the enable committed; the witness builds the <enabled/> reply and names the owner fence"]
#[derive(Debug)]
pub struct EnabledCommit {
    fence: OwnerFence,
    token: SmResumeToken,
    max_resume: Duration,
}

impl EnabledCommit {
    /// The owner fence of generation one.
    pub fn fence(&self) -> &OwnerFence {
        &self.fence
    }

    /// The `<enabled/>` reply for this session, with resumption offered.
    pub fn enabled_reply(&self) -> SmEnabled {
        let max = u32::try_from(self.max_resume.as_secs()).unwrap_or(u32::MAX);
        SmEnabled::with_resume(self.token.to_wire(), max)
    }
}

/// A known outcome: the enable did not commit and never will under this
/// operation. The caller may mint a new operation.
#[derive(Debug, Error)]
pub enum EnableRejection {
    #[error("the principal is not the bare JID of the bound resource")]
    ResourceMismatch,
    #[error("the minted resume token collides with a stored session")]
    ResumeTokenCollision,
    #[error("this node does not hold the session claim it named")]
    ClaimFenceMissing,
    #[error("the operation was resolved as abandoned")]
    Abandoned,
    #[error("the operation id already belongs to another operation or owner")]
    OperationReused,
    #[error(transparent)]
    Store(#[from] DurableSmError),
}

#[derive(Debug, Error)]
pub enum EnableError {
    #[error(transparent)]
    Rejected(#[from] EnableRejection),
    /// The commit outcome is unknown. The caller keeps its operation and
    /// settles it with [`DurableSmStore::resolve_enable`]. Until then it has
    /// no reply to send, no fence, and may not enable the binding again.
    #[error("the enable commit outcome is unknown")]
    Indeterminate(#[source] DurableSmError),
}

/// The known outcome of a previously indeterminate enable.
#[derive(Debug)]
pub enum EnableResolution {
    Committed(EnabledCommit),
    /// The enable committed, and the session has since left generation one
    /// or become terminal. The binding owns no session; no reply is due.
    Superseded,
    /// The enable did not commit and is now barred from committing.
    NotCommitted,
}

impl DurableSmStore {
    /// Commit a new session as `Live`, generation one, owned by
    /// `request.binding`, and return the witness for the reply.
    ///
    /// Calling again with the same operation after a success returns the
    /// same witness. A commit error is indeterminate; any earlier failure
    /// rolled back and is a known rejection. A caller whose call was
    /// cancelled before it returned treats the outcome as indeterminate too.
    pub async fn enable(
        &self,
        operation: &EnableOperation,
        request: EnableRequest<'_>,
    ) -> Result<EnabledCommit, EnableError> {
        if *request.principal.bare_jid() != request.full_jid.to_bare() {
            return Err(EnableRejection::ResourceMismatch.into());
        }
        let claim = self
            .write_claim(request.claim)
            .map_err(EnableRejection::from)?;
        let mut tx = self.begin_write().await.map_err(EnableRejection::from)?;
        if let Some(claim) = claim {
            if !self
                .claim_is_held(&mut tx, operation.session, claim)
                .await
                .map_err(EnableRejection::from)?
            {
                return Err(EnableRejection::ClaimFenceMissing.into());
            }
        }
        if let Some(existing) = operations::find(&mut tx, operation.id)
            .await
            .map_err(EnableRejection::from)?
        {
            let stored = record::load(&mut tx, operation.session)
                .await
                .map_err(EnableRejection::from)?;
            tx.rollback()
                .await
                .map_err(|error| EnableRejection::Store(error.into()))?;
            if existing.kind != OperationKind::Enable
                || existing.session != operation.session
                || existing.generation != first_generation()
            {
                return Err(EnableRejection::OperationReused.into());
            }
            return match existing.outcome {
                OperationOutcome::Abandoned => Err(EnableRejection::Abandoned.into()),
                OperationOutcome::Committed => {
                    match committed_witness(stored, operation, request.binding) {
                        Some(witness) => Ok(witness),
                        None => Err(EnableRejection::OperationReused.into()),
                    }
                }
            };
        }
        if record::token_exists(&mut tx, &operation.token.digest())
            .await
            .map_err(EnableRejection::from)?
        {
            return Err(EnableRejection::ResumeTokenCollision.into());
        }
        let generation = first_generation();
        let stored = DurableSessionRecord {
            session: operation.session,
            full_jid: request.full_jid.clone(),
            state: DurableSmState::Live,
            generation,
            binding: request.binding.clone(),
            principal: request.principal.clone(),
            claim: claim.map(RecordedClaim::from_fence),
            inbound_handled: 0,
            window: OutboundWindow::EMPTY,
            unacked_bytes: 0,
            max_resume: request.max_resume,
            detached_at: None,
            enable_operation: operation.id,
            last_operation: operation.id,
        };
        record::insert(&mut tx, &stored, &operation.token.digest(), request.now)
            .await
            .map_err(EnableRejection::from)?;
        operations::record_committed(
            &mut tx,
            operation.id,
            operation.session,
            OperationKind::Enable,
            generation,
            request.now,
        )
        .await
        .map_err(EnableRejection::from)?;
        match tx.commit().await {
            Ok(()) => Ok(EnabledCommit {
                fence: stored.fence(),
                token: operation.token.clone(),
                max_resume: request.max_resume,
            }),
            Err(cause) => Err(EnableError::Indeterminate(cause.into())),
        }
    }

    /// Settle an enable whose commit outcome is unknown.
    ///
    /// An abandonment marker is inserted under the operation id. If the
    /// original commit is durable the marker conflicts and the commit is
    /// reported; otherwise the marker commits and bars the original. An
    /// error means the outcome is still unknown; resolve again.
    pub async fn resolve_enable(
        &self,
        operation: &EnableOperation,
        binding: &BindingRef,
        now: DateTime<Utc>,
    ) -> Result<EnableResolution, DurableSmError> {
        let mut tx = self.begin_write().await?;
        let row = operations::abandon(
            &mut tx,
            operation.id,
            operation.session,
            OperationKind::Enable,
            first_generation(),
            now,
        )
        .await?;
        let stored = record::load(&mut tx, operation.session).await?;
        // The marker must be durable before its absence of a commit is
        // reported; a failed commit here leaves the outcome unknown.
        tx.commit().await?;
        if row.kind != OperationKind::Enable || row.session != operation.session {
            return Err(DurableSmError::CorruptRecord {
                field: "enable operation",
            });
        }
        Ok(match row.outcome {
            // The outcome row of a released terminal session is deleted, so
            // a marker can land for an enable that did commit. Its session
            // row still names the operation.
            OperationOutcome::Abandoned
                if stored
                    .as_ref()
                    .is_some_and(|stored| stored.enable_operation == operation.id) =>
            {
                EnableResolution::Superseded
            }
            OperationOutcome::Abandoned => EnableResolution::NotCommitted,
            OperationOutcome::Committed => match committed_witness(stored, operation, binding) {
                Some(witness) => EnableResolution::Committed(witness),
                None => EnableResolution::Superseded,
            },
        })
    }
}

fn first_generation() -> SessionGeneration {
    SessionGeneration::new(1).unwrap_or_else(|| unreachable!("one is a valid session generation"))
}

/// The witness for a committed enable, if the session is still the live
/// generation-one session this binding enabled.
fn committed_witness(
    stored: Option<DurableSessionRecord>,
    operation: &EnableOperation,
    binding: &BindingRef,
) -> Option<EnabledCommit> {
    let stored = stored?;
    (stored.enable_operation == operation.id
        && stored.state == DurableSmState::Live
        && stored.generation == first_generation()
        && stored.binding == *binding)
        .then(|| EnabledCommit {
            fence: stored.fence(),
            token: operation.token.clone(),
            max_resume: stored.max_resume,
        })
}
