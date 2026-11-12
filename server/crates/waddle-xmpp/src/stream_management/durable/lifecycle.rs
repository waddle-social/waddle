//! Lifecycle of a durable session as a total transition function.

use thiserror::Error;

/// Why a session ended. A terminal session never resumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalReason {
    /// The enable commit was resolved as not committed.
    EnableAbandoned,
    /// The resume window elapsed while detached.
    Expired,
    /// The resource was bound again, or its binding enabled a new session.
    Replaced,
    /// The stream ended deliberately.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DurableSmState {
    /// The enable commit has started and its outcome is not yet held. This
    /// state is never stored: the commit inserts the session as `Live`.
    Enabling,
    /// One binding owns the session and may expose frames.
    Live,
    /// No transport is attached; the session may be resumed until it expires.
    Detached,
    /// A resume committed a new generation whose endpoint is not yet
    /// published. Frames are retained, not exposed.
    TransferHydrating,
    Terminal(TerminalReason),
}

impl DurableSmState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal(_))
    }

    /// Whether a committed frame may be written to a transport.
    pub const fn exposes_frames(self) -> bool {
        matches!(self, Self::Live)
    }

    /// Whether the state has an owning binding whose fence authorizes writes.
    pub const fn has_owner(self) -> bool {
        matches!(self, Self::Live | Self::TransferHydrating)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DurableSmEvent {
    EnableCommitted,
    EnableAbandoned,
    /// The owner's transport ended, or a hydrating owner compensated.
    Detach,
    /// An authorized resume committed the next generation.
    ResumeClaimed,
    /// The resuming owner published its endpoint.
    ResumeCompleted,
    /// A successor proved the recorded owner no longer holds its claim.
    OwnerLost,
    Expire,
    Replace,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TransitionError {
    #[error("the session is terminal ({0:?})")]
    Terminal(TerminalReason),
    #[error("{event:?} does not apply to a session in {state:?}")]
    NotApplicable {
        state: DurableSmState,
        event: DurableSmEvent,
    },
}

/// The state after `event`, or why `event` does not apply. Total: every
/// state and event pair has an explicit answer, and an error means the
/// session is unchanged.
pub const fn transition(
    state: DurableSmState,
    event: DurableSmEvent,
) -> Result<DurableSmState, TransitionError> {
    use DurableSmEvent as Event;
    use DurableSmState as State;

    let rejected = Err(TransitionError::NotApplicable { state, event });
    match (state, event) {
        (State::Terminal(reason), _) => Err(TransitionError::Terminal(reason)),

        (State::Enabling, Event::EnableCommitted) => Ok(State::Live),
        (State::Enabling, Event::EnableAbandoned) => {
            Ok(State::Terminal(TerminalReason::EnableAbandoned))
        }
        (
            State::Enabling,
            Event::Detach
            | Event::ResumeClaimed
            | Event::ResumeCompleted
            | Event::OwnerLost
            | Event::Expire
            | Event::Replace
            | Event::Close,
        ) => rejected,

        (State::Live, Event::Detach | Event::OwnerLost) => Ok(State::Detached),
        (State::Live, Event::ResumeClaimed) => Ok(State::TransferHydrating),
        (State::Live, Event::Replace) => Ok(State::Terminal(TerminalReason::Replaced)),
        (State::Live, Event::Close) => Ok(State::Terminal(TerminalReason::Closed)),
        (
            State::Live,
            Event::EnableCommitted
            | Event::EnableAbandoned
            | Event::ResumeCompleted
            | Event::Expire,
        ) => rejected,

        (State::Detached, Event::ResumeClaimed) => Ok(State::TransferHydrating),
        (State::Detached, Event::Expire) => Ok(State::Terminal(TerminalReason::Expired)),
        (State::Detached, Event::Replace) => Ok(State::Terminal(TerminalReason::Replaced)),
        (State::Detached, Event::Close) => Ok(State::Terminal(TerminalReason::Closed)),
        (
            State::Detached,
            Event::EnableCommitted
            | Event::EnableAbandoned
            | Event::Detach
            | Event::ResumeCompleted
            | Event::OwnerLost,
        ) => rejected,

        (State::TransferHydrating, Event::ResumeCompleted) => Ok(State::Live),
        (State::TransferHydrating, Event::Detach | Event::OwnerLost) => Ok(State::Detached),
        (State::TransferHydrating, Event::ResumeClaimed) => Ok(State::TransferHydrating),
        (State::TransferHydrating, Event::Replace) => Ok(State::Terminal(TerminalReason::Replaced)),
        (State::TransferHydrating, Event::Close) => Ok(State::Terminal(TerminalReason::Closed)),
        (
            State::TransferHydrating,
            Event::EnableCommitted | Event::EnableAbandoned | Event::Expire,
        ) => rejected,
    }
}
