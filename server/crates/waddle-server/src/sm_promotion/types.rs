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
    /// Valid stanza that intentionally bypassed Q6 sinks because it
    /// has no XEP-0160 offline-delivery semantics.
    NotPromotable,
    /// Skipped — stanza could not be parsed back to a typed value
    /// (corrupt unacked queue entry). Logged for operator visibility.
    Unparseable,
    /// Dropped because a recently applied XEP-0424/0425 tombstone
    /// matches this stanza (round-2 review R2): the retraction raced
    /// the drain, so the promotion-time re-check scrubs the in-flight
    /// copy instead of delivering retracted content on next login.
    Scrubbed,
    /// Storage backend failure — `pending_delivery.insert` returned
    /// `Err`. The caller MUST treat this as a transient promotion
    /// failure and SKIP `confirm_drained` for the owning session so
    /// the durable SM row survives for restart-time retry. (Copilot
    /// review on PR #346: previously collapsed into `Dropped` so the
    /// caller would call `confirm_drained` and permanently lose the
    /// stanza when offline storage was temporarily failing.)
    StorageFailure,
    /// A durable cross-store invariant could not be established. The caller
    /// MUST retain this row for operator reconciliation and MUST NOT age it
    /// through the transient storage-failure dead-letter budget.
    Quarantined,
    /// The exact captured SM fence is no longer authoritative.
    AuthorityLost,
}

/// Aggregate outcome of promoting every unacked stanza in a session.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PromotionSummary {
    pub redelivered: u32,
    pub queued: u32,
    pub bounced: u32,
    pub dropped: u32,
    pub not_promotable: u32,
    pub unparseable: u32,
    /// Number of stanzas dropped by the promotion-time recent-
    /// tombstone re-check (round-2 review R2). Counted separately
    /// from `dropped` so retraction-race scrubs stay visible in the
    /// per-session summary logs.
    pub scrubbed: u32,
    /// Number of stanzas that failed to insert into pending storage.
    /// Non-zero means the session's promotion was lossy: the caller
    /// MUST NOT call `confirm_drained` for this session, so its
    /// durable SM row survives for restart-time retry.
    pub storage_failed: u32,
    /// Number of rows retained because their durable replay invariants could
    /// not be established. These rows require reconciliation and are never
    /// eligible for automatic dead-lettering.
    pub quarantined: u32,
    /// Whether promotion stopped because the captured generation/fence ceased
    /// to be authoritative.
    pub authority_lost: bool,
    /// XEP-0198 sequences of every stanza this promotion pass fully
    /// handled (every outcome except [`PromotedOutcome::StorageFailure`],
    /// [`PromotedOutcome::Quarantined`], and [`PromotedOutcome::AuthorityLost`]).
    /// On a partial failure the retry path durably deletes exactly
    /// these `sm_unacked` rows and drops them from the reinserted
    /// session, so the next tick retries only the failed stanzas
    /// (round-2 review R4) instead of re-promoting the whole queue.
    pub promoted_sequences: Vec<u32>,
}

impl PromotionSummary {
    pub(super) fn record(&mut self, sequence: u32, outcome: &PromotedOutcome) {
        match outcome {
            PromotedOutcome::Redelivered { .. } => self.redelivered += 1,
            PromotedOutcome::Queued => self.queued += 1,
            PromotedOutcome::Bounced => self.bounced += 1,
            PromotedOutcome::Dropped => self.dropped += 1,
            PromotedOutcome::NotPromotable => self.not_promotable += 1,
            PromotedOutcome::Unparseable => self.unparseable += 1,
            PromotedOutcome::Scrubbed => self.scrubbed += 1,
            PromotedOutcome::StorageFailure => self.storage_failed += 1,
            PromotedOutcome::Quarantined => self.quarantined += 1,
            PromotedOutcome::AuthorityLost => self.authority_lost = true,
        }
        if !matches!(
            outcome,
            PromotedOutcome::StorageFailure
                | PromotedOutcome::Quarantined
                | PromotedOutcome::AuthorityLost
        ) {
            self.promoted_sequences.push(sequence);
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

    /// True when at least one row needs durable invariant reconciliation.
    /// Quarantine takes precedence over transient retry/dead-letter policy.
    pub fn has_quarantined(&self) -> bool {
        self.quarantined > 0
    }

    /// Whether a transient promotion failure has exhausted its automatic
    /// retry budget. Quarantined rows are deliberately ineligible regardless
    /// of the current attempt count.
    pub(crate) fn should_dead_letter(&self, attempts: u32, max_attempts: u32) -> bool {
        !self.has_quarantined() && self.has_storage_failure() && attempts >= max_attempts
    }

    pub fn has_authority_lost(&self) -> bool {
        self.authority_lost
    }
}
