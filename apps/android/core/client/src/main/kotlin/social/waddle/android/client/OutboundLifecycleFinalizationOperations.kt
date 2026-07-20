package social.waddle.android.client

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import java.io.IOException
import java.util.logging.Level
import java.util.logging.Logger
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryAttemptTransition
import social.waddle.android.client.prefs.DeliveryTerminalKind
import social.waddle.android.client.session.ActiveSession
import social.waddle.android.client.session.ResumePersistence

internal sealed interface OwnerFinalizationResult {
    data object Finalized : OwnerFinalizationResult

    data class Pending(
        val component: LifecyclePendingComponent,
        val count: Int,
        val operation: DurableCleanupOperation = DurableCleanupOperation.JOURNAL_INSPECTION,
        val attempt: DeliveryAttemptRef? = null,
    ) : OwnerFinalizationResult

    data class DurableCleanupFailed(
        val component: LifecyclePendingComponent,
        val count: Int,
        val operation: DurableCleanupOperation,
        val cause: DurableCleanupFailureCause,
        val attempt: DeliveryAttemptRef?,
    ) : OwnerFinalizationResult
}

internal sealed interface DurableCleanupBoundary<out T> {
    data class Completed<T>(val value: T) : DurableCleanupBoundary<T>

    data class Failed(
        val operation: DurableCleanupOperation,
        val cause: DurableCleanupFailureCause,
        val attempt: DeliveryAttemptRef?,
    ) : DurableCleanupBoundary<Nothing>
}

/** Only an explicitly transient durable-store failure becomes retryable. */
internal suspend fun <T> durableCleanupBoundary(
    operation: DurableCleanupOperation,
    attempt: DeliveryAttemptRef? = null,
    block: suspend () -> T,
): DurableCleanupBoundary<T> = try {
    DurableCleanupBoundary.Completed(block())
} catch (cancelled: kotlinx.coroutines.CancellationException) {
    throw cancelled
} catch (failure: IOException) {
    DURABLE_CLEANUP_LOG.log(
        Level.WARNING,
        "durable recovery cleanup failed operation=$operation attempt=$attempt",
        failure,
    )
    DurableCleanupBoundary.Failed(operation, DurableCleanupFailureCause.IO_FAILURE, attempt)
}

/** The only recoverable durability operations needed by worker recovery. */
internal interface DurableRecoveryCleanup {
    suspend fun inspectActiveAttempt(lifecycle: SessionLifecycleRef): DeliveryAttemptRef?
    suspend fun fenceAttempt(attempt: DeliveryAttemptRef)
    suspend fun retireAttempt(attempt: DeliveryAttemptRef)
    suspend fun endActiveSessionAttempt(attempt: DeliveryAttemptRef)
}

internal class ProductionDurableRecoveryCleanup(
    private val journal: OutboundQueue,
    private val resume: ResumePersistence,
    private val activeSession: ActiveSession,
) : DurableRecoveryCleanup {
    override suspend fun inspectActiveAttempt(lifecycle: SessionLifecycleRef): DeliveryAttemptRef? =
        journal.activeAttempt(lifecycle.ownerBareJid)

    override suspend fun fenceAttempt(attempt: DeliveryAttemptRef) {
        journal.fenceAttempt(attempt)
    }

    override suspend fun retireAttempt(attempt: DeliveryAttemptRef) {
        resume.retireAttempt(attempt)
    }

    override suspend fun endActiveSessionAttempt(attempt: DeliveryAttemptRef) {
        activeSession.endAttempt(attempt)
    }
}

/**
 * Stateless, bounded cleanup for durable attempt projections and workers.
 * It never reads or mutates coordinator lifecycle state.
 */
