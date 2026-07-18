package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.OutboundQueue.TerminalEffect
import social.waddle.android.client.OutboundQueue.TerminalRecordResult
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryTerminalKind
import java.util.concurrent.atomic.AtomicInteger
import java.util.logging.Level
import java.util.logging.Logger

/**
 * One serialized record/apply worker per authenticated owner.
 *
 * The ordered native pull lane awaits each command's durable record/apply
 * barrier before polling Rust again. Other message-send paths use the same
 * worker, so both the one-shot signal record and persisted-intent apply use
 * the required bounded retry cadence and then retry every five seconds until
 * success or stale fencing.
 */
internal class DeliveryTerminalWorker(
    private val journal: OutboundQueue,
    private val dispatchEvent: (XmppEvent) -> Unit,
    private val commandCapacity: Int = COMMAND_CAPACITY,
    private val stopTimeoutMillis: Long = STOP_TIMEOUT_MILLIS,
) {
    init {
        require(commandCapacity > 0) { "terminal command capacity must be positive" }
        require(stopTimeoutMillis > 0) { "terminal stop timeout must be positive" }
    }

    sealed interface StopResult {
        data object Drained : StopResult

        data class FencedWithPending(
            val ownerBareJid: String,
            val pendingCommands: Int,
        ) : StopResult
    }

    @Volatile
    private var commands = Channel<TerminalSignal>(commandCapacity)

    @Volatile
    private var workerJob: Job? = null

    @Volatile
    private var activeOwnerBareJid: String? = null

    @Volatile
    private var startupDrain = CompletableDeferred<Unit>()

    @Volatile
    private var workerFailure: Throwable? = null

    private val pendingCommands = AtomicInteger()

    fun start(scope: CoroutineScope, ownerBareJid: String) {
        check(workerJob == null) { "delivery terminal worker already started" }
        activeOwnerBareJid = ownerBareJid
        workerFailure = null
        pendingCommands.set(0)
        commands = Channel(commandCapacity)
        startupDrain = CompletableDeferred()
        val commandStream = commands
        val barrier = startupDrain
        workerJob = scope.launch {
            try {
                drainPersisted(ownerBareJid)
                barrier.complete(Unit)
                for (signal in commandStream) {
                    try {
                        check(signal.ownerBareJid == ownerBareJid) {
                            "terminal command owner changed inside one worker"
                        }
                        record(signal)
                        drainPersisted(ownerBareJid)
                        signal.committed?.complete(Unit)
                    } catch (cancellation: CancellationException) {
                        signal.committed?.cancel(cancellation)
                        throw cancellation
                    } catch (failure: Throwable) {
                        // A malformed command cannot kill the supervised
                        // owner worker or strand later bounded admissions.
                        signal.committed?.completeExceptionally(failure)
                        LOGGER.log(Level.SEVERE, "delivery terminal command failed", failure)
                    } finally {
                        pendingCommands.decrementAndGet()
                    }
                }
            } catch (cancellation: CancellationException) {
                barrier.cancel(cancellation)
                throw cancellation
            } catch (failure: Throwable) {
                // Defensive last boundary: operation failures are retried
                // below, but a programming/runtime failure must not cancel
                // the sibling connection loop under the SupervisorJob.
                barrier.completeExceptionally(failure)
                workerFailure = failure
                LOGGER.log(Level.SEVERE, "delivery terminal worker stopped", failure)
            } finally {
                rejectQueuedCommands(commandStream)
            }
        }
    }

    suspend fun awaitStartupDrain(ownerBareJid: String) {
        if (activeOwnerBareJid != ownerBareJid) return
        startupDrain.await()
    }

    fun canRestart(): Boolean =
        workerJob == null && activeOwnerBareJid == null

    /**
     * Ordered native-poll barrier: record and apply the callback before the
     * connection loop asks Rust for another event.
     */
    suspend fun submitAndAwait(
        ownerBareJid: String,
        clientStanzaId: String,
        attempt: DeliveryAttemptRef,
        kind: DeliveryTerminalKind,
    ) {
        val committed = CompletableDeferred<Unit>()
        val signal = TerminalSignal(ownerBareJid, clientStanzaId, attempt, kind, committed)
        pendingCommands.incrementAndGet()
        try {
            commands.send(signal)
        } catch (failure: Throwable) {
            pendingCommands.decrementAndGet()
            throw failure
        }
        committed.await()
    }

    suspend fun stop(ownerBareJid: String): StopResult {
        if (activeOwnerBareJid != ownerBareJid) return StopResult.Drained
        activeOwnerBareJid = null
        commands.close()
        val job = workerJob
        val drained = withTimeoutOrNull(stopTimeoutMillis) {
            job?.join()
            true
        } == true
        if (drained) {
            val unresolved = maxOf(
                pendingCommands.get().coerceAtLeast(0),
                runCatching { journal.terminalIntentCount(ownerBareJid) }.getOrDefault(0),
                if (workerFailure == null) 0 else 1,
            )
            workerJob = null
            if (unresolved == 0) return StopResult.Drained
            LOGGER.log(
                Level.SEVERE,
                "delivery terminal worker stopped with unresolved work; owner={0}, pending={1}",
                arrayOf<Any>(ownerBareJid, unresolved),
            )
            return StopResult.FencedWithPending(ownerBareJid, unresolved)
        }

        val pendingAtFence = maxOf(
            pendingCommands.get().coerceAtLeast(0),
            runCatching { journal.terminalIntentCount(ownerBareJid) }.getOrDefault(0),
        )
        LOGGER.log(
            Level.SEVERE,
            "delivery terminal shutdown timed out; owner={0}, pending={1}; journal retained for restart",
            arrayOf<Any>(ownerBareJid, pendingAtFence),
        )
        job?.cancel()
        val cancelled = withTimeoutOrNull(stopTimeoutMillis) {
            job?.join()
            true
        } == true
        if (cancelled) workerJob = null
        return StopResult.FencedWithPending(ownerBareJid, pendingAtFence)
    }

    private fun rejectQueuedCommands(commandStream: Channel<TerminalSignal>) {
        while (true) {
            val signal = commandStream.tryReceive().getOrNull() ?: return
            signal.committed?.completeExceptionally(
                IllegalStateException(
                    "delivery terminal worker fenced before durable command completion",
                ),
            )
            pendingCommands.decrementAndGet()
        }
    }

    private suspend fun record(signal: TerminalSignal) {
        retrying("terminal signal record") {
            when (
                journal.recordTerminal(
                    ownerBareJid = signal.ownerBareJid,
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

    private suspend fun drainPersisted(ownerBareJid: String) {
        while (
            retryingResult("terminal intent inspection") {
                journal.hasTerminalIntents(ownerBareJid)
            }
        ) {
            val effect = retryingResult("terminal intent apply") {
                journal.applyNextTerminal(ownerBareJid)
            }
            if (effect != null) applyEffect(effect)
        }
    }

    private suspend fun applyEffect(effect: TerminalEffect) {
        when (effect) {
            is TerminalEffect.Acknowledged -> dispatchEvent(
                XmppEvent.DeliveryAcked(
                    DeliveryOutcomeRef(effect.row.identity, effect.row.source),
                ),
            )
            is TerminalEffect.Failed -> dispatchEvent(
                XmppEvent.DeliveryFailed(
                    DeliveryOutcomeRef(effect.row.identity, effect.row.source),
                ),
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
        val ownerBareJid: String,
        val clientStanzaId: String,
        val attempt: DeliveryAttemptRef,
        val kind: DeliveryTerminalKind,
        val committed: CompletableDeferred<Unit>?,
    )

    private companion object {
        val LOGGER: Logger = Logger.getLogger(DeliveryTerminalWorker::class.java.name)
        val RETRY_DELAYS_MILLIS = longArrayOf(250L, 500L, 1_000L, 2_000L, 5_000L)
        const val COMMAND_CAPACITY = 256
        const val STOP_TIMEOUT_MILLIS = 30_000L
    }
}
