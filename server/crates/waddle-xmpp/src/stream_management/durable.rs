//! Pure domain model of a durable XEP-0198 session.
//!
//! Nothing here performs I/O or holds authority. A repository loads the
//! stored record, asks these functions what a request may do, and commits the
//! answer in the same transaction that locked the record. The model is fixed
//! by `docs/planning/2139-durable-sm-session-model.md`.

mod counter;
mod lifecycle;
mod resume;
#[cfg(test)]
mod tests;
mod token;

pub use counter::{
    AckKind, AppendError, BoundsError, Exposure, ExposureBounds, HandledDecision, HandledError,
    OutboundWindow, ReplayRange, SmOrdinal, WindowError, MAX_OUTSTANDING_LIMIT,
};
pub use lifecycle::{transition, DurableSmEvent, DurableSmState, TerminalReason, TransitionError};
pub use resume::{
    authorize_resume, check_owner_fence, DurableSessionView, FenceError, OwnerFence,
    PrincipalStanding, ResumeAttempt, ResumeDenial, ResumeGrant,
};
pub use token::{SmOperationId, SmResumeToken, SmResumeTokenDigest, SmResumeTokenError};