internal class OutboundLifecycleFinalizationOperations(
    private val activeSession: ActiveSession,
    private val journal: OutboundQueue,
    private val resume: ResumePersistence,
    private val drainWorker: OutboundDrainWorker,
    private val terminalWorker: DeliveryTerminalWorker,
    private val transitionTimeoutMillis: Long,
    private val durableRecoveryCleanup: DurableRecoveryCleanup =
        ProductionDurableRecoveryCleanup(journal, resume, activeSession),
) {
    suspend fun startWorkers(
        scope: CoroutineScope,
        workers: OwnerWorkers,
        onReady: suspend (WorkerOwnership) -> Unit,
        onExit: suspend (WorkerExit) -> Unit,
    ): OwnerWorkers {
        var terminal: DeliveryTerminalWorker.Run? = null
        var installed = false
        try {
            terminal = terminalWorker.start(
                scope,
                workers.terminalOwnership,
                onReady,
                onExit,
            )
            val drain = drainWorker.start(
                scope,
                workers.drainOwnership,
                onReady,
                onExit,
            )
            workers.install(terminal, drain)
            installed = true
            return workers
        } finally {
            if (!installed) {
                terminal?.requestStop()
                terminal?.awaitExit(transitionTimeoutMillis)
            }
        }
    }

    suspend fun awaitStartupTerminalDrain(workers: OwnerWorkers, ownerBareJid: String) {
        if (workers.lifecycle.ownerBareJid == ownerBareJid) workers.terminal.awaitStartupDrain()
    }

    suspend fun submitTerminal(
        workers: OwnerWorkers,
        ownerBareJid: String,
        clientStanzaId: String,
        attempt: DeliveryAttemptRef,
        kind: DeliveryTerminalKind,
    ): TerminalCommandOutcome =
        if (workers.lifecycle.ownerBareJid == ownerBareJid) {
            workers.terminal.submitAndAwait(clientStanzaId, attempt, kind)
        } else {
            TerminalCommandOutcome.WorkerUnavailable
        }

    fun signalDrain(
        workers: OwnerWorkers?,
        active: OutboundLifecycleState.Active?,
        attempt: DeliveryAttemptRef,
    ): DrainSignalOutcome = when {
        active == null -> DrainSignalOutcome.WorkerUnavailable
        active.attempt != attempt -> DrainSignalOutcome.Mismatch
        else ->
            workers?.takeIf { it.lifecycle == active.lifecycle }
                ?.drain
                ?.signal(active.handle, attempt)
                ?: DrainSignalOutcome.WorkerUnavailable
    }

    suspend fun disconnect(claim: DisconnectClaim): Boolean {
        if (claim is DisconnectClaim.Execute) {
            try {
                val client = claim.record.client
                val disconnected = if (client == null) {
                    true
                } else {
                    withTimeoutOrNull(transitionTimeoutMillis) {
                        client.disconnect()
                        true
                    } == true
                }
                claim.result.complete(disconnected)
            } catch (failure: Throwable) {
                claim.result.completeExceptionally(failure)
                throw failure
            }
        }
        return withTimeoutOrNull(transitionTimeoutMillis) {
            claim.result.await()
        } == true
    }

    fun prepareAttemptClose(
        record: AttemptRecord,
        producerQuiesced: Boolean,
    ): AttemptCloseOutcome.FencedWithPending? {
        if (!producerQuiesced) {
            return AttemptCloseOutcome.FencedWithPending(
                LifecyclePendingComponent.NATIVE_PRODUCER,
                1,
            )
        }
        record.producerStopped.complete(Unit)
        return null
    }

    suspend fun finalizeAttemptClose(
        workers: OwnerWorkers,
        record: AttemptRecord,
    ): AttemptCloseOutcome =
        withContext(NonCancellable) {
            if (finalizeAttempt(workers, record)) {
                AttemptCloseOutcome.Closed
            } else {
                AttemptCloseOutcome.FencedWithPending(
                    LifecyclePendingComponent.ATTEMPT_FINALIZATION,
                    1,
                )
            }
        }

    suspend fun quiesceTransport(
        record: AttemptRecord,
        claim: DisconnectClaim,
    ): OwnerFinalizationResult {
        if (!disconnect(claim)) {
            return OwnerFinalizationResult.Pending(
                LifecyclePendingComponent.NATIVE_DISCONNECT,
                1,
            )
        }
        val producerStopped = withTimeoutOrNull(transitionTimeoutMillis) {
            record.producerStopped.await()
            true
        } == true
        return if (producerStopped) {
            if (transportClosed(record)) {
                OwnerFinalizationResult.Finalized
            } else {
                OwnerFinalizationResult.Pending(
                    LifecyclePendingComponent.NATIVE_CLIENT_CLOSE,
                    1,
                )
            }
        } else {
            OwnerFinalizationResult.Pending(
                LifecyclePendingComponent.NATIVE_PRODUCER,
                1,
            )
        }
    }

    suspend fun transportClosed(record: AttemptRecord): Boolean {
        if (!record.requiresClientCloseProof) return true
        return withTimeoutOrNull(transitionTimeoutMillis) {
            record.clientClosed.await()
        } == true
    }

    suspend fun recoverDurableState(
        workers: OwnerWorkers,
        lifecycle: SessionLifecycleRef,
    ): OwnerFinalizationResult {
        val terminalPending = workers.terminal.pendingCommandCount()
        if (terminalPending > 0) {
            return OwnerFinalizationResult.Pending(LifecyclePendingComponent.TERMINAL_DRAIN, terminalPending)
        }
        val durableAttempt = when (
            val result = durableCleanupBoundary(DurableCleanupOperation.JOURNAL_INSPECTION) {
                durableRecoveryCleanup.inspectActiveAttempt(lifecycle)
            }
        ) {
            is DurableCleanupBoundary.Completed -> result.value
            is DurableCleanupBoundary.Failed -> return result.failed()
        }
        val attempts = linkedSetOf<DeliveryAttemptRef>().apply {
            durableAttempt?.let(::add)
            activeSession.attemptRef
                ?.takeIf { it.ownerBareJid == lifecycle.ownerBareJid }
                ?.let(::add)
        }
        attempts.forEach { attempt ->
            when (
                val result = durableCleanupBoundary(
                    operation = DurableCleanupOperation.JOURNAL_FENCE,
                    attempt = attempt,
                ) { durableRecoveryCleanup.fenceAttempt(attempt) }
            ) {
                is DurableCleanupBoundary.Completed -> Unit
                is DurableCleanupBoundary.Failed -> return result.failed()
            }
            when (
                val result = durableCleanupBoundary(
                    operation = DurableCleanupOperation.RESUME_RETIREMENT,
                    attempt = attempt,
                ) { durableRecoveryCleanup.retireAttempt(attempt) }
            ) {
                is DurableCleanupBoundary.Completed -> Unit
                is DurableCleanupBoundary.Failed -> return result.failed()
            }
            when (
                val result = durableCleanupBoundary(
                    operation = DurableCleanupOperation.ACTIVE_SESSION_CLEANUP,
                    attempt = attempt,
                ) { durableRecoveryCleanup.endActiveSessionAttempt(attempt) }
            ) {
                is DurableCleanupBoundary.Completed -> Unit
                is DurableCleanupBoundary.Failed -> return result.failed()
            }
        }
        return OwnerFinalizationResult.Finalized
    }

    private fun DurableCleanupBoundary.Failed.failed(): OwnerFinalizationResult.DurableCleanupFailed =
        OwnerFinalizationResult.DurableCleanupFailed(
            component = LifecyclePendingComponent.ATTEMPT_FINALIZATION,
            count = 1,
            operation = operation,
            cause = cause,
            attempt = attempt,
        )

    private suspend fun finalizeAttempt(workers: OwnerWorkers, record: AttemptRecord): Boolean =
        withTimeoutOrNull(transitionTimeoutMillis) {
            journal.fenceAttempt(record.attempt)
            resume.retireAttempt(record.attempt)
            workers.drain.unbind(record.handle, record.attempt)
            activeSession.endAttempt(record.attempt)
            true
        } == true

    suspend fun compensateActivation(
        workers: OwnerWorkers,
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        knownAttempt: DeliveryAttemptRef?,
    ): Boolean = withTimeoutOrNull(transitionTimeoutMillis) {
        val attempt = knownAttempt ?: journal.activeAttempt(lifecycle.ownerBareJid)
        if (attempt != null) {
            journal.fenceAttempt(attempt)
            resume.retireAttempt(attempt)
            workers.drain.unbind(handle, attempt)
            activeSession.endAttempt(attempt)
        }
        true
    } == true

    suspend fun compensateHandoff(
        workers: OwnerWorkers,
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        transition: DeliveryAttemptTransition,
    ): Boolean = withTimeoutOrNull(transitionTimeoutMillis) {
        journal.fenceAttempt(transition.old)
        journal.fenceAttempt(transition.fresh)
        resume.retireAttempt(transition.old)
        resume.retireAttempt(transition.fresh)
        workers.drain.unbind(handle, transition.old)
        workers.drain.unbind(handle, transition.fresh)
        activeSession.endAttempt(transition.old)
        activeSession.endAttempt(transition.fresh)
        true
    } == true

    suspend fun finalizeOwner(
        workers: OwnerWorkers,
        lifecycle: SessionLifecycleRef,
        record: AttemptRecord?,
    ): OwnerFinalizationResult {
        val attempts = collectAttempts(workers, lifecycle, record)
            ?: return OwnerFinalizationResult.Pending(
                LifecyclePendingComponent.ATTEMPT_FINALIZATION,
                1,
            )
        workers.terminal.requestStop()
        val terminalResult = workers.terminal.awaitExit(transitionTimeoutMillis)
        val pendingCommands = workers.terminal.pendingCommandCount()
        if (terminalResult is WorkerAwaitOutcome.TimedOut && pendingCommands > 0) {
            return OwnerFinalizationResult.Pending(
                LifecyclePendingComponent.TERMINAL_DRAIN,
                maxOf(pendingCommands, journal.terminalIntentCount(lifecycle.ownerBareJid)),
            )
        }
        workers.drain.requestStop()
        val drainResult = workers.drain.awaitExit(transitionTimeoutMillis)
        if (
            drainResult !is WorkerAwaitOutcome.Exited ||
                drainResult.exit.reason !is WorkerExitReason.RequestedStop
        ) {
            return OwnerFinalizationResult.Pending(
                LifecyclePendingComponent.OUTBOUND_DRAIN,
                1,
            )
        }
        val attemptsFinalized = finalizeAttempts(attempts)
        if (!attemptsFinalized) {
            return OwnerFinalizationResult.Pending(
                LifecyclePendingComponent.ATTEMPT_FINALIZATION,
                attempts.size,
            )
        }
        if (terminalResult is WorkerAwaitOutcome.TimedOut) {
            return OwnerFinalizationResult.Pending(
                LifecyclePendingComponent.TERMINAL_DRAIN,
                maxOf(pendingCommands, journal.terminalIntentCount(lifecycle.ownerBareJid)),
            )
        }
        val terminalPending = maxOf(
            pendingCommands,
            journal.terminalIntentCount(lifecycle.ownerBareJid),
            if (terminalResult is WorkerAwaitOutcome.Exited &&
                terminalResult.exit.reason is WorkerExitReason.RequestedStop
            ) 0 else 1,
        )
        return if (terminalPending == 0) OwnerFinalizationResult.Finalized else OwnerFinalizationResult.Pending(
            LifecyclePendingComponent.TERMINAL_DRAIN,
            terminalPending,
        )
    }

    private suspend fun collectAttempts(
        workers: OwnerWorkers,
        lifecycle: SessionLifecycleRef,
        record: AttemptRecord?,
    ): Set<DeliveryAttemptRef>? = withTimeoutOrNull(transitionTimeoutMillis) {
        linkedSetOf<DeliveryAttemptRef>().apply {
            journal.activeAttempt(lifecycle.ownerBareJid)?.let(::add)
            record?.attempt?.let(::add)
            workers.drain.boundAttempt()?.let(::add)
            activeSession.attemptRef
                ?.takeIf { it.ownerBareJid == lifecycle.ownerBareJid }
                ?.let(::add)
        }
    }

    private suspend fun finalizeAttempts(
        attempts: Set<DeliveryAttemptRef>,
    ): Boolean = withTimeoutOrNull(transitionTimeoutMillis) {
        attempts.forEach { attempt ->
            journal.fenceAttempt(attempt)
            resume.retireAttempt(attempt)
            activeSession.endAttempt(attempt)
        }
        true
    } == true
}

private val DURABLE_CLEANUP_LOG: Logger =
    Logger.getLogger(OutboundLifecycleFinalizationOperations::class.java.name)
