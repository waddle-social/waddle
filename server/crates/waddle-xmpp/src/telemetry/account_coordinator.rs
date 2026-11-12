//! Bounded-cardinality samples for the account coordinator.
use super::attributes::{AccountMutationKind, AccountOperationOutcome, MailboxRejection};

/// One operation an account coordinator handled, by mutation and outcome.
pub fn record_operation(mutation: AccountMutationKind, outcome: AccountOperationOutcome) {
    add_operation(1, mutation, outcome);
}

fn add_operation(count: u64, mutation: AccountMutationKind, outcome: AccountOperationOutcome) {
    crate::counter_add!(
        "xmpp.account_coordinator.operations",
        "{operation}",
        "Account coordinator operations by mutation and bounded outcome.",
        count,
        mutation,
        outcome,
    );
}

/// Publish a zero for the series that mean something went wrong: an
/// operation the coordinator or its store could not decide, and a message
/// a mailbox refused. Committed, replayed and rejected operations are
/// progress and stay unregistered.
pub(super) fn register_counters() {
    for mutation in AccountMutationKind::ALL {
        add_operation(0, mutation, AccountOperationOutcome::Failed);
    }
    for reason in MailboxRejection::ALL {
        add_mailbox_rejected(0, reason);
    }
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
    add_mailbox_rejected(1, reason);
}

fn add_mailbox_rejected(count: u64, reason: MailboxRejection) {
    crate::counter_add!(
        "xmpp.account_coordinator.mailbox.rejected",
        "{message}",
        "Messages an account coordinator mailbox refused, by reason.",
        count,
        reason,
    );
}

const SLOT_BUCKETS: [f64; 8] = [0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0];
