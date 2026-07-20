package social.waddle.android.client

import java.util.concurrent.ConcurrentHashMap

/**
 * Exception-only evidence for an exact worker ownership. Durable lifecycle
 * values deliberately remain Throwable-free. Evidence is retained while its
 * exact lifecycle remains fenced and disposed only by lifecycle transitions.
 */
internal object WorkerExitExceptionEvidence {
    private val failures = ConcurrentHashMap<WorkerOwnership, Throwable>()

    fun record(ownership: WorkerOwnership, failure: Throwable) {
        failures.putIfAbsent(ownership, failure)
    }

    fun discard(ownership: WorkerOwnership) {
        failures.remove(ownership)
    }

    fun lookup(outcome: WorkerRecoveryOutcome): Throwable? =
        ownership(outcome)?.let(failures::get)

    private fun ownership(outcome: WorkerRecoveryOutcome): WorkerOwnership? = when (outcome) {
        is WorkerRecoveryOutcome.WorkerFenced -> when (val cause = outcome.cause) {
            is LifecycleFenceCause.WorkerExited -> cause.fence.exit.ownership()
            is LifecycleFenceCause.AwaitingRequestedWorkerExit -> cause.ownership
        }
        is WorkerRecoveryOutcome.RecoveryInProgress -> outcome.claim.fence.exit.ownership()
        is WorkerRecoveryOutcome.DurableCleanupPending -> outcome.claim.fence.exit.ownership()
        is WorkerRecoveryOutcome.DurableCleanupFailed -> outcome.claim.fence.exit.ownership()
        is WorkerRecoveryOutcome.TerminalReceiptCleanupFailed -> outcome.claim.fence.exit.ownership()
        is WorkerRecoveryOutcome.WorkerExitPending -> outcome.ownership
        WorkerRecoveryOutcome.Recovered,
        WorkerRecoveryOutcome.NotFenced,
        is WorkerRecoveryOutcome.OwnershipMismatch,
        is WorkerRecoveryOutcome.RetainedOperationsPending,
        -> null
    }
}
