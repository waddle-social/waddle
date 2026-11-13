//! XEP-0198 inline resume and Bind 2 enablement inside a SASL2 exchange.
//!
//! XEP-0388 lets an `<authenticate/>` carry a `<resume/>` and an XEP-0386
//! `<bind/>`; the `<success/>` then carries their results. A resumed session
//! keeps its resource, so the bind is processed only when no session
//! resumed. This module turns the typed inline requests into durable
//! authority operations and their witnesses into the typed inline results.
//! It decides nothing about advertisement: a server offers these inline
//! features only where its SASL2 handshake is enabled.

use std::time::Duration;

use chrono::{DateTime, Utc};
use jid::FullJid;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ownership::BindingRef;
use waddle_xmpp::stream_management::durable::{PrincipalStanding, ResumeDenial};
use waddle_xmpp::stream_management::persistence::SmClaimFence;
use waddle_xmpp_core::xep0388_inline::{
    Bind2Bound, Bind2Request, Bind2StreamManagementResult, Sasl2InlineRequests, Sasl2InlineResults,
    SmInlineEnable, SmInlineEnabled, SmInlineFailed, SmInlineResume, SmInlineResumeResult,
    SmInlineResumed, SmStreamId,
};
use xmpp_parsers::stanza_error::DefinedCondition;

use super::enable::{EnableError, EnableOperation, EnableRejection, EnableRequest, EnabledCommit};
use super::error::DurableSmError;
use super::resume::{
    FreshBindPermit, ResumeCandidate, ResumeError, ResumeOperation, ResumeRejection,
    ResumeRejectionReason, ResumeRequest, ResumedCommit,
};
use super::store::DurableSmStore;
use crate::auth::session::PrincipalResolution;

/// What an authenticated SASL2 exchange asks of stream management, in the
/// order XEP-0386 fixes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InlinePlan<'a> {
    /// Neither inline feature was requested.
    None,
    /// Resume first. `fallback` is the bind to process if the resume is
    /// known to have failed.
    Resume {
        resume: &'a SmInlineResume,
        fallback: Option<&'a Bind2Request>,
    },
    Bind(&'a Bind2Request),
}

/// Order the inline requests of one `<authenticate/>`.
pub fn plan_inline(requests: &Sasl2InlineRequests) -> InlinePlan<'_> {
    match (&requests.resume, &requests.bind) {
        (Some(resume), fallback) => InlinePlan::Resume {
            resume,
            fallback: fallback.as_ref(),
        },
        (None, Some(bind)) => InlinePlan::Bind(bind),
        (None, None) => InlinePlan::None,
    }
}

/// How the stored principal reference of a resume candidate stands, from
/// the session authority's resolution of it. A storage failure is not a
/// standing: the caller fails the resume with
/// [`InlineResumeFailed::standing_unavailable`].
pub fn principal_standing(resolution: &PrincipalResolution) -> Option<PrincipalStanding> {
    match resolution {
        PrincipalResolution::Resolved(_) => Some(PrincipalStanding::Current),
        PrincipalResolution::Missing => Some(PrincipalStanding::Missing),
        PrincipalResolution::Mismatched => Some(PrincipalStanding::Diverged),
        PrincipalResolution::StorageError(_) => None,
    }
}

/// An inline resume that found its session and awaits the standing of the
/// stored principal.
#[derive(Debug)]
pub struct InlineResumeAttempt {
    operation: ResumeOperation,
    candidate: ResumeCandidate,
    h: u32,
}

impl InlineResumeAttempt {
    /// The operation identity, kept until the resume's outcome is known.
    pub fn operation(&self) -> &ResumeOperation {
        &self.operation
    }

    /// The stored principal reference to resolve against the session
    /// authority.
    pub fn stored_principal(&self) -> &AuthenticatedPrincipalRef {
        &self.candidate.principal
    }

    /// The resource the session is bound to.
    pub fn full_jid(&self) -> &FullJid {
        &self.candidate.full_jid
    }

    /// The session the token names. It keys the session's claim and its
    /// ingress checkpoint.
    pub fn session(&self) -> waddle_xmpp::ownership::SmSessionKey {
        self.candidate.session
    }
}

/// A resume known not to have happened: the `<failed/>` to report and the
/// permit that lets the exchange fall back to Bind 2.
#[derive(Debug)]
pub struct InlineResumeFailed {
    failed: SmInlineFailed,
    permit: FreshBindPermit,
}

