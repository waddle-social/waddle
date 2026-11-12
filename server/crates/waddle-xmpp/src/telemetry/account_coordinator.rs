//! Bounded-cardinality samples for the account coordinator.
use super::attributes::{AccountMutationKind, AccountOperationOutcome, MailboxRejection};

/// One operation an account coordinator handled, by mutation and outcome.
pub fn record_operation(mutation: AccountMutationKind, outcome: AccountOperationOutcome) {
    crate::counter_add!(
        "xmpp.account_coordinator.operations",
        "{operation}",
        "Account coordinator operations by mutation and bounded outcome.",
        1,
        mutation,
        outcome,
    );
}

/// The number of resource slots one account holds, sampled after a
/// committed mutation. The sample describes the account the mutation was
/// for; it is not a process-wide total.
pub fn record_slots_in_use(slots: usize) {
    crate::histogram_record!(
        "xmpp.account_coordinator.slots",
        "{slot}",
        "Resource slots held by an account, sampled after each committed mutation.",
        buckets: SLOT_BUCKETS,
        slots as f64,
    );
}

/// A message an account coordinator mailbox refused.
pub fn record_mailbox_rejected(reason: MailboxRejection) {
    crate::counter_add!(
        "xmpp.account_coordinator.mailbox.rejected",
        "{message}",
        "Messages an account coordinator mailbox refused, by reason.",
        1,
        reason,
    );
}

const SLOT_BUCKETS: [f64; 8] = [0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0];
