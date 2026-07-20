package social.waddle.android.client

/** State transitions kept behind the coordinator's lifecycle mutex. */
internal interface WorkerRecoveryStateOwner {
    suspend fun claim(lifecycle: SessionLifecycleRef): WorkerRecoveryClaimDecision
    suspend fun currentLifecycle(): SessionLifecycleRef?
    suspend fun decideSiblingStop(
        claim: WorkerRecoveryClaim,
        workers: OwnerWorkers,
    ): RecoverySiblingStopDecision
    suspend fun ownsClaim(claim: WorkerRecoveryClaim, workers: OwnerWorkers): Boolean
    suspend fun awaitLeases(): Boolean
    suspend fun pendingLeaseCount(): Int
    suspend fun complete(claim: WorkerRecoveryClaim, workers: OwnerWorkers): WorkerRecoveryOutcome
    suspend fun clearClaim(claim: WorkerRecoveryClaim)
}

/** Executes one fenced-worker recovery after the coordinator admits its claim. */
internal class WorkerRecoveryOrchestrator(
    private val state: WorkerRecoveryStateOwner,
    private val recoverDurableState: suspend (OwnerWorkers, SessionLifecycleRef) -> OwnerFinalizationResult,
    private val timeoutMillis: Long,
) {
    suspend fun recover(lifecycle: SessionLifecycleRef): WorkerRecoveryOutcome {
        val granted = when (val decision = state.claim(lifecycle)) {
            WorkerRecoveryClaimDecision.NotFenced -> return WorkerRecoveryOutcome.NotFenced
            is WorkerRecoveryClaimDecision.OwnershipMismatch -> return WorkerRecoveryOutcome.OwnershipMismatch(
                decision.lifecycle,
                state.currentLifecycle(),
            )
            is WorkerRecoveryClaimDecision.RecoveryInProgress -> return WorkerRecoveryOutcome.RecoveryInProgress(decision.claim)
            is WorkerRecoveryClaimDecision.AwaitingExit -> return WorkerRecoveryOutcome.WorkerExitPending(
                decision.lifecycle,
                decision.ownership,
            )
            is WorkerRecoveryClaimDecision.Granted -> decision
        }
        val claim = granted.claim
        val workers = granted.workers
        try {
            when (val sibling = state.decideSiblingStop(claim, workers)) {
                is RecoverySiblingStopDecision.Stop -> when (sibling.sibling.kind) {
                    WorkerKind.DELIVERY_TERMINAL -> workers.terminal.requestStop()
                    WorkerKind.OUTBOUND_DRAIN -> workers.drain.requestStop()
                }
                RecoverySiblingStopDecision.AlreadyExited -> Unit
                RecoverySiblingStopDecision.RecoveryClaimLost,
                RecoverySiblingStopDecision.UnknownFailedWorker,
                is RecoverySiblingStopDecision.RecordedExitMismatch,
                -> return WorkerRecoveryOutcome.OwnershipMismatch(lifecycle, state.currentLifecycle())
            }
            val exits = listOf(
                workers.terminalOwnership to workers.terminal.awaitExit(timeoutMillis),
                workers.drainOwnership to workers.drain.awaitExit(timeoutMillis),
            )
            exits.firstOrNull { (_, outcome) -> outcome is WorkerAwaitOutcome.TimedOut }?.let {
                return WorkerRecoveryOutcome.WorkerExitPending(lifecycle, it.first)
            }
            val receiptCleanup = try {
                workers.terminal.recoverUnresolvedReceiptCleanup()
            } catch (failure: TerminalReceiptCleanupException) {
                return WorkerRecoveryOutcome.TerminalReceiptCleanupFailed(lifecycle, claim, failure.evidence)
            }
            if (receiptCleanup is TerminalReceiptRecoveryCleanupResult.Unresolved) {
                return WorkerRecoveryOutcome.TerminalReceiptCleanupFailed(lifecycle, claim, receiptCleanup.evidence)
            }
            if (!state.awaitLeases()) {
                return WorkerRecoveryOutcome.RetainedOperationsPending(lifecycle, state.pendingLeaseCount())
            }
            if (!state.ownsClaim(claim, workers)) {
                return WorkerRecoveryOutcome.OwnershipMismatch(lifecycle, state.currentLifecycle())
            }
            return when (val cleanup = recoverDurableState(workers, lifecycle)) {
                OwnerFinalizationResult.Finalized -> state.complete(claim, workers)
                is OwnerFinalizationResult.Pending -> WorkerRecoveryOutcome.DurableCleanupPending(
                    lifecycle, claim, cleanup.component, cleanup.count, cleanup.operation, cleanup.attempt,
                )
                is OwnerFinalizationResult.DurableCleanupFailed -> WorkerRecoveryOutcome.DurableCleanupFailed(
                    lifecycle, claim, cleanup.component, cleanup.count, cleanup.operation, cleanup.cause, cleanup.attempt,
                )
            }
        } finally {
            state.clearClaim(claim)
        }
    }
}
