package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.yield
import social.waddle.android.client.prefs.DeliveryAttemptRef
import java.util.logging.Level
import java.util.logging.Logger

/** Creates isolated outbound-drain workers; lifecycle state is held only by [Run]. */
internal class OutboundDrainWorker(
    private val drain: suspend (
        SessionLifecycleRef,
        ConnectionAttemptHandle,
        DeliveryAttemptRef,
    ) -> Unit,
) {
    fun start(
        scope: CoroutineScope,
        ownership: WorkerOwnership,
        onReady: suspend (WorkerOwnership) -> Unit,
        onExit: suspend (WorkerExit) -> Unit,
    ): Run {
        check(ownership.kind == WorkerKind.OUTBOUND_DRAIN) {
            "outbound drain worker requires a drain ownership"
        }
        return Run(drain, ownership, onReady, onExit, scope)
    }

    internal class Run internal constructor(
        private val drain: suspend (
            SessionLifecycleRef,
            ConnectionAttemptHandle,
            DeliveryAttemptRef,
        ) -> Unit,
        val ownership: WorkerOwnership,
        private val onReady: suspend (WorkerOwnership) -> Unit,
        private val onExit: suspend (WorkerExit) -> Unit,
        scope: CoroutineScope,
    ) {
        private val signals = Channel<DrainWakeSignal>(Channel.CONFLATED)
        private val ready = CompletableDeferred<Unit>()
        private val exit = CompletableDeferred<WorkerExit>()
        private val runState = Any()
        private var unavailable = false
        private var binding: AttemptBinding? = null

        @Volatile
        private var stopRequested = false

        private val job: Job = scope.launch(start = CoroutineStart.UNDISPATCHED) { runWorker() }

        fun bind(handle: ConnectionAttemptHandle, attempt: DeliveryAttemptRef): Boolean = synchronized(runState) {
            if (attempt.ownerBareJid != ownership.lifecycle.ownerBareJid || unavailable) return@synchronized false
            binding = AttemptBinding(handle, attempt)
            true
        }

        fun unbind(handle: ConnectionAttemptHandle, attempt: DeliveryAttemptRef) = synchronized(runState) {
            if (binding == AttemptBinding(handle, attempt)) binding = null
        }

        fun boundAttempt(): DeliveryAttemptRef? = synchronized(runState) { binding?.attempt }

        fun signal(handle: ConnectionAttemptHandle, attempt: DeliveryAttemptRef): DrainSignalOutcome =
            synchronized(runState) {
                if (unavailable) return@synchronized DrainSignalOutcome.WorkerUnavailable
                if (binding != AttemptBinding(handle, attempt)) return@synchronized DrainSignalOutcome.Mismatch
                if (signals.trySend(DrainWakeSignal(handle, attempt)).isSuccess) {
                    DrainSignalOutcome.Accepted
                } else {
                    unavailable = true
                    binding = null
                    DrainSignalOutcome.WorkerUnavailable
                }
            }

        fun requestStop() = synchronized(runState) {
            stopRequested = true
            unavailable = true
            binding = null
            signals.close()
        }

        suspend fun awaitExit(timeoutMillis: Long): WorkerAwaitOutcome =
            withTimeoutOrNull(timeoutMillis) { WorkerAwaitOutcome.Exited(exit.await()) }
                ?: WorkerAwaitOutcome.TimedOut

        suspend fun awaitReady() = ready.await()

        private suspend fun runWorker() {
            var reason: WorkerExitReason? = null
            try {
                yield()
                withContext(NonCancellable) { onReady(ownership) }
                ready.complete(Unit)
                for (signal in signals) {
                    drain(ownership.lifecycle, signal.handle, signal.attempt)
                }
            } catch (cancellation: CancellationException) {
                ready.cancel(cancellation)
                WorkerExitExceptionEvidence.record(ownership, cancellation)
                reason = if (stopRequested) {
                    WorkerExitReason.RequestedStop
                } else {
                    WorkerExitReason.OwnerScopeCancelled
                }
            } catch (failure: Throwable) {
                ready.completeExceptionally(failure)
                WorkerExitExceptionEvidence.record(ownership, failure)
                val cause = WorkerFailureKind.DEPENDENCY_FAILURE
                reason = WorkerExitReason.UnexpectedFailure(cause)
                LOGGER.log(Level.SEVERE, "outbound predecessor drain worker stopped", failure)
            } finally {
                synchronized(runState) {
                    unavailable = true
                    binding = null
                    signals.close()
                }
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

        private data class DrainWakeSignal(
            val handle: ConnectionAttemptHandle,
            val attempt: DeliveryAttemptRef,
        )

        private data class AttemptBinding(
            val handle: ConnectionAttemptHandle,
            val attempt: DeliveryAttemptRef,
        )
    }

    private companion object {
        val LOGGER: Logger = Logger.getLogger(OutboundDrainWorker::class.java.name)
    }
}
