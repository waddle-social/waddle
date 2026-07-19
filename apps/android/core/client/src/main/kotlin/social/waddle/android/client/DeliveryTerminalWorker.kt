package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.yield
import social.waddle.android.client.OutboundQueue.TerminalEffect
import social.waddle.android.client.OutboundQueue.TerminalRecordResult
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryTerminalKind
import java.util.concurrent.atomic.AtomicInteger
import java.util.logging.Level
import java.util.logging.Logger

/** Creates isolated terminal workers; each [Run] belongs to one lifecycle generation. */
internal class DeliveryTerminalWorker(
    private val journal: OutboundQueue,
    private val dispatchEvent: (XmppEvent) -> Unit,
    private val commandCapacity: Int = COMMAND_CAPACITY,
) {
    init {
        require(commandCapacity > 0) { "terminal command capacity must be positive" }
    }

    fun start(
        scope: CoroutineScope,
        ownership: WorkerOwnership,
        onExit: suspend (WorkerExit) -> Unit,
    ): Run {
        check(ownership.kind == WorkerKind.DELIVERY_TERMINAL) {
            "delivery terminal worker requires a terminal ownership"
        }
        return Run(journal, dispatchEvent, commandCapacity, ownership, onExit, scope)
    }

    internal class Run internal constructor(
        private val journal: OutboundQueue,
        private val dispatchEvent: (XmppEvent) -> Unit,
        commandCapacity: Int,
        val ownership: WorkerOwnership,
        private val onExit: suspend (WorkerExit) -> Unit,
        scope: CoroutineScope,
    ) {
        private val commands = Channel<TerminalSignal>(commandCapacity)
        private val startupDrain = CompletableDeferred<Unit>()
        private val exit = CompletableDeferred<WorkerExit>()
        private val pendingCommands = AtomicInteger()

        @Volatile
        private var stopRequested = false

        private val job: Job = scope.launch(start = CoroutineStart.UNDISPATCHED) { runWorker() }

        suspend fun awaitStartupDrain() = startupDrain.await()

        suspend fun submitAndAwait(
            clientStanzaId: String,
            attempt: DeliveryAttemptRef,
            kind: DeliveryTerminalKind,
        ): TerminalCommandOutcome {
            if (attempt.ownerBareJid != ownership.lifecycle.ownerBareJid || stopRequested) {
                return TerminalCommandOutcome.WorkerUnavailable
            }
            val committed = CompletableDeferred<TerminalCommandOutcome>()
            val signal = TerminalSignal(clientStanzaId, attempt, kind, committed)
            pendingCommands.incrementAndGet()
            try {
                commands.send(signal)
            } catch (cancellation: CancellationException) {
                pendingCommands.decrementAndGet()
                throw cancellation
            } catch (_: Throwable) {
                pendingCommands.decrementAndGet()
                return TerminalCommandOutcome.WorkerUnavailable
            }
            return committed.await()
        }

        fun requestStop() {
            stopRequested = true
            commands.close()
        }

        suspend fun awaitExit(timeoutMillis: Long): WorkerAwaitOutcome =
            withTimeoutOrNull(timeoutMillis) { WorkerAwaitOutcome.Exited(exit.await()) }
                ?: WorkerAwaitOutcome.TimedOut

        fun pendingCommandCount(): Int = pendingCommands.get().coerceAtLeast(0)

        private suspend fun runWorker() {
            var reason: WorkerExitReason? = null
            var activeSignal: TerminalSignal? = null
            try {
                yield()
                drainPersisted()
                startupDrain.complete(Unit)
                for (signal in commands) {
                    activeSignal = signal
                    record(signal)
                    drainPersisted()
                    signal.committed.complete(TerminalCommandOutcome.Committed)
                    pendingCommands.decrementAndGet()
                    activeSignal = null
                }
            } catch (cancellation: CancellationException) {
                startupDrain.cancel(cancellation)
                activeSignal?.committed?.complete(TerminalCommandOutcome.WorkerUnavailable)
                if (activeSignal != null) pendingCommands.decrementAndGet()
                activeSignal = null
                reason = if (stopRequested) {
                    WorkerExitReason.RequestedStop
                } else {
                    WorkerExitReason.OwnerScopeCancelled
                }
            } catch (failure: Throwable) {
                startupDrain.completeExceptionally(failure)
                val cause = WorkerFailureCause.from(failure)
                activeSignal?.committed?.complete(
                    TerminalCommandOutcome.Failed(TerminalWorkerFailure(cause)),
                )
                if (activeSignal != null) pendingCommands.decrementAndGet()
                activeSignal = null
                reason = WorkerExitReason.UnexpectedFailure(cause)
                LOGGER.log(Level.SEVERE, "delivery terminal worker stopped", failure)
            } finally {
                rejectQueuedCommands()
                val exactExit = WorkerExit(
                    lifecycle = ownership.lifecycle,
                    generation = ownership.generation,
                    kind = ownership.kind,
                    reason = reason ?: if (stopRequested) {
                        WorkerExitReason.RequestedStop
                    } else {
                        WorkerExitReason.UnexpectedReturn
                    },
                )
                val exitCallbackFailure = try {
                    withContext(NonCancellable) { onExit(exactExit) }
                    null
                } catch (failure: Throwable) {
                    failure
                }
                if (exitCallbackFailure == null) {
                    exit.complete(exactExit)
                } else {
                    exit.completeExceptionally(exitCallbackFailure)
                }
            }
        }

        private fun rejectQueuedCommands() {
            while (true) {
                val signal = commands.tryReceive().getOrNull() ?: return
                signal.committed.complete(TerminalCommandOutcome.WorkerUnavailable)
                pendingCommands.decrementAndGet()
            }
        }

        private suspend fun record(signal: TerminalSignal) {
            retrying("terminal signal record") {
                when (
                    journal.recordTerminal(
                        ownerBareJid = ownership.lifecycle.ownerBareJid,
                        clientStanzaId = signal.clientStanzaId,
                        attempt = signal.attempt,
                        kind = signal.kind,
                    )
                ) {
                    is TerminalRecordResult.Recorded,
                    TerminalRecordResult.Stale,
                    -> true
                }
            }
        }

        private suspend fun drainPersisted() {
            while (retryingResult("terminal intent inspection") {
                journal.hasTerminalIntents(ownership.lifecycle.ownerBareJid)
            }) {
                val effect = retryingResult("terminal intent apply") {
                    journal.applyNextTerminal(ownership.lifecycle.ownerBareJid)
                }
                if (effect != null) applyEffect(effect)
            }
        }

        private fun applyEffect(effect: TerminalEffect) {
            when (effect) {
                is TerminalEffect.Acknowledged -> dispatchEvent(
                    XmppEvent.DeliveryAcked(DeliveryOutcomeRef(effect.row.identity, effect.row.source)),
                )
                is TerminalEffect.Failed -> dispatchEvent(
                    XmppEvent.DeliveryFailed(DeliveryOutcomeRef(effect.row.identity, effect.row.source)),
                )
            }
        }

        private suspend fun retrying(label: String, operation: suspend () -> Boolean): Boolean {
            var retryIndex = 0
            while (true) {
                try {
                    return operation()
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (failure: Throwable) {
                    LOGGER.log(Level.WARNING, "$label failed; retrying", failure)
                    delay(RETRY_DELAYS_MILLIS[retryIndex.coerceAtMost(RETRY_DELAYS_MILLIS.lastIndex)])
                    if (retryIndex < RETRY_DELAYS_MILLIS.lastIndex) retryIndex += 1
                }
            }
        }

        private suspend fun <T> retryingResult(label: String, operation: suspend () -> T): T {
            var retryIndex = 0
            while (true) {
                try {
                    return operation()
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (failure: Throwable) {
                    LOGGER.log(Level.WARNING, "$label failed; retrying", failure)
                    delay(RETRY_DELAYS_MILLIS[retryIndex.coerceAtMost(RETRY_DELAYS_MILLIS.lastIndex)])
                    if (retryIndex < RETRY_DELAYS_MILLIS.lastIndex) retryIndex += 1
                }
            }
        }

        private data class TerminalSignal(
            val clientStanzaId: String,
            val attempt: DeliveryAttemptRef,
            val kind: DeliveryTerminalKind,
            val committed: CompletableDeferred<TerminalCommandOutcome>,
        )
    }

    private companion object {
        val LOGGER: Logger = Logger.getLogger(DeliveryTerminalWorker::class.java.name)
        val RETRY_DELAYS_MILLIS = longArrayOf(250L, 500L, 1_000L, 2_000L, 5_000L)
        const val COMMAND_CAPACITY = 256
    }
}
