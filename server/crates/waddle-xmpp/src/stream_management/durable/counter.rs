//! Nonwrapping outbound ordinals and wrap-safe handled-count validation.

use thiserror::Error;

/// Largest number of retained frames a session may hold. Keeping the window
/// below half the 32-bit counter space makes every wire `h` inside it
/// unambiguous.
pub const MAX_OUTSTANDING_LIMIT: u32 = (1 << 31) - 1;

const HALF_RANGE: u32 = 1 << 31;

/// Absolute position of one countable outbound stanza in a session.
///
/// Ordinal zero is "nothing sent"; the first stanza is ordinal one. The value
/// never wraps; [`Self::wire`] is the XEP-0198 counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SmOrdinal(u64);

impl SmOrdinal {
    pub const ZERO: Self = Self(0);

    /// Ordinals are stored in signed 64-bit columns.
    pub const fn new(value: u64) -> Option<Self> {
        if value <= i64::MAX as u64 {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    /// The XEP-0198 counter value: the ordinal modulo `2^32`.
    pub const fn wire(self) -> u32 {
        self.0 as u32
    }

    pub fn checked_add(self, count: u32) -> Option<Self> {
        self.0.checked_add(u64::from(count)).and_then(Self::new)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BoundsError {
    #[error("the retained frame bound must be between 1 and {MAX_OUTSTANDING_LIMIT}")]
    Outstanding,
    #[error("the retained byte bound must be nonzero")]
    Bytes,
}

/// How much unacknowledged output one session may retain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExposureBounds {
    max_outstanding: u32,
    max_outstanding_bytes: u64,
}

impl ExposureBounds {
    pub const fn new(
        max_outstanding: u32,
        max_outstanding_bytes: u64,
    ) -> Result<Self, BoundsError> {
        if max_outstanding == 0 || max_outstanding > MAX_OUTSTANDING_LIMIT {
            return Err(BoundsError::Outstanding);
        }
        if max_outstanding_bytes == 0 {
            return Err(BoundsError::Bytes);
        }
        Ok(Self {
            max_outstanding,
            max_outstanding_bytes,
        })
    }

    pub const fn max_outstanding(self) -> u32 {
        self.max_outstanding
    }

    pub const fn max_outstanding_bytes(self) -> u64 {
        self.max_outstanding_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WindowError {
    #[error("outbound ordinals are not ordered acked <= ceiling <= appended")]
    Unordered,
    #[error("the retained window spans half the counter space or more")]
    TooWide,
}

/// Which acknowledgement a handled count arrived in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckKind {
    /// `<a h=…/>` on the owning transport.
    Live,
    /// The `h` of `<resume/>`.
    Resume,
}

/// What a valid handled count does to the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandledDecision {
    /// The peer repeated the acknowledged floor.
    Unchanged,
    /// The floor advances; `released` frames leave the retained window.
    Advanced { through: SmOrdinal, released: u32 },
    /// A live acknowledgement older than the floor. It changes nothing.
    IgnoredStale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HandledError {
    /// The peer acknowledged a stanza that was never exposed to it.
    #[error("handled count {h} is above the exposure ceiling {ceiling}")]
    TooHigh { h: u32, ceiling: u32 },
    /// Exactly half the counter space ahead: neither forward nor backward.
    #[error("handled count {h} is ambiguous against the acknowledged floor")]
    Ambiguous { h: u32 },
    /// A resume named a count below the floor; the frames between are gone.
    #[error("handled count {h} is behind the acknowledged floor {acked}")]
    Behind { h: u32, acked: u32 },
}

/// Whether an appended frame may be written to a transport once committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// The owner writes the frame after the commit; the ceiling covers it.
    Exposed,
    /// No transport is attached; the frame waits for replay.
    Retained,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AppendError {
    #[error("the session already retains the maximum number of frames")]
    OutstandingBound,
    #[error("the session already retains the maximum number of payload bytes")]
    ByteBound,
    #[error("the session ordinal space is exhausted")]
    OrdinalExhausted,
}

/// The retained frames to resend, in ordinal order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayRange {
    after: SmOrdinal,
    through: SmOrdinal,
}

impl ReplayRange {
    /// The acknowledged floor; replay starts at the next ordinal.
    pub const fn after(self) -> SmOrdinal {
        self.after
    }

    pub const fn through(self) -> SmOrdinal {
        self.through
    }

    pub const fn len(self) -> u64 {
        self.through.0 - self.after.0
    }

    pub const fn is_empty(self) -> bool {
        self.through.0 == self.after.0
    }

    pub const fn contains(self, ordinal: SmOrdinal) -> bool {
        ordinal.0 > self.after.0 && ordinal.0 <= self.through.0
    }

    /// Map a stored 32-bit sequence back to its ordinal, if it lies in range.
    pub fn ordinal_for_wire(self, sequence: u32) -> Option<SmOrdinal> {
        let delta = sequence.wrapping_sub(self.after.wire());
        let ordinal = self.after.checked_add(delta)?;
        (delta != 0 && self.contains(ordinal)).then_some(ordinal)
    }

    pub fn ordinals(self) -> impl DoubleEndedIterator<Item = SmOrdinal> {
        (self.after.0 + 1..=self.through.0).map(SmOrdinal)
    }
}

/// The three outbound ordinals of a session: `acked <= ceiling <= appended`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundWindow {
    acked: SmOrdinal,
    ceiling: SmOrdinal,
    appended: SmOrdinal,
}

impl OutboundWindow {
    pub const EMPTY: Self = Self {
        acked: SmOrdinal::ZERO,
        ceiling: SmOrdinal::ZERO,
        appended: SmOrdinal::ZERO,
    };

    pub fn new(
        acked: SmOrdinal,
        ceiling: SmOrdinal,
        appended: SmOrdinal,
    ) -> Result<Self, WindowError> {
        if acked > ceiling || ceiling > appended {
            return Err(WindowError::Unordered);
        }
        if appended.0 - acked.0 > u64::from(MAX_OUTSTANDING_LIMIT) {
            return Err(WindowError::TooWide);
        }
        Ok(Self {
            acked,
            ceiling,
            appended,
        })
    }

    pub const fn acked(self) -> SmOrdinal {
        self.acked
    }

    /// Last ordinal the owner may have written to a transport.
    pub const fn exposure_ceiling(self) -> SmOrdinal {
        self.ceiling
    }

    pub const fn appended(self) -> SmOrdinal {
        self.appended
    }

    pub const fn outstanding(self) -> u64 {
        self.appended.0 - self.acked.0
    }

    /// Classify a wire handled count without changing the window.
    pub fn classify_handled(self, h: u32, kind: AckKind) -> Result<HandledDecision, HandledError> {
        let delta = h.wrapping_sub(self.acked.wire());
        if delta == 0 {
            return Ok(HandledDecision::Unchanged);
        }
        // The constructor keeps the window below half the counter space, so
        // this difference fits in 31 bits.
        let exposed = (self.ceiling.0 - self.acked.0) as u32;
        if delta <= exposed {
            return Ok(HandledDecision::Advanced {
                through: SmOrdinal(self.acked.0 + u64::from(delta)),
                released: delta,
            });
        }
        if delta < HALF_RANGE {
            return Err(HandledError::TooHigh {
                h,
                ceiling: self.ceiling.wire(),
            });
        }
        if delta == HALF_RANGE {
            return Err(HandledError::Ambiguous { h });
        }
        match kind {
            AckKind::Live => Ok(HandledDecision::IgnoredStale),
            AckKind::Resume => Err(HandledError::Behind {
                h,
                acked: self.acked.wire(),
            }),
        }
    }

    /// Apply a wire handled count. An error leaves the window as it was.
    pub fn acknowledge(
        self,
        h: u32,
        kind: AckKind,
    ) -> Result<(Self, HandledDecision), HandledError> {
        let decision = self.classify_handled(h, kind)?;
        let window = match decision {
            HandledDecision::Advanced { through, .. } => Self {
                acked: through,
                ..self
            },
            HandledDecision::Unchanged | HandledDecision::IgnoredStale => self,
        };
        Ok((window, decision))
    }

    /// Allocate the next ordinal for a frame of `frame_bytes`, given the
    /// payload bytes the session already retains.
    pub fn append(
        self,
        exposure: Exposure,
        bounds: ExposureBounds,
        retained_bytes: u64,
        frame_bytes: u64,
    ) -> Result<(Self, SmOrdinal), AppendError> {
        if self.outstanding() >= u64::from(bounds.max_outstanding) {
            return Err(AppendError::OutstandingBound);
        }
        let bytes = retained_bytes
            .checked_add(frame_bytes)
            .ok_or(AppendError::ByteBound)?;
        if bytes > bounds.max_outstanding_bytes {
            return Err(AppendError::ByteBound);
        }
        let ordinal = self
            .appended
            .checked_add(1)
            .ok_or(AppendError::OrdinalExhausted)?;
        let ceiling = match exposure {
            // A live owner writes in ordinal order, so exposing this frame
            // exposes every retained frame below it.
            Exposure::Exposed => ordinal,
            Exposure::Retained => self.ceiling,
        };
        Ok((
            Self {
                acked: self.acked,
                ceiling,
                appended: ordinal,
            },
            ordinal,
        ))
    }

    /// Raise the ceiling over every retained frame, as a replay does.
    pub const fn expose_all(self) -> Self {
        Self {
            acked: self.acked,
            ceiling: self.appended,
            appended: self.appended,
        }
    }

    /// Every retained frame, including frames never exposed.
    pub const fn replay(self) -> ReplayRange {
        ReplayRange {
            after: self.acked,
            through: self.appended,
        }
    }

    /// The retained frames above the exposure ceiling.
    pub const fn unexposed(self) -> ReplayRange {
        ReplayRange {
            after: self.ceiling,
            through: self.appended,
        }
    }
}
