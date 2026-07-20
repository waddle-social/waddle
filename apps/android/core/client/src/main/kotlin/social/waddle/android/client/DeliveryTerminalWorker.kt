package social.waddle.android.client

import java.io.IOException
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.logging.Level
import java.util.logging.Logger
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
import social.waddle.android.client.prefs.LifecycleGeneration
import social.waddle.android.client.prefs.ProcessEpoch
import social.waddle.android.client.prefs.TerminalClaimId
import social.waddle.android.client.prefs.TerminalReceiptClaimState
import social.waddle.android.client.prefs.TerminalReceiptClaimant
import social.waddle.android.client.prefs.TerminalReceiptEffect
import social.waddle.android.client.prefs.TerminalReceiptWorkerKind
import social.waddle.android.client.prefs.WorkerGeneration as ReceiptWorkerGeneration

/** Creates isolated terminal workers; each [Run] belongs to one lifecycle generation. */
internal class DeliveryTerminalWorker(
    private val journal: OutboundQueue,
    private val dispatchEvent: (XmppEvent) -> Unit,
    private val processEpoch: ProcessEpoch = DeliveryProcessEpoch.current,
    private val commandCapacity: Int = COMMAND_CAPACITY,
) {
    init {
        require(commandCapacity > 0) { "terminal command capacity must be positive" }
    }

    fun start(
        scope: CoroutineScope,
        ownership: WorkerOwnership,
        onReady: suspend (WorkerOwnership) -> Unit,
        onExit: suspend (WorkerExit) -> Unit,
    ): Run {
        check(ownership.kind == WorkerKind.DELIVERY_TERMINAL) {
            "delivery terminal worker requires a terminal ownership"
        }
        return Run(journal, dispatchEvent, processEpoch, commandCapacity, ownership, onReady, onExit, scope)
    }

    internal class Run internal constructor(
        private val journal: OutboundQueue,
        private val dispatchEvent: (XmppEvent) -> Unit,
        private val processEpoch: ProcessEpoch,
        commandCapacity: Int,
        val ownership: WorkerOwnership,
        private val onReady: suspend (WorkerOwnership) -> Unit,
        private val onExit: suspend (WorkerExit) -> Unit,
        scope: CoroutineScope,
    ) {
        private val commands = Channel<TerminalSignal>(commandCapacity)
        private val ready = CompletableDeferred<Unit>()
        private val startupDrain = CompletableDeferred<Unit>()
        private val exit = CompletableDeferred<WorkerExit>()
        private val pendingCommands = AtomicInteger()
        private val unavailable = AtomicBoolean()

        @Volatile
        private var stopRequested = false

        private val job: Job = scope.launch(start = CoroutineStart.UNDISPATCHED) { runWorker() }

        suspend fun awaitStartupDrain() = startupDrain.await()

        suspend fun awaitReady() = ready.await()

        suspend fun submitAndAwait(
            clientStanzaId: String,
            attempt: DeliveryAttemptRef,
            kind: DeliveryTerminalKind,
        ): TerminalCommandOutcome {
            if (attempt.ownerBareJid != ownership.lifecycle.ownerBareJid || unavailable.get()) {
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
            unavailable.set(true)
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
                withContext(NonCancellable) { onReady(ownership) }
                ready.complete(Unit)
                drainTerminalReceipts()
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
                ready.cancel(cancellation)
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
                ready.completeExceptionally(failure)
                startupDrain.completeExceptionally(failure)
                val cause = (failure as? TerminalReceiptApplicationException)?.failure
                    ?.let(WorkerFailureKind::TERMINAL_RECEIPT_APPLICATION)
                    ?: WorkerFailureKind.DEPENDENCY_FAILURE
                activeSignal?.committed?.complete(
                    TerminalCommandOutcome.Failed(TerminalWorkerFailure(cause)),
                )
                if (activeSignal != null) pendingCommands.decrementAndGet()
                activeSignal = null
                reason = WorkerExitReason.UnexpectedFailure(cause)
                LOGGER.log(Level.SEVERE, "delivery terminal worker stopped", failure)
            } finally {
                // Closing admission precedes callback fencing: a capability
                // retained before the callback can never wait on a dead run.
                unavailable.set(true)
                commands.close()
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

        private suspend fun drainTerminalReceipts() {
            val owner = social.waddle.android.client.prefs.DeliveryOwnerBareJid(ownership.lifecycle.ownerBareJid)
            when (val discovery = retryingResult("terminal receipt discovery") {
                journal.discoverTerminalReceipt(owner)
            }) {
                is TerminalReceiptDiscovery.Pending -> {
                    val claim = TerminalReceiptClaimState.Claimed(
                        id = TerminalClaimId.random(),
                        claimant = ownership.toTerminalReceiptClaimant(),
                        processEpoch = processEpoch,
                    )
                    val requested = TerminalReceiptClaimRequest(discovery.ref, claim)
                    when (val result = retryingResult("terminal receipt claim") {
                        journal.claimTerminalReceipt(requested)
                    }) {
                        is TerminalReceiptApplicationResult.Claimed -> applyClaimedReceipt(result)
                        is TerminalReceiptApplicationResult.Busy,
                        is TerminalReceiptApplicationResult.AlreadyAcknowledged,
                        is TerminalReceiptApplicationResult.None,
                        is TerminalReceiptApplicationResult.Stale,
                        -> Unit
                        is TerminalReceiptApplicationResult.Corrupt -> throw TerminalReceiptApplicationException(
                            TerminalReceiptApplicationFailure(TerminalReceiptOperation.CLAIM, result.reason),
                        )
                        is TerminalReceiptApplicationResult.Acknowledged,
                        is TerminalReceiptApplicationResult.Released,
                        -> throw TerminalReceiptApplicationException(
                            TerminalReceiptApplicationFailure(TerminalReceiptOperation.CLAIM, null),
                        )
                    }
                }
                is TerminalReceiptDiscovery.Corrupt -> throw TerminalReceiptApplicationException(
                    TerminalReceiptApplicationFailure(TerminalReceiptOperation.DISCOVERY, discovery.reason),
                )
                TerminalReceiptDiscovery.None,
                is TerminalReceiptDiscovery.AlreadyAcknowledged,
                -> Unit
                TerminalReceiptDiscovery.Stale -> throw TerminalReceiptApplicationException(
                    TerminalReceiptApplicationFailure(
                        TerminalReceiptOperation.DISCOVERY,
                        TerminalReceiptCorruption.ACTIVE_OWNER_MISMATCH,
                    ),
                )
            }
        }

        private suspend fun applyClaimedReceipt(claimed: TerminalReceiptApplicationResult.Claimed) {
            try {
                claimed.effects.forEach(::applyReceiptEffect)
                when (val acknowledgement = retryingResult("terminal receipt acknowledge") {
                    journal.acknowledgeTerminalReceipt(claimed.lease)
                }) {
                    is TerminalReceiptApplicationResult.Acknowledged,
                    is TerminalReceiptApplicationResult.AlreadyAcknowledged,
                    -> Unit
                    is TerminalReceiptApplicationResult.Busy,
                    is TerminalReceiptApplicationResult.None,
                    is TerminalReceiptApplicationResult.Stale,
                    is TerminalReceiptApplicationResult.Released,
                    is TerminalReceiptApplicationResult.Claimed,
                    -> throw TerminalReceiptApplicationException(
                        TerminalReceiptApplicationFailure(TerminalReceiptOperation.ACKNOWLEDGE, null),
                    )
                    is TerminalReceiptApplicationResult.Corrupt -> throw TerminalReceiptApplicationException(
                        TerminalReceiptApplicationFailure(TerminalReceiptOperation.ACKNOWLEDGE, acknowledgement.reason),
                    )
                }
            } catch (cancellation: CancellationException) {
                releaseReceiptClaim(claimed.lease)?.let { releaseFailure ->
                    cancellation.addSuppressed(TerminalReceiptApplicationException(releaseFailure))
                }
                throw cancellation
            } catch (failure: Throwable) {
                releaseReceiptClaim(claimed.lease)?.let { releaseFailure ->
                    failure.addSuppressed(TerminalReceiptApplicationException(releaseFailure))
                }
                throw failure
            }
        }

        private suspend fun releaseReceiptClaim(
            lease: TerminalReceiptLease,
        ): TerminalReceiptApplicationFailure? = withContext(NonCancellable) {
                try {
                    when (val release = journal.releaseTerminalReceipt(lease)) {
                        is TerminalReceiptApplicationResult.Released,
                        is TerminalReceiptApplicationResult.AlreadyAcknowledged,
                        is TerminalReceiptApplicationResult.Stale,
                        is TerminalReceiptApplicationResult.None,
                        -> null
                        is TerminalReceiptApplicationResult.Busy,
                        is TerminalReceiptApplicationResult.Claimed,
                        is TerminalReceiptApplicationResult.Acknowledged,
                        -> TerminalReceiptApplicationFailure(TerminalReceiptOperation.RELEASE, null)
                        is TerminalReceiptApplicationResult.Corrupt ->
                            TerminalReceiptApplicationFailure(TerminalReceiptOperation.RELEASE, release.reason)
                    }
                } catch (failure: IOException) {
                    LOGGER.log(Level.WARNING, "terminal receipt release failed", failure)
                    TerminalReceiptApplicationFailure(TerminalReceiptOperation.RELEASE, null)
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

        private fun applyReceiptEffect(effect: TerminalReceiptEffect) {
            when (effect) {
                is TerminalReceiptEffect.Acknowledged -> dispatchEvent(
                    XmppEvent.DeliveryAcked(DeliveryOutcomeRef(effect.row.identity, effect.row.source)),
                )
                is TerminalReceiptEffect.Failed -> dispatchEvent(
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
                } catch (failure: IOException) {
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
                } catch (failure: IOException) {
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

internal class TerminalReceiptApplicationException(
    val failure: TerminalReceiptApplicationFailure,
) : IllegalStateException()

private fun WorkerOwnership.toTerminalReceiptClaimant(): TerminalReceiptClaimant.Worker =
    TerminalReceiptClaimant.Worker(
        lifecycleGeneration = LifecycleGeneration(lifecycle.id.value.toString()),
        kind = TerminalReceiptWorkerKind.DELIVERY_TERMINAL,
        workerGeneration = ReceiptWorkerGeneration(generation.value.toString()),
    )
