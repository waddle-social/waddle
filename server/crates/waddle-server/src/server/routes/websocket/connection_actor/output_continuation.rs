//! Scheduler model for bounded, lazy output continuation.
//!
//! This test-support model establishes cursor, credit, and milestone rules. It
//! does not own a WebSocket pump and is not transport custody proof.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use waddle_xmpp::ownership::ConnectionRef;
use waddle_xmpp::Stanza;

const MAX_HANDLED_WINDOW: u64 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContinuationLimits {
    pub max_frames: usize,
    pub max_serialized_bytes: usize,
    pub max_frame_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationPhase {
    Prepared,
    Admitted,
    Attempted,
    Uncertain,
    Flushed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationTicket {
    connection: ConnectionRef,
    ordinal: u64,
}

impl ContinuationTicket {
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection
    }

    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContinuationError {
    #[error("continuation bounds must be nonzero and internally consistent")]
    InvalidBounds,
    #[error("continuation is stopped or has an uncertain output")]
    Stopped,
    #[error("continuation output credits are exhausted")]
    CreditsExhausted,
    #[error("serialized frame exceeds its reserved byte ceiling")]
    FrameTooLarge,
    #[error("ticket does not identify the current retained frame")]
    WrongTicket,
    #[error("output milestone transition is invalid")]
    InvalidTransition,
    #[error("handled-input window is full or sequence is outside its bound")]
    HandledWindowFull,
}

struct RetainedFrame {
    ticket: ContinuationTicket,
    stanza: Stanza,
    serialized_bytes: usize,
    phase: ContinuationPhase,
}

/// One synthetic producer cursor with at most one frame owned by its writer.
/// The caller supplies a synchronous effect closure; it runs only after hard
/// count and worst-case byte credits have been reserved.
pub struct OutputContinuation {
    connection: ConnectionRef,
    limits: ContinuationLimits,
    next_ordinal: u64,
    used_frames: usize,
    used_bytes: usize,
    current: Option<RetainedFrame>,
    last_flushed: Option<ContinuationTicket>,
    stopped: bool,
    handled_sequences: HashSet<u64>,
    checkpointed_handled: u64,
}

impl OutputContinuation {
    pub fn new(
        connection: ConnectionRef,
        limits: ContinuationLimits,
    ) -> Result<Self, ContinuationError> {
        if limits.max_frames == 0
            || limits.max_serialized_bytes == 0
            || limits.max_frame_bytes == 0
            || limits.max_frame_bytes > limits.max_serialized_bytes
        {
            return Err(ContinuationError::InvalidBounds);
        }
        Ok(Self {
            connection,
            limits,
            next_ordinal: 0,
            used_frames: 0,
            used_bytes: 0,
            current: None,
            last_flushed: None,
            stopped: false,
            handled_sequences: HashSet::new(),
            checkpointed_handled: 0,
        })
    }

    /// Lazily produce one typed stanza after reserving one frame and its
    /// maximum serialized size. A retained frame is returned by ticket on
    /// repeated calls, so cancellation of a waiter cannot re-run its effect.
    pub fn prepare_next(
        &mut self,
        effect: impl FnOnce(u64) -> Option<Stanza>,
    ) -> Result<Option<ContinuationTicket>, ContinuationError> {
        if self.stopped {
            return Err(ContinuationError::Stopped);
        }
        if let Some(current) = &self.current {
            return Ok(Some(current.ticket.clone()));
        }
        if self.next_ordinal == u64::MAX {
            self.stopped = true;
            return Err(ContinuationError::Stopped);
        }
        if self.used_frames >= self.limits.max_frames
            || self
                .used_bytes
                .checked_add(self.limits.max_frame_bytes)
                .is_none_or(|bytes| bytes > self.limits.max_serialized_bytes)
        {
            return Err(ContinuationError::CreditsExhausted);
        }

        // Reserve before invoking the effect. Shrink to actual encoded size
        // only after typed serialization succeeds within the reserved ceiling.
        self.used_frames += 1;
        self.used_bytes += self.limits.max_frame_bytes;
        let ordinal = self.next_ordinal;
        let Some(stanza) = effect(ordinal) else {
            self.release_reservation(self.limits.max_frame_bytes);
            self.stopped = true;
            return Ok(None);
        };
        let mut encoded = Vec::new();
        if stanza.to_element().write_to(&mut encoded).is_err()
            || encoded.len() > self.limits.max_frame_bytes
        {
            self.release_reservation(self.limits.max_frame_bytes);
            self.stopped = true;
            return Err(ContinuationError::FrameTooLarge);
        }
        let serialized_bytes = encoded.len();
        self.used_bytes -= self.limits.max_frame_bytes - serialized_bytes;
        let ticket = ContinuationTicket {
            connection: self.connection.clone(),
            ordinal,
        };
        self.current = Some(RetainedFrame {
            ticket: ticket.clone(),
            stanza,
            serialized_bytes,
            phase: ContinuationPhase::Prepared,
        });
        Ok(Some(ticket))
    }

    /// Return a typed snapshot without transferring frame ownership.
    pub fn stanza_snapshot(
        &self,
        ticket: &ContinuationTicket,
    ) -> Result<Stanza, ContinuationError> {
        let current = self.current(ticket)?;
        Ok(current.stanza.clone())
    }

    pub fn phase(
        &self,
        ticket: &ContinuationTicket,
    ) -> Result<ContinuationPhase, ContinuationError> {
        if let Some(frame) = self
            .current
            .as_ref()
            .filter(|frame| &frame.ticket == ticket)
        {
            return Ok(frame.phase);
        }
        if self.last_flushed.as_ref() == Some(ticket) {
            return Ok(ContinuationPhase::Flushed);
        }
        Err(ContinuationError::WrongTicket)
    }

    pub fn serialized_bytes(
        &self,
        ticket: &ContinuationTicket,
    ) -> Result<usize, ContinuationError> {
        Ok(self.current(ticket)?.serialized_bytes)
    }

    pub fn admit(&mut self, ticket: &ContinuationTicket) -> Result<(), ContinuationError> {
        if self.stopped {
            return Err(ContinuationError::Stopped);
        }
        self.transition(
            ticket,
            ContinuationPhase::Prepared,
            ContinuationPhase::Admitted,
        )
    }

    pub fn attempted(&mut self, ticket: &ContinuationTicket) -> Result<(), ContinuationError> {
        self.transition(
            ticket,
            ContinuationPhase::Admitted,
            ContinuationPhase::Attempted,
        )
    }

    /// Preserve the frame and credits, stop the suffix, and prohibit retry
    /// when a writer outcome is uncertain.
    pub fn mark_uncertain(&mut self, ticket: &ContinuationTicket) -> Result<(), ContinuationError> {
        let frame = self.current_mut(ticket)?;
        if !matches!(
            frame.phase,
            ContinuationPhase::Admitted | ContinuationPhase::Attempted
        ) {
            return Err(ContinuationError::InvalidTransition);
        }
        frame.phase = ContinuationPhase::Uncertain;
        self.stopped = true;
        Ok(())
    }

    /// Local flush settles the output credit and advances the lazy cursor. It
    /// does not establish peer receipt or XEP-0198 acknowledgment.
    pub fn mark_flushed(&mut self, ticket: &ContinuationTicket) -> Result<(), ContinuationError> {
        let frame = self.current_mut(ticket)?;
        if frame.phase != ContinuationPhase::Attempted {
            return Err(ContinuationError::InvalidTransition);
        }
        frame.phase = ContinuationPhase::Flushed;
        let bytes = frame.serialized_bytes;
        let flushed_ticket = frame.ticket.clone();
        self.release_reservation(bytes);
        self.next_ordinal = self.next_ordinal.saturating_add(1);
        self.current = None;
        self.last_flushed = Some(flushed_ticket);
        Ok(())
    }

    pub fn stop(&mut self) {
        self.stopped = true;
    }

    pub fn retained_credits(&self) -> (usize, usize) {
        (self.used_frames, self.used_bytes)
    }

    /// Record one handled input sequence inside the fixed 256-sequence window.
    pub fn mark_handled(&mut self, sequence: u64) -> Result<(), ContinuationError> {
        if sequence <= self.checkpointed_handled {
            return Ok(());
        }
        if sequence - self.checkpointed_handled > MAX_HANDLED_WINDOW
            || (self.handled_sequences.len() >= MAX_HANDLED_WINDOW as usize
                && !self.handled_sequences.contains(&sequence))
        {
            return Err(ContinuationError::HandledWindowFull);
        }
        self.handled_sequences.insert(sequence);
        Ok(())
    }

    pub fn checkpoint_contiguous_handled_prefix(&mut self) -> u64 {
        while self
            .handled_sequences
            .remove(&self.checkpointed_handled.saturating_add(1))
        {
            self.checkpointed_handled = self.checkpointed_handled.saturating_add(1);
        }
        self.checkpointed_handled
    }

    /// The acknowledged value is only the checkpointed contiguous prefix;
    /// XEP-0198 control is modeled as uncounted work at a cooperative yield.
    pub fn acknowledgement_at_safe_yield(&self) -> Option<u32> {
        Some(self.checkpointed_handled as u32)
    }

    /// Seed the model's absolute handled frontier for wrap-boundary tests.
    /// Production activation does not use this scheduler model.
    pub fn seed_handled_frontier_for_test(&mut self, frontier: u64) {
        self.handled_sequences.clear();
        self.checkpointed_handled = frontier;
    }

    fn current(&self, ticket: &ContinuationTicket) -> Result<&RetainedFrame, ContinuationError> {
        self.current
            .as_ref()
            .filter(|frame| &frame.ticket == ticket)
            .ok_or(ContinuationError::WrongTicket)
    }

    fn current_mut(
        &mut self,
        ticket: &ContinuationTicket,
    ) -> Result<&mut RetainedFrame, ContinuationError> {
        self.current
            .as_mut()
            .filter(|frame| &frame.ticket == ticket)
            .ok_or(ContinuationError::WrongTicket)
    }

    fn transition(
        &mut self,
        ticket: &ContinuationTicket,
        from: ContinuationPhase,
        to: ContinuationPhase,
    ) -> Result<(), ContinuationError> {
        let frame = self.current_mut(ticket)?;
        if frame.phase != from {
            return Err(ContinuationError::InvalidTransition);
        }
        frame.phase = to;
        Ok(())
    }

    fn release_reservation(&mut self, bytes: usize) {
        self.used_frames = self.used_frames.saturating_sub(1);
        self.used_bytes = self.used_bytes.saturating_sub(bytes);
    }
}

/// Exact-connection arbiter claim used by the scheduler model. Repeated
/// claims for the same `ConnectionRef` fail; successor generations are
/// distinct owners.
#[derive(Clone, Default)]
pub struct ContinuationArbiter {
    owners: Arc<Mutex<HashSet<ConnectionRef>>>,
}

impl ContinuationArbiter {
    pub fn claim(
        &self,
        connection: ConnectionRef,
    ) -> Result<ContinuationOwnerClaim, ContinuationError> {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if !owners.insert(connection.clone()) {
            return Err(ContinuationError::Stopped);
        }
        Ok(ContinuationOwnerClaim {
            connection,
            owners: Arc::clone(&self.owners),
        })
    }
}

pub struct ContinuationOwnerClaim {
    connection: ConnectionRef,
    owners: Arc<Mutex<HashSet<ConnectionRef>>>,
}

impl ContinuationOwnerClaim {
    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection
    }
}

impl Drop for ContinuationOwnerClaim {
    fn drop(&mut self) {
        self.owners
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .remove(&self.connection);
    }
}
