package social.waddle.android.client

import java.util.concurrent.ConcurrentHashMap

/** Exception-only evidence associated with one exact worker ownership. */
internal interface WorkerExitEvidence {
    fun record(ownership: WorkerOwnership, failure: Throwable)
    fun discard(ownership: WorkerOwnership)
    fun lookup(outcome: WorkerRecoveryOutcome): Throwable?
}

/**
 * Process production owner for exact worker evidence. Durable lifecycle values
 * deliberately remain Throwable-free; first-cause ownership is immutable.
 */
internal object WorkerExitExceptionEvidence : WorkerExitEvidence {
    private val failures = ConcurrentHashMap<WorkerOwnership, Throwable>()

    override fun record(ownership: WorkerOwnership, failure: Throwable) {
        failures.putIfAbsent(ownership, failure)
    }

    override fun discard(ownership: WorkerOwnership) {
        failures.remove(ownership)
    }

    override fun lookup(outcome: WorkerRecoveryOutcome): Throwable? =
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
        -> null
        is WorkerRecoveryOutcome.RetainedOperationsPending -> outcome.claim.fence.exit.ownership()
    }
}
