package social.waddle.android.client

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
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
    ) : OwnerFinalizationResult
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
) {
    fun startWorkers(
        scope: CoroutineScope,
        lifecycle: SessionLifecycleRef,
    ): OwnerWorkers {
        val workers = OwnerWorkers(lifecycle)
        workers.terminal = terminalWorker.start(
            scope,
            WorkerOwnership(lifecycle, WorkerKind.DELIVERY_TERMINAL, WorkerGeneration.random()),
            workers::recordExit,
        )
        workers.drain = drainWorker.start(
            scope,
            WorkerOwnership(lifecycle, WorkerKind.OUTBOUND_DRAIN, WorkerGeneration.random()),
            workers::recordExit,
        )
        return workers
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
    ) {
        if (active?.attempt == attempt) {
            workers?.takeIf { it.lifecycle == active.lifecycle }
                ?.drain
                ?.signal(active.handle, attempt)
        }
    }

    suspend fun disconnect(claim: DisconnectClaim): Boolean {
        if (claim is DisconnectClaim.Execute) {
            val client = claim.record.client
            val disconnected = if (client == null) {
                true
            } else {
                withTimeoutOrNull(transitionTimeoutMillis) {
                    runCatching { client.disconnect() }.isSuccess
                } == true
            }
            claim.result.complete(disconnected)
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

    suspend fun terminalRecoveryReady(workers: OwnerWorkers, ownerBareJid: String): Boolean {
        if (workers.terminal.awaitExit(transitionTimeoutMillis) !is WorkerAwaitOutcome.Exited) return false
        return withTimeoutOrNull(transitionTimeoutMillis) {
            linkedSetOf<DeliveryAttemptRef>().apply {
                journal.activeAttempt(ownerBareJid)?.let(::add)
                activeSession.attemptRef
                    ?.takeIf { it.ownerBareJid == ownerBareJid }
                    ?.let(::add)
            }.forEach { attempt ->
                journal.fenceAttempt(attempt)
                resume.retireAttempt(attempt)
                activeSession.endAttempt(attempt)
            }
            true
        } == true
    }

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
        workers.terminal.requestStop()
        val terminalResult = workers.terminal.awaitExit(transitionTimeoutMillis)
        val pendingCommands = workers.terminal.pendingCommandCount()
        if (terminalResult is WorkerAwaitOutcome.TimedOut && pendingCommands > 0) {
            return OwnerFinalizationResult.Pending(
                LifecyclePendingComponent.TERMINAL_DRAIN,
                maxOf(
                    pendingCommands,
                    runCatching { journal.terminalIntentCount(lifecycle.ownerBareJid) }.getOrDefault(1),
                ).coerceAtLeast(1),
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
                maxOf(
                    pendingCommands,
                    runCatching { journal.terminalIntentCount(lifecycle.ownerBareJid) }.getOrDefault(1),
                ).coerceAtLeast(1),
            )
        }
        val terminalPending = maxOf(
            pendingCommands,
            runCatching { journal.terminalIntentCount(lifecycle.ownerBareJid) }.getOrDefault(0),
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