impl InlineResumeFailed {
    /// The failure a known resume rejection reports.
    pub fn from_rejection(rejection: &ResumeRejection) -> Self {
        Self {
            failed: SmInlineFailed {
                h: rejection.handled_floor(),
                condition: failed_condition(rejection.reason()),
            },
            permit: rejection.fresh_bind_permit(),
        }
    }

    /// The session authority could not be read to resolve the stored
    /// principal, so the resume transaction was not run.
    pub fn standing_unavailable() -> Self {
        Self::from_rejection(&ResumeRejection::new(
            ResumeRejectionReason::PrincipalStandingUnavailable,
        ))
    }

    /// This node does not hold, and could not take, the session's claim,
    /// so the resume transaction was not run.
    pub fn claim_unavailable() -> Self {
        Self::from_rejection(&ResumeRejection::new(
            ResumeRejectionReason::ClaimFenceMissing,
        ))
    }

    /// The session ended before its claim could be held, so the resume
    /// transaction was not run.
    pub fn session_gone() -> Self {
        Self::from_rejection(&ResumeRejection::new(ResumeRejectionReason::UnknownSession))
    }

    /// The resuming account is not the one the session is bound to, so its
    /// claim was not moved and the resume transaction was not run.
    pub fn account_mismatch() -> Self {
        Self::from_rejection(&ResumeRejection::new(ResumeRejectionReason::Denied(
            ResumeDenial::PrincipalMismatch,
        )))
    }

    pub fn failed(&self) -> &SmInlineFailed {
        &self.failed
    }

    pub fn fresh_bind_permit(&self) -> &FreshBindPermit {
        &self.permit
    }
}

/// The outcome of the inline resume step.
#[derive(Debug)]
pub enum InlineResumeOutcome {
    Resumed(Box<ResumedCommit>),
    Failed(InlineResumeFailed),
    /// The commit outcome is unknown. The exchange can report neither a
    /// resumed session nor a failure, and must not bind: settle the
    /// operation with [`DurableSmStore::resolve_resume`] first.
    Unknown(DurableSmError),
}

/// What the resuming connection presents once the stored principal's
/// standing is known.
#[derive(Debug, Clone, Copy)]
pub struct InlineResumeRequest<'a> {
    pub binding: &'a BindingRef,
    pub presented: &'a AuthenticatedPrincipalRef,
    pub standing: PrincipalStanding,
    pub claim: Option<&'a SmClaimFence>,
    pub now: DateTime<Utc>,
}

/// What Bind 2 does after the resume step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bind2Decision<'a> {
    /// No bind was requested, or none remains to process.
    NotRequested,
    /// A session resumed and keeps its resource; the bind is skipped and no
    /// `<bound/>` is reported.
    SkippedForResume,
    /// Bind a resource, enabling stream management if `enable` is present.
    Bind {
        request: &'a Bind2Request,
        enable: Option<SmInlineEnable>,
    },
}

/// The resume outcome is unknown, so whether to bind is unknown too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the inline resume outcome is unknown; Bind 2 cannot be decided")]
pub struct InlineOutcomeUnknown;

/// Decide the bind from the plan and the resume step's outcome. A fallback
/// bind after a resume request proceeds only on a known failure.
pub fn decide_bind2<'a>(
    plan: InlinePlan<'a>,
    resume: Option<&InlineResumeOutcome>,
) -> Result<Bind2Decision<'a>, InlineOutcomeUnknown> {
    let bind = |request: &'a Bind2Request| Bind2Decision::Bind {
        request,
        enable: request.stream_management,
    };
    match (plan, resume) {
        (InlinePlan::None, _) => Ok(Bind2Decision::NotRequested),
        (InlinePlan::Bind(request), _) => Ok(bind(request)),
        (InlinePlan::Resume { .. }, Some(InlineResumeOutcome::Resumed(_))) => {
            Ok(Bind2Decision::SkippedForResume)
        }
        (InlinePlan::Resume { fallback, .. }, Some(InlineResumeOutcome::Failed(_))) => {
            Ok(fallback.map_or(Bind2Decision::NotRequested, bind))
        }
        // A resume that was planned and not yet run has no outcome either.
        (InlinePlan::Resume { .. }, Some(InlineResumeOutcome::Unknown(_)) | None) => {
            Err(InlineOutcomeUnknown)
        }
    }
}

