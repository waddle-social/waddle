//! Scheduler model for bounded, lazy output continuation.
//!
//! This test-support model establishes cursor, credit, and milestone rules. It
//! does not own a WebSocket pump and is not transport custody proof.

use std::collections::HashSet;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use waddle_xmpp::ownership::ConnectionRef;
use waddle_xmpp::Stanza;

const MAX_HANDLED_WINDOW: u64 = 256;

struct BoundedByteCounter {
    count: usize,
    limit: usize,
}

impl Write for BoundedByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .count
            .checked_add(bytes.len())
            .filter(|next| *next <= self.limit)
            .ok_or_else(|| io::Error::other("serialized frame exceeds its reservation"))?;
        self.count = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContinuationLimits {
    pub max_frames: usize,
    pub max_serialized_bytes: usize,
    pub max_frame_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationPhase {
    /// Credits are reserved and the producer's effect is still running.
    Producing,
    /// The effect returned a frame that could not fit its reservation.
    /// Its exact typed result and reservation remain held for reconciliation.
    Rejected,
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
    stanza: Option<Stanza>,
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

    /// Reserve one frame and its maximum encoded size before starting an
    /// asynchronous effect. A repeated observation returns the existing ticket.
    /// The exclusive owner must start the effect only once; a ticket is an
    /// identity for observation, not permission to start another effect.
    pub fn reserve_next(&mut self) -> Result<ContinuationTicket, ContinuationError> {
        if self.stopped {
            return Err(ContinuationError::Stopped);
        }
        if let Some(current) = &self.current {
            return Ok(current.ticket.clone());
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
        self.used_frames += 1;
        self.used_bytes += self.limits.max_frame_bytes;
        let ticket = ContinuationTicket {
            connection: self.connection.clone(),
            ordinal: self.next_ordinal,
        };
        self.current = Some(RetainedFrame {
            ticket: ticket.clone(),
            stanza: None,
            serialized_bytes: self.limits.max_frame_bytes,
            phase: ContinuationPhase::Producing,
        });
        Ok(ticket)
    }

    /// Finish a previously reserved effect even after stop has won. The
    /// returned typed result remains recoverable without starting it again.
    pub fn finish_preparation(
        &mut self,
        ticket: &ContinuationTicket,
        stanza: Option<Stanza>,
    ) -> Result<Option<ContinuationTicket>, ContinuationError> {
        if self.current(ticket)?.phase != ContinuationPhase::Producing {
            return Err(ContinuationError::InvalidTransition);
        }
        let Some(stanza) = stanza else {
            self.current = None;
            self.release_reservation(self.limits.max_frame_bytes);
            self.stopped = true;
            return Ok(None);
        };
        let mut counter = BoundedByteCounter {
            count: 0,
            limit: self.limits.max_frame_bytes,
        };
        if stanza.to_element().write_to(&mut counter).is_err() {
            let current = self.current_mut(ticket)?;
            current.stanza = Some(stanza);
            current.phase = ContinuationPhase::Rejected;
            self.stopped = true;
            return Err(ContinuationError::FrameTooLarge);
        }
        self.used_bytes -= self.limits.max_frame_bytes - counter.count;
        let current = self.current_mut(ticket)?;
        current.stanza = Some(stanza);
        current.serialized_bytes = counter.count;
        current.phase = ContinuationPhase::Prepared;
        Ok(Some(ticket.clone()))
    }

    /// Lazily produce one typed stanza after reserving one frame and its
    /// maximum serialized size. This synchronous wrapper is for model tests.
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
        let ticket = self.reserve_next()?;
        self.finish_preparation(&ticket, effect(ticket.ordinal()))
    }

    /// Return a typed snapshot without transferring frame ownership.
    pub fn stanza_snapshot(
        &self,
        ticket: &ContinuationTicket,
    ) -> Result<Stanza, ContinuationError> {
        let current = self.current(ticket)?;
        current
            .stanza
            .clone()
            .ok_or(ContinuationError::InvalidTransition)
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
