use jid::FullJid;

/// Why a stanza still needs its durable replay/custody responsibility.
/// Clustered owner-policy deferrals cannot be settled by this shutdown
/// owner; backend failures and quota rejection may succeed on a later pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionFailureReason {
    ClusteredIq,
    ClusteredPresence,
    ClusteredNonStorableMessage,
    ClusteredQuotaExceeded,
    CustodyQuotaExceeded,
    PendingStorage,
    ClaimLost,
    CustodyLookup,
    CustodyCompletion,
    NonDurableCustodyHandoff,
    QuotaBounceUnavailable,
}

impl PromotionFailureReason {
    pub(crate) fn requires_restart(self) -> bool {
        matches!(
            self,
            Self::ClusteredIq | Self::ClusteredPresence | Self::ClusteredNonStorableMessage
        )
    }
}

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
    /// Promotion did not commit a durable handoff. The caller MUST skip
    /// `confirm_drained` and retain the replay/custody payload for retry.
    StorageFailure(PromotionFailureReason),
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
    /// Number of stanzas whose durable promotion responsibility remains.
    /// Non-zero means the caller MUST NOT call `confirm_drained`.
    pub storage_failed: u32,
    /// Exact first failed sequence and its typed cause. Shutdown uses the
    /// cause to distinguish policy deferrals from retryable backend errors.
    pub first_failure: Option<(u32, PromotionFailureReason)>,
    /// XEP-0198 sequences of every stanza this promotion pass fully
    /// handled (every outcome except [`PromotedOutcome::StorageFailure`]).
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
            PromotedOutcome::StorageFailure(reason) => {
                self.storage_failed += 1;
                self.first_failure.get_or_insert((sequence, *reason));
                tracing::warn!(
                    sequence,
                    ?reason,
                    "Q6 promotion: durable handoff incomplete; retaining stanza for retry"
                );
            }
        }
        if !matches!(outcome, PromotedOutcome::StorageFailure(_)) {
            self.promoted_sequences.push(sequence);
        }
    }

    /// True when a backend failure or intentional policy deferral left
    /// durable responsibility outstanding. Keep the SM row for retry.
    pub fn has_storage_failure(&self) -> bool {
        self.storage_failed > 0
    }
}