/// The outcome of enabling stream management for a Bind 2 resource.
#[derive(Debug)]
pub enum Bind2EnableOutcome {
    Enabled(EnabledCommit),
    /// The resource is bound without stream management.
    Failed(SmInlineFailed),
    /// The commit outcome is unknown; settle the operation with
    /// [`DurableSmStore::resolve_enable`] before reporting `<bound/>`.
    Unknown(DurableSmError),
}

/// The stream-management session an exchange ended with.
#[derive(Debug)]
pub enum InlineSession<'a> {
    /// No inline feature produced a result.
    None,
    Resumed {
        commit: &'a ResumedCommit,
        /// The server's handled count of client stanzas.
        h: u32,
    },
    /// The resume failed and no bind followed.
    ResumeFailed(&'a InlineResumeFailed),
    Bound {
        /// The failed resume this bind fell back from, if any.
        resume_failed: Option<&'a InlineResumeFailed>,
        /// The enable requested with the bind and its outcome, if any.
        stream_management: Option<Bind2StreamManagement<'a>>,
    },
}

/// The known outcome of a Bind 2 `<enable/>`.
#[derive(Debug)]
pub enum Bind2StreamManagement<'a> {
    Enabled {
        commit: &'a EnabledCommit,
        /// Whether the client asked for a resumable session.
        resumable: bool,
    },
    Failed(&'a SmInlineFailed),
}

/// Build the inline results of `<success/>`. `Resumed` carries no `<bound/>`
/// and `Bound` no `<resumed/>`, as XEP-0386 requires.
pub fn inline_results(session: InlineSession<'_>) -> Result<Sasl2InlineResults, DurableSmError> {
    let mut results = Sasl2InlineResults::default();
    match session {
        InlineSession::None => {}
        InlineSession::Resumed { commit, h } => {
            results.resume = Some(SmInlineResumeResult::Resumed(SmInlineResumed {
                previd: stream_id(commit.resumed_reply(h).previd)?,
                h,
            }));
        }
        InlineSession::ResumeFailed(failed) => {
            results.resume = Some(SmInlineResumeResult::Failed(failed.failed.clone()));
        }
        InlineSession::Bound {
            resume_failed,
            stream_management,
        } => {
            results.resume =
                resume_failed.map(|failed| SmInlineResumeResult::Failed(failed.failed.clone()));
            let stream_management = match stream_management {
                None => None,
                Some(Bind2StreamManagement::Failed(failed)) => {
                    Some(Bind2StreamManagementResult::Failed(failed.clone()))
                }
                Some(Bind2StreamManagement::Enabled { commit, resumable }) => Some(
                    Bind2StreamManagementResult::Enabled(inline_enabled(commit, resumable)?),
                ),
            };
            results.bound = Some(Bind2Bound {
                stream_management,
                other_results: Vec::new(),
            });
        }
    }
    Ok(results)
}

/// The resume window granted for a Bind 2 enable: the client's preference
/// capped by the server's maximum.
pub fn granted_resume_window(enable: SmInlineEnable, server_max: Duration) -> Duration {
    enable.max.map_or(server_max, |requested| {
        server_max.min(Duration::from_secs(u64::from(requested)))
    })
}

impl DurableSmStore {
    /// Look up the session an inline `<resume/>` names. A `previd` that is
    /// not a token this server issues, or that names no session or one that
    /// has ended, is a known failure.
    pub async fn begin_inline_resume(
        &self,
        resume: &SmInlineResume,
    ) -> Result<InlineResumeAttempt, InlineResumeFailed> {
        let unknown = || {
            InlineResumeFailed::from_rejection(&ResumeRejection::new(
                ResumeRejectionReason::UnknownSession,
            ))
        };
        let operation =
            ResumeOperation::for_previd(resume.previd.as_str()).map_err(|_| unknown())?;
        match self.resume_candidate(&operation).await {
            Ok(Some(candidate)) => match candidate.terminal {
                Some(reason) => Err(InlineResumeFailed::from_rejection(&ResumeRejection::new(
                    ResumeDenial::Terminal(reason),
                ))),
                None => Ok(InlineResumeAttempt {
                    operation,
                    candidate,
                    h: resume.h,
                }),
            },
            Ok(None) => Err(unknown()),
            Err(error) => Err(InlineResumeFailed::from_rejection(&ResumeRejection::new(
                ResumeRejectionReason::Store(error),
            ))),
        }
    }

