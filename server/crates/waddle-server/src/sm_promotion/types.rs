use jid::FullJid;

/// Outcome of promoting a single unacked stanza per the Q6 chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotedOutcome {
    /// Live-redelivered to an alternate non-negative-priority
    /// resource of the recipient.
    Redelivered { to: FullJid },
    /// Inserted into `pending_delivery` for offline replay.
    Queued,
    /// Bounced `<service-unavailable/>` to the sender per
    /// XEP-0160 §3 step 3 (`pending_delivery` quota exceeded).
    Bounced,
    /// Dropped — classifier produced no actionable sink (e.g.
    /// `<no-store/>`, chat-states-only, error-type to fully-offline
    /// recipient per RFC 6121 §8.5.2.1.4).
    Dropped,
    /// Skipped — stanza could not be parsed back to a typed value
    /// (corrupt unacked queue entry). Logged for operator visibility.
    Unparseable,
    /// Storage backend failure — `pending_delivery.insert` returned
    /// `Err`. The caller MUST treat this as a transient promotion
    /// failure and SKIP `confirm_drained` for the owning session so
    /// the durable SM row survives for restart-time retry. (Copilot
    /// review on PR #346: previously collapsed into `Dropped` so the
    /// caller would call `confirm_drained` and permanently lose the
    /// stanza when offline storage was temporarily failing.)
    StorageFailure,
}

/// Aggregate outcome of promoting every unacked stanza in a session.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PromotionSummary {
    pub redelivered: u32,
    pub queued: u32,
    pub bounced: u32,
    pub dropped: u32,
    pub unparseable: u32,
    /// Number of stanzas that failed to insert into pending storage.
    /// Non-zero means the session's promotion was lossy: the caller
    /// MUST NOT call `confirm_drained` for this session, so its
    /// durable SM row survives for restart-time retry.
    pub storage_failed: u32,
}

impl PromotionSummary {
    pub(super) fn record(&mut self, outcome: &PromotedOutcome) {
        match outcome {
            PromotedOutcome::Redelivered { .. } => self.redelivered += 1,
            PromotedOutcome::Queued => self.queued += 1,
            PromotedOutcome::Bounced => self.bounced += 1,
            PromotedOutcome::Dropped => self.dropped += 1,
            PromotedOutcome::Unparseable => self.unparseable += 1,
            PromotedOutcome::StorageFailure => self.storage_failed += 1,
        }
    }

    /// True when at least one stanza in this session failed to
    /// promote due to a transient storage backend error. Callers
    /// MUST inspect this before invoking `confirm_drained`: a
    /// `true` result means the durable SM row must be kept so a
    /// later janitor pass / restart can retry promotion.
    pub fn has_storage_failure(&self) -> bool {
        self.storage_failed > 0
    }
}
