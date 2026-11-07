//! An unselected, in-process frame-occurrence model.
//!
//! An occurrence is separate from a source obligation: one obligation may
//! produce several frames, including byte-identical frames. This model retains
//! the original serialized payload and its identity while a persistence result
//! is uncertain. It does not itself provide durable custody or a send permit.
//! The fixture adapter can reconcile a retained known identity, including under
//! a sealed successor claim. It has no autonomous occurrence inventory after
//! that identity is lost; that remains a blocker for any serving selection.

use chrono::{DateTime, Utc};
use jid::FullJid;
use uuid::Uuid;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ownership::{BindingRef, ClaimEpoch, NodeIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::SmReplayPayload;
use waddle_xmpp::stream_management::SmFrameProvenance;

/// The exact session captured before retaining this frame. These values are
/// identity claims; a persistence adapter must verify them against authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameCapture {
    pub gate_id: Uuid,
    pub token: Uuid,
    pub resource: FullJid,
    pub binding: BindingRef,
    pub session: SmSessionId,
    pub generation: u64,
    pub principal: AuthenticatedPrincipalRef,
    pub owner: NodeIdentity,
    pub claim: ClaimEpoch,
}

/// Absolute, nonwrapping position. `h` is only its wrapping representation;
/// this position does not grant permission to expose or acknowledge a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePosition(u64);

impl FramePosition {
    pub fn ordinal(self) -> u64 {
        self.0
    }

    pub fn h(self) -> u32 {
        self.0 as u32
    }
}

/// Knowledge about one retained occurrence, never delivery authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKnowledge {
    Retained,
    Unknown,
    Committed(FramePosition),
}

/// Evidence supplied by a future persistence adapter after exact positive
/// readback and a known commit. It cannot be constructed by a caller.
#[derive(Debug)]
pub struct FrameCommitEvidence {
    capture: FrameCapture,
    obligation: Uuid,
    occurrence: Uuid,
    payload: SmReplayPayload,
    received_at: DateTime<Utc>,
    provenance: SmFrameProvenance,
    position: FramePosition,
}

impl FrameCommitEvidence {
    // The fixture adapter calls this only after an exact readback and commit.
    #[cfg(all(feature = "test-support", feature = "clustering"))]
    pub(crate) fn verified(frame: &RetainedFrameOccurrence, ordinal: i64) -> Option<Self> {
        let ordinal = u64::try_from(ordinal).ok().filter(|ordinal| *ordinal > 0)?;
        Some(Self {
            capture: frame.capture.clone(),
            obligation: frame.obligation,
            occurrence: frame.occurrence,
            payload: frame.payload.clone(),
            received_at: frame.received_at,
            provenance: frame.provenance.clone(),
            position: FramePosition(ordinal),
        })
    }
}

/// Immutable caller-owned identity and exact serialized frame. Constructing it
/// is not admission. In particular, unknown provenance is retained and must
/// be refused by any adapter that requires verified ingress evidence.
#[derive(Debug)]
pub struct RetainedFrameOccurrence {
    capture: FrameCapture,
    obligation: Uuid,
    occurrence: Uuid,
    payload: SmReplayPayload,
    received_at: DateTime<Utc>,
    provenance: SmFrameProvenance,
    knowledge: FrameKnowledge,
}

impl RetainedFrameOccurrence {
    pub fn retain(
        capture: FrameCapture,
        obligation: Uuid,
        occurrence: Uuid,
        payload: SmReplayPayload,
        received_at: DateTime<Utc>,
        provenance: SmFrameProvenance,
    ) -> Self {
        Self {
            capture,
            obligation,
            occurrence,
            payload,
            received_at,
            provenance,
            knowledge: FrameKnowledge::Retained,
        }
    }

    pub fn capture(&self) -> &FrameCapture {
        &self.capture
    }

    pub fn obligation_id(&self) -> Uuid {
        self.obligation
    }

    pub fn occurrence_id(&self) -> Uuid {
        self.occurrence
    }

    pub fn payload(&self) -> &SmReplayPayload {
        &self.payload
    }

    pub fn received_at(&self) -> DateTime<Utc> {
        self.received_at
    }

    pub fn provenance(&self) -> &SmFrameProvenance {
        &self.provenance
    }

    pub fn knowledge(&self) -> FrameKnowledge {
        self.knowledge
    }

    // Mark before the first await. Dropping a borrowed persistence future then
    // leaves the caller's exact occurrence intact and conservatively unknown.
    pub fn mark_unknown(&mut self) {
        if !matches!(self.knowledge, FrameKnowledge::Committed(_)) {
            self.knowledge = FrameKnowledge::Unknown;
        }
    }

    pub fn confirm(&mut self, evidence: &FrameCommitEvidence) -> bool {
        if self.capture != evidence.capture
            || self.obligation != evidence.obligation
            || self.occurrence != evidence.occurrence
            || self.payload.wire() != evidence.payload.wire()
            || self.received_at != evidence.received_at
            || self.provenance != evidence.provenance
            || matches!(self.knowledge, FrameKnowledge::Committed(old) if old != evidence.position)
        {
            return false;
        }
        self.knowledge = FrameKnowledge::Committed(evidence.position);
        true
    }
}
