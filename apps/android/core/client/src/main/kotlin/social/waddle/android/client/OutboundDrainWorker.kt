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
        onExit: suspend (WorkerExit) -> Unit,
    ): Run {
        check(ownership.kind == WorkerKind.OUTBOUND_DRAIN) {
            "outbound drain worker requires a drain ownership"
        }
        return Run(drain, ownership, onExit, scope)
    }

    internal class Run internal constructor(
        private val drain: suspend (
            SessionLifecycleRef,
            ConnectionAttemptHandle,
            DeliveryAttemptRef,
        ) -> Unit,
        val ownership: WorkerOwnership,
        private val onExit: suspend (WorkerExit) -> Unit,
        scope: CoroutineScope,
    ) {
        private val signals = Channel<DrainWakeSignal>(Channel.CONFLATED)
        private val exit = CompletableDeferred<WorkerExit>()

        @Volatile
        private var binding: AttemptBinding? = null

        @Volatile
        private var stopRequested = false

        private val job: Job = scope.launch(start = CoroutineStart.UNDISPATCHED) { runWorker() }

        fun bind(handle: ConnectionAttemptHandle, attempt: DeliveryAttemptRef): Boolean {
            if (attempt.ownerBareJid != ownership.lifecycle.ownerBareJid || stopRequested) return false
            binding = AttemptBinding(handle, attempt)
            return true
        }

        fun unbind(handle: ConnectionAttemptHandle, attempt: DeliveryAttemptRef) {
            if (binding == AttemptBinding(handle, attempt)) binding = null
        }

        fun boundAttempt(): DeliveryAttemptRef? = binding?.attempt

        fun signal(handle: ConnectionAttemptHandle, attempt: DeliveryAttemptRef) {
            if (binding != AttemptBinding(handle, attempt) || stopRequested) return
            signals.trySend(DrainWakeSignal(handle, attempt))
        }

        fun requestStop() {
            stopRequested = true
            signals.close()
        }

        suspend fun awaitExit(timeoutMillis: Long): WorkerAwaitOutcome =
            withTimeoutOrNull(timeoutMillis) { WorkerAwaitOutcome.Exited(exit.await()) }
                ?: WorkerAwaitOutcome.TimedOut

        private suspend fun runWorker() {
            var reason: WorkerExitReason? = null
            try {
                yield()
                for (signal in signals) {
                    drain(ownership.lifecycle, signal.handle, signal.attempt)
                }
            } catch (cancellation: CancellationException) {
                reason = if (stopRequested) {
                    WorkerExitReason.RequestedStop
                } else {
                    WorkerExitReason.OwnerScopeCancelled
                }
            } catch (failure: Throwable) {
                val cause = WorkerFailureCause.from(failure)
                reason = WorkerExitReason.UnexpectedFailure(cause)
                LOGGER.log(Level.SEVERE, "outbound predecessor drain worker stopped", failure)
            } finally {
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
