//! Owner fence comparison and the resume authorization decision.

use std::time::Duration;

use chrono::{DateTime, Utc};
use thiserror::Error;

use super::counter::{AckKind, HandledDecision, HandledError, OutboundWindow, ReplayRange};
use super::lifecycle::{transition, DurableSmEvent, DurableSmState, TerminalReason};
use crate::auth::AuthenticatedPrincipalRef;
use crate::ownership::{BindingRef, SessionGeneration, SmSessionKey};

/// The exact owner of one session generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerFence {
    pub session: SmSessionKey,
    pub generation: SessionGeneration,
    pub binding: BindingRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FenceError {
    #[error("the fence names another session")]
    OtherSession,
    /// A later resume owns the session; this owner's effects are refused.
    #[error("generation {presented:?} was superseded by {stored:?}")]
    StaleGeneration {
        stored: SessionGeneration,
        presented: SessionGeneration,
    },
    #[error("generation {presented:?} was never committed (stored {stored:?})")]
    UnknownGeneration {
        stored: SessionGeneration,
        presented: SessionGeneration,
    },
    #[error("the generation is owned by another binding attempt")]
    BindingMismatch,
}

/// Compare a presented owner fence with the stored one. Only exact equality
/// of session, generation and binding attempt authorizes an owner effect.
pub fn check_owner_fence(stored: &OwnerFence, presented: &OwnerFence) -> Result<(), FenceError> {
    if stored.session != presented.session {
        return Err(FenceError::OtherSession);
    }
    if presented.generation < stored.generation {
        return Err(FenceError::StaleGeneration {
            stored: stored.generation,
            presented: presented.generation,
        });
    }
    if presented.generation > stored.generation {
        return Err(FenceError::UnknownGeneration {
            stored: stored.generation,
            presented: presented.generation,
        });
    }
    if stored.binding != presented.binding {
        return Err(FenceError::BindingMismatch);
    }
    Ok(())
}

/// How the stored principal reference resolved against the session authority
/// when the resume was attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalStanding {
    /// The exact auth-context version and principal epoch are still current.
    Current,
    /// No such authentication context exists any more.
    Missing,
    /// The context exists but its version, epoch or JID changed.
    Diverged,
}

/// The stored facts a resume is decided against.
#[derive(Debug, Clone, Copy)]
pub struct DurableSessionView<'a> {
    pub state: DurableSmState,
    pub generation: SessionGeneration,
    pub principal: &'a AuthenticatedPrincipalRef,
    pub window: OutboundWindow,
    /// When the session first lost its transport, if it has.
    pub detached_at: Option<DateTime<Utc>>,
    pub max_resume: Duration,
}

/// What the resuming connection presents.
#[derive(Debug, Clone, Copy)]
pub struct ResumeAttempt<'a> {
    pub presented: &'a AuthenticatedPrincipalRef,
    pub stored_standing: PrincipalStanding,
    pub h: u32,
    pub now: DateTime<Utc>,
}

/// An authorized resume: what the commit must store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeGrant {
    pub generation: SessionGeneration,
    pub state: DurableSmState,
    /// The window after the resume's handled count settled.
    pub window: OutboundWindow,
    pub handled: HandledDecision,
    pub replay: ReplayRange,
}

/// A known reason the session cannot be resumed by this attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ResumeDenial {
    #[error("the session is terminal ({0:?})")]
    Terminal(TerminalReason),
    #[error("the resume window elapsed")]
    Expired,
    #[error("the session's principal reference no longer resolves")]
    PrincipalUnavailable,
    #[error("the session's principal reference diverged from its authority")]
    PrincipalDiverged,
    #[error("the resuming connection authenticated as another principal")]
    PrincipalMismatch,
    #[error(transparent)]
    Handled(#[from] HandledError),
    #[error("the session generation space is exhausted")]
    GenerationExhausted,
}

/// Decide whether `attempt` may resume the session in `view`.
///
/// The checks run in a fixed order so a denial never depends on a later
/// input: lifecycle, expiry, stored principal standing, presented principal,
/// then the handled count against the exposure ceiling.
pub fn authorize_resume(
    view: &DurableSessionView<'_>,
    attempt: &ResumeAttempt<'_>,
) -> Result<ResumeGrant, ResumeDenial> {
    let state = match transition(view.state, DurableSmEvent::ResumeClaimed) {
        Ok(state) => state,
        Err(super::TransitionError::Terminal(reason)) => {
            return Err(ResumeDenial::Terminal(reason))
        }
        // `Enabling` is never stored, so a stored session is never in it.
        Err(super::TransitionError::NotApplicable { .. }) => {
            return Err(ResumeDenial::Terminal(TerminalReason::EnableAbandoned))
        }
    };
    if let Some(detached_at) = view.detached_at {
        let elapsed = attempt
            .now
            .signed_duration_since(detached_at)
            .to_std()
            .unwrap_or(Duration::ZERO);
        if elapsed >= view.max_resume {
            return Err(ResumeDenial::Expired);
        }
    }
    match attempt.stored_standing {
        PrincipalStanding::Current => {}
        PrincipalStanding::Missing => return Err(ResumeDenial::PrincipalUnavailable),
        PrincipalStanding::Diverged => return Err(ResumeDenial::PrincipalDiverged),
    }
    if attempt.presented.bare_jid() != view.principal.bare_jid()
        || attempt.presented.auth_epoch() != view.principal.auth_epoch()
    {
        return Err(ResumeDenial::PrincipalMismatch);
    }
    let (window, handled) = view.window.acknowledge(attempt.h, AckKind::Resume)?;
    let generation = view
        .generation
        .checked_next()
        .ok_or(ResumeDenial::GenerationExhausted)?;
    Ok(ResumeGrant {
        generation,
        state,
        window,
        handled,
        replay: window.replay(),
    })
}