    /// Run the resume transaction for an inline attempt.
    pub async fn finish_inline_resume(
        &self,
        attempt: &InlineResumeAttempt,
        request: InlineResumeRequest<'_>,
    ) -> InlineResumeOutcome {
        let resumed = self
            .resume(
                &attempt.operation,
                ResumeRequest {
                    binding: request.binding,
                    presented: request.presented,
                    resolved: &attempt.candidate.principal,
                    standing: request.standing,
                    h: attempt.h,
                    claim: request.claim,
                    now: request.now,
                },
            )
            .await;
        match resumed {
            Ok(commit) => InlineResumeOutcome::Resumed(Box::new(commit)),
            Err(ResumeError::Rejected(rejection)) => {
                InlineResumeOutcome::Failed(InlineResumeFailed::from_rejection(&rejection))
            }
            Err(ResumeError::Indeterminate(cause)) => InlineResumeOutcome::Unknown(cause),
        }
    }

    /// Enable stream management for a resource Bind 2 just bound. The
    /// request's `max_resume` is the granted window; see
    /// [`granted_resume_window`].
    pub async fn inline_enable(
        &self,
        operation: &EnableOperation,
        request: EnableRequest<'_>,
    ) -> Bind2EnableOutcome {
        match self.enable(operation, request).await {
            Ok(commit) => Bind2EnableOutcome::Enabled(commit),
            Err(EnableError::Rejected(rejection)) => Bind2EnableOutcome::Failed(SmInlineFailed {
                h: None,
                condition: Some(match rejection {
                    EnableRejection::ResourceMismatch => DefinedCondition::NotAuthorized,
                    EnableRejection::ClaimFenceMissing => DefinedCondition::ResourceConstraint,
                    EnableRejection::ResumeTokenCollision
                    | EnableRejection::Abandoned
                    | EnableRejection::OperationReused
                    | EnableRejection::Store(_) => DefinedCondition::InternalServerError,
                }),
            }),
            Err(EnableError::Indeterminate(cause)) => Bind2EnableOutcome::Unknown(cause),
        }
    }
}

/// The `<enabled/>` reported inside `<bound/>`. A client that did not ask
/// for resumption is told the session is not resumable and is given no id,
/// so the resume token never leaves the server.
fn inline_enabled(
    commit: &EnabledCommit,
    resumable: bool,
) -> Result<SmInlineEnabled, DurableSmError> {
    let reply = commit.enabled_reply();
    if !resumable {
        return Ok(SmInlineEnabled {
            id: None,
            resume: false,
            max: None,
            location: None,
        });
    }
    Ok(SmInlineEnabled {
        id: Some(stream_id(reply.id)?),
        resume: true,
        max: reply.max,
        location: None,
    })
}

fn stream_id(token: String) -> Result<SmStreamId, DurableSmError> {
    SmStreamId::new(token).ok_or(DurableSmError::CorruptRecord {
        field: "resume token",
    })
}

/// The stanza error condition of `<failed/>` for a known resume rejection.
/// A handled count the session cannot honour carries no condition, only the
/// server's own count.
fn failed_condition(reason: &ResumeRejectionReason) -> Option<DefinedCondition> {
    match reason {
        ResumeRejectionReason::UnknownSession
        | ResumeRejectionReason::Abandoned
        | ResumeRejectionReason::Superseded
        | ResumeRejectionReason::Denied(ResumeDenial::Terminal(_) | ResumeDenial::Expired) => {
            Some(DefinedCondition::ItemNotFound)
        }
        ResumeRejectionReason::PrincipalChanged
        | ResumeRejectionReason::Denied(
            ResumeDenial::PrincipalUnavailable
            | ResumeDenial::PrincipalDiverged
            | ResumeDenial::PrincipalMismatch,
        ) => Some(DefinedCondition::NotAuthorized),
        ResumeRejectionReason::Denied(ResumeDenial::Handled(_)) => None,
        ResumeRejectionReason::ClaimFenceMissing
        | ResumeRejectionReason::Denied(ResumeDenial::GenerationExhausted) => {
            Some(DefinedCondition::ResourceConstraint)
        }
        ResumeRejectionReason::OperationReused
        | ResumeRejectionReason::PrincipalStandingUnavailable
        | ResumeRejectionReason::Store(_) => Some(DefinedCondition::InternalServerError),
    }
}
