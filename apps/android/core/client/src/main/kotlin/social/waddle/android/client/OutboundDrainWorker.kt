package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import social.waddle.android.client.prefs.DeliveryAttemptRef
import java.util.logging.Level
import java.util.logging.Logger

internal class OutboundDrainWorker(
    private val drain: suspend (
        SessionLifecycleRef,
        ConnectionAttemptHandle,
        DeliveryAttemptRef,
    ) -> Unit,
    private val stopTimeoutMillis: Long = STOP_TIMEOUT_MILLIS,
) {
    @Volatile
    private var generation: Generation? = null

    fun start(scope: CoroutineScope, lifecycle: SessionLifecycleRef) {
        check(generation == null) { "outbound drain worker already started" }
        val started = Generation(
            lifecycle = lifecycle,
            signals = Channel(Channel.CONFLATED),
        )
        generation = started
        started.job = scope.launch {
            for (signal in started.signals) {
                try {
                    drain(signal.lifecycle, signal.handle, signal.attempt)
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (failure: Throwable) {
                    LOGGER.log(
                        Level.WARNING,
                        "outbound predecessor drain failed; durable work remains queued",
                        failure,
                    )
                }
            }
        }
    }

    fun bind(
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        attempt: DeliveryAttemptRef,
    ): Boolean {
        val current = generation
        if (
            current?.lifecycle != lifecycle ||
            attempt.ownerBareJid != lifecycle.ownerBareJid
        ) {
            return false
        }
        current.binding = AttemptBinding(handle, attempt)
        return true
    }

    fun unbind(
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        attempt: DeliveryAttemptRef,
    ) {
        val current = generation
        if (
            current?.lifecycle == lifecycle &&
            current.binding == AttemptBinding(handle, attempt)
        ) {
            current.binding = null
        }
    }

    fun boundAttempt(lifecycle: SessionLifecycleRef): DeliveryAttemptRef? =
        generation
            ?.takeIf { it.lifecycle == lifecycle }
            ?.binding
            ?.attempt

    fun signal(
        lifecycle: SessionLifecycleRef,
        handle: ConnectionAttemptHandle,
        attempt: DeliveryAttemptRef,
    ) {
        val current = generation
        if (
            current?.lifecycle != lifecycle ||
            current.binding != AttemptBinding(handle, attempt)
        ) {
            return
        }
        current.signals.trySend(DrainWakeSignal(lifecycle, handle, attempt))
    }

    suspend fun stop(lifecycle: SessionLifecycleRef): StopResult {
        val current = generation ?: return StopResult.Stopped
        if (current.lifecycle != lifecycle) return StopResult.Stale
        current.signals.close()
        val job = current.job
        val drained = withTimeoutOrNull(stopTimeoutMillis) {
            job?.join()
            true
        } == true
        if (!drained) {
            job?.cancel()
            val cancelled = withTimeoutOrNull(stopTimeoutMillis) {
                job?.join()
                true
            } == true
            if (!cancelled) return StopResult.FencedWithPending
        }
        if (generation === current) generation = null
        return StopResult.Stopped
    }

    private data class DrainWakeSignal(
        val lifecycle: SessionLifecycleRef,
        val handle: ConnectionAttemptHandle,
        val attempt: DeliveryAttemptRef,
    )

    private data class AttemptBinding(
        val handle: ConnectionAttemptHandle,
        val attempt: DeliveryAttemptRef,
    )

    private class Generation(
        val lifecycle: SessionLifecycleRef,
        val signals: Channel<DrainWakeSignal>,
    ) {
        @Volatile
        var binding: AttemptBinding? = null

        @Volatile
        var job: Job? = null
    }

    private companion object {
        val LOGGER: Logger = Logger.getLogger(OutboundDrainWorker::class.java.name)
        const val STOP_TIMEOUT_MILLIS = 5_000L
    }

    sealed interface StopResult {
        data object Stopped : StopResult
        data object Stale : StopResult
        data object FencedWithPending : StopResult
    }
}
