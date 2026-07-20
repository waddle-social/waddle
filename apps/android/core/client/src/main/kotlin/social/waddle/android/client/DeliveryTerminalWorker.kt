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
import kotlinx.serialization.SerializationException
import social.waddle.android.client.prefs.DeliveryJournalDecodeException
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

        /** Written before this run exits and consumed only by its fenced recovery claim. */
        @Volatile
        private var unresolvedReceiptCleanup: TerminalReceiptCleanupException? = null

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
                WorkerExitExceptionEvidence.record(ownership, cancellation)
                activeSignal?.committed?.complete(TerminalCommandOutcome.WorkerUnavailable)
                if (activeSignal != null) pendingCommands.decrementAndGet()
                activeSignal = null
                reason = when (val receiptFailure = terminalReceiptFailure(cancellation)) {
                    is TerminalReceiptFailureExtraction.Found ->
                        WorkerExitReason.UnexpectedFailure(
                            WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION(receiptFailure.failure),
                        )
                    TerminalReceiptFailureExtraction.None -> if (stopRequested) {
                        WorkerExitReason.RequestedStop
                    } else {
                        WorkerExitReason.OwnerScopeCancelled
                    }
                }
            } catch (failure: Throwable) {
                ready.completeExceptionally(failure)
                startupDrain.completeExceptionally(failure)
                WorkerExitExceptionEvidence.record(ownership, failure)
                val cause = when (val receiptFailure = terminalReceiptFailure(failure)) {
                    is TerminalReceiptFailureExtraction.Found ->
                        WorkerFailureKind.TERMINAL_RECEIPT_APPLICATION(receiptFailure.failure)
                    TerminalReceiptFailureExtraction.None -> WorkerFailureKind.DEPENDENCY_FAILURE
                }
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
            try {
                drainTerminalReceipts(owner)
            } catch (failure: DeliveryJournalDecodeException) {
                throw TerminalReceiptApplicationException(
                    TerminalReceiptApplicationFailure.DiscoveryCorrupt(
                        owner,
                        TerminalReceiptCorruption.PERSISTED_DECODE_FAILURE,
                    ),
                    failure,
                )
            }
        }

        private suspend fun drainTerminalReceipts(owner: social.waddle.android.client.prefs.DeliveryOwnerBareJid) {
            when (val discovery = receiptRetrying(
                operation = TerminalReceiptPersistenceOperation.DISCOVERY,
                owner = owner,
                receipt = null,
            ) { journal.discoverTerminalReceipt(owner) }) {
                is TerminalReceiptDiscovery.Pending -> {
                    val claim = TerminalReceiptClaimState.Claimed(
                        id = TerminalClaimId.random(),
                        claimant = ownership.toTerminalReceiptClaimant(),
                        processEpoch = processEpoch,
                    )
                    val requested = TerminalReceiptClaimRequest(discovery.ref, claim)
                    when (val result = receiptRetrying(
                        operation = TerminalReceiptPersistenceOperation.CLAIM,
                        owner = owner,
                        receipt = discovery.ref,
                    ) { journal.claimTerminalReceipt(requested) }) {
                        is TerminalReceiptClaimResult.Claimed -> applyClaimedReceipt(result)
                        is TerminalReceiptClaimResult.AlreadyAcknowledged -> Unit
                        is TerminalReceiptClaimResult.Busy -> throw TerminalReceiptApplicationException(
                            TerminalReceiptApplicationFailure.ClaimBusy(result),
                        )
                        is TerminalReceiptClaimResult.ReceiptMissing -> throw TerminalReceiptApplicationException(
                            TerminalReceiptApplicationFailure.ClaimMissing(result),
                        )
                        is TerminalReceiptClaimResult.ReceiptReplaced -> throw TerminalReceiptApplicationException(
                            TerminalReceiptApplicationFailure.ClaimReplaced(result),
                        )
                        is TerminalReceiptClaimResult.OwnerFenced -> throw TerminalReceiptApplicationException(
                            TerminalReceiptApplicationFailure.ClaimOwnerFenced(result),
                        )
                        is TerminalReceiptClaimResult.Corrupt -> throw TerminalReceiptApplicationException(
                            TerminalReceiptApplicationFailure.ClaimCorrupt(result),
                        )
                    }
                }
                is TerminalReceiptDiscovery.Corrupt -> throw TerminalReceiptApplicationException(
                    TerminalReceiptApplicationFailure.DiscoveryCorrupt(owner, discovery.reason),
                )
                TerminalReceiptDiscovery.None,
                is TerminalReceiptDiscovery.AlreadyAcknowledged,
                -> Unit
                is TerminalReceiptDiscovery.OwnerFenced -> throw TerminalReceiptApplicationException(
                    TerminalReceiptApplicationFailure.DiscoveryOwnerFenced(
                        discovery.requested,
                        discovery.actual,
                    ),
                )
            }
        }

        private suspend fun applyClaimedReceipt(claimed: TerminalReceiptClaimResult.Claimed) {
            try {
                claimed.effects.forEach(::applyReceiptEffect)
                when (val acknowledgement = receiptRetrying(
                    operation = TerminalReceiptPersistenceOperation.ACKNOWLEDGE,
                    owner = claimed.lease.ref.owner,
                    receipt = claimed.lease.ref,
                ) { journal.acknowledgeTerminalReceipt(claimed.lease) }) {
                    is TerminalReceiptAcknowledgeResult.Acknowledged,
                    is TerminalReceiptAcknowledgeResult.AlreadyAcknowledged,
                    -> Unit
                    is TerminalReceiptAcknowledgeResult.LeaseMismatch -> throw TerminalReceiptApplicationException(TerminalReceiptApplicationFailure.AcknowledgeLeaseMismatch(acknowledgement))
                    is TerminalReceiptAcknowledgeResult.ReceiptMissing -> throw TerminalReceiptApplicationException(TerminalReceiptApplicationFailure.AcknowledgeMissing(acknowledgement))
                    is TerminalReceiptAcknowledgeResult.ReceiptReplaced -> throw TerminalReceiptApplicationException(TerminalReceiptApplicationFailure.AcknowledgeReplaced(acknowledgement))
                    is TerminalReceiptAcknowledgeResult.Corrupt -> throw TerminalReceiptApplicationException(TerminalReceiptApplicationFailure.AcknowledgeCorrupt(acknowledgement))
                }
            } catch (cancellation: CancellationException) {
                preservePrimaryWithRelease(cancellation, claimed.lease)
                throw cancellation
            } catch (failure: Throwable) {
                preservePrimaryWithRelease(failure, claimed.lease)
                throw failure
            }
        }

        private suspend fun preservePrimaryWithRelease(primary: Throwable, lease: TerminalReceiptLease) {
            try {
                when (val cleanup = releaseReceiptClaim(lease)) {
                    TerminalReceiptCleanupResult.Released -> Unit
                    is TerminalReceiptCleanupResult.Unresolved -> {
                        val failure = TerminalReceiptCleanupException(cleanup.evidence)
                        unresolvedReceiptCleanup = failure
                        primary.addSuppressed(failure)
                    }
                }
            } catch (failure: TerminalReceiptCleanupException) {
                unresolvedReceiptCleanup = failure
                primary.addSuppressed(failure)
            }
        }

        /**
         * Fenced recovery never replays callbacks. It retries only the exact
         * durable release that prevented this run from completing.
         */
        suspend fun recoverUnresolvedReceiptCleanup(): TerminalReceiptRecoveryCleanupResult {
            val pending = unresolvedReceiptCleanup ?: return TerminalReceiptRecoveryCleanupResult.NoPendingLease
            return when (val recovered = releaseReceiptClaim(pending.evidence.lease)) {
                TerminalReceiptCleanupResult.Released -> {
                    unresolvedReceiptCleanup = null
                    TerminalReceiptRecoveryCleanupResult.Released
                }
                is TerminalReceiptCleanupResult.Unresolved ->
                    TerminalReceiptRecoveryCleanupResult.Unresolved(recovered.evidence)
            }
        }

        private suspend fun releaseReceiptClaim(
            lease: TerminalReceiptLease,
        ): TerminalReceiptCleanupResult = withContext(NonCancellable) {
            var attempts = 0
            var lastFailure: Throwable? = null
            while (attempts < RECEIPT_MAX_ATTEMPTS) {
                try {
                    attempts += 1
                    when (val release = journal.releaseTerminalReceipt(lease)) {
                        is TerminalReceiptReleaseResult.Released,
                        is TerminalReceiptReleaseResult.AlreadyAcknowledged,
                        -> return@withContext TerminalReceiptCleanupResult.Released
                        is TerminalReceiptReleaseResult.LeaseMismatch
                        -> return@withContext TerminalReceiptCleanupResult.Unresolved(
                            cleanupEvidence(lease, TerminalReceiptCleanupReason.LeaseMismatch(release.current), attempts),
                        )
                        is TerminalReceiptReleaseResult.ReceiptMissing ->
                            return@withContext TerminalReceiptCleanupResult.Unresolved(
                                cleanupEvidence(lease, TerminalReceiptCleanupReason.ReceiptMissing, attempts),
                            )
                        is TerminalReceiptReleaseResult.ReceiptReplaced ->
                            return@withContext TerminalReceiptCleanupResult.Unresolved(
                                cleanupEvidence(lease, TerminalReceiptCleanupReason.ReceiptReplaced(release.actual), attempts),
                            )
                        is TerminalReceiptReleaseResult.Corrupt ->
                            return@withContext TerminalReceiptCleanupResult.Unresolved(
                                cleanupEvidence(lease, TerminalReceiptCleanupReason.Corrupt(release.reason), attempts),
                            )
                    }
                } catch (failure: Throwable) {
                    lastFailure = failure
                    LOGGER.log(Level.WARNING, "terminal receipt release failed", failure)
                    if (attempts == RECEIPT_MAX_ATTEMPTS) {
                        throw unresolvedCleanup(lease, cleanupCategory(failure), failure, attempts)
                    }
                    delay(RECEIPT_RETRY_DELAYS_MILLIS[attempts - 1])
                }
            }
            throw unresolvedCleanup(
                lease,
                cleanupCategory(checkNotNull(lastFailure)),
                checkNotNull(lastFailure),
                attempts,
            )
        }

        private fun unresolvedCleanup(
            lease: TerminalReceiptLease,
            category: TerminalReceiptCleanupFailureCategory,
            cause: Throwable? = null,
            attempts: Int = 1,
        ): TerminalReceiptCleanupException = TerminalReceiptCleanupException(
            cleanupEvidence(lease, TerminalReceiptCleanupReason.Persistence(category), attempts),
            cause,
        )

        private fun cleanupEvidence(
            lease: TerminalReceiptLease,
            reason: TerminalReceiptCleanupReason,
            attempts: Int = 1,
        ): TerminalReceiptCleanupEvidence = TerminalReceiptCleanupEvidence(lease, attempts, reason)

        private fun cleanupCategory(failure: Throwable): TerminalReceiptCleanupFailureCategory = when {
            failure is DeliveryJournalDecodeException &&
                generateSequence<Throwable>(failure) { it.cause }.any { it is SerializationException } ->
                TerminalReceiptCleanupFailureCategory.CODEC_FAILURE
            failure is IOException -> TerminalReceiptCleanupFailureCategory.IO_FAILURE
            failure is CancellationException -> TerminalReceiptCleanupFailureCategory.CANCELLATION
            failure is SerializationException -> TerminalReceiptCleanupFailureCategory.CODEC_FAILURE
            failure is Error -> TerminalReceiptCleanupFailureCategory.ERROR_FAILURE
            failure is IllegalStateException -> TerminalReceiptCleanupFailureCategory.INVARIANT_FAILURE
            else -> TerminalReceiptCleanupFailureCategory.RUNTIME_FAILURE
        }

        private fun terminalReceiptFailure(failure: Throwable): TerminalReceiptFailureExtraction {
            val application = failure as? TerminalReceiptApplicationException
            if (application != null) return TerminalReceiptFailureExtraction.Found(application.failure)
            val cleanup = failure.suppressed
                .filterIsInstance<TerminalReceiptCleanupException>()
                .lastOrNull()
            return if (cleanup == null) {
                TerminalReceiptFailureExtraction.None
            } else {
                TerminalReceiptFailureExtraction.Found(
                    TerminalReceiptApplicationFailure.CleanupUnresolved(cleanup.evidence),
                )
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

        /** Receipt persistence is bounded; legacy terminal intents retain their established retry policy. */
        private suspend fun <T> receiptRetrying(
            operation: TerminalReceiptPersistenceOperation,
            owner: social.waddle.android.client.prefs.DeliveryOwnerBareJid,
            receipt: TerminalReceiptRef?,
            block: suspend () -> T,
        ): T {
            var retryIndex = 0
            var attempts = 0
            while (true) {
                try {
                    attempts += 1
                    return block()
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (failure: IOException) {
                    if (retryIndex >= RECEIPT_RETRY_DELAYS_MILLIS.size) {
                        throw TerminalReceiptApplicationException(
                            TerminalReceiptApplicationFailure.PersistenceExhausted(
                                operation,
                                owner,
                                receipt,
                                attempts,
                            ),
                            failure,
                        )
                    }
                    delay(RECEIPT_RETRY_DELAYS_MILLIS[retryIndex])
                    retryIndex += 1
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
        val RECEIPT_RETRY_DELAYS_MILLIS = longArrayOf(250L, 500L, 1_000L, 2_000L, 5_000L)
        const val RECEIPT_MAX_ATTEMPTS = 6
        const val COMMAND_CAPACITY = 256

    }
}

internal class TerminalReceiptApplicationException(
    val failure: TerminalReceiptApplicationFailure,
    cause: Throwable? = null,
) : IllegalStateException("terminal receipt application failed: ${failure::class.simpleName}", cause)

/**
 * Cleanup failure is an exception rather than a protocol value so a primary
 * callback, acknowledgement, cancellation, runtime failure, or Error keeps
 * its exact identity and receives this failure through suppression.
 */
internal class TerminalReceiptCleanupException(
    val evidence: TerminalReceiptCleanupEvidence,
    cause: Throwable? = null,
) : IllegalStateException(
    "terminal receipt cleanup failed: ${evidence.reason}",
    cause,
)

private fun WorkerOwnership.toTerminalReceiptClaimant(): TerminalReceiptClaimant.Worker =
    TerminalReceiptClaimant.Worker(
        lifecycleGeneration = LifecycleGeneration(lifecycle.id.value.toString()),
        kind = TerminalReceiptWorkerKind.DELIVERY_TERMINAL,
        workerGeneration = ReceiptWorkerGeneration(generation.value.toString()),
    )
