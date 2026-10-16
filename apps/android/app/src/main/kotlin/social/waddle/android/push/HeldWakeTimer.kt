package social.waddle.android.push

import android.content.Context
import android.os.PowerManager
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/**
 * [WakeTimer] that keeps the CPU awake from the moment its [alarm] fires
 * until the step it triggered is on the wire. `AlarmManager` holds its
 * wake lock only while the alarm callback runs, and that callback only
 * asks for a re-evaluation: the presence recompute, the session's
 * collector and the FFI send all happen afterwards, so without a lock of
 * its own the phone could fall back asleep before the away or xa
 * presence goes out. The lock is released once [settled] returns, or
 * after [holdMillis] at most.
 */
class HeldWakeTimer(
    private val alarm: WakeTimer,
    private val wakeLocks: WakeLocks,
    private val scope: CoroutineScope,
    /** Suspends until the presence the wake-up produced has been advertised. */
    private val settled: suspend () -> Unit,
    private val holdMillis: Long = HOLD_MILLIS,
) : WakeTimer {
    override fun schedule(atElapsedRealtimeMillis: Long, onFire: () -> Unit) {
        alarm.schedule(atElapsedRealtimeMillis) { fire(onFire) }
    }

    override fun cancel() = alarm.cancel()

    private fun fire(onFire: () -> Unit) {
        val held = wakeLocks.acquire(holdMillis)
        try {
            onFire()
        } catch (failure: RuntimeException) {
            held.release()
            throw failure
        }
        scope.launch {
            try {
                withTimeoutOrNull(holdMillis) { settled() }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Exception) {
                // Nothing left to wait for: the lock goes either way.
            } finally {
                held.release()
            }
        }
    }

    companion object {
        /** Long enough for an FFI presence send; the lock times out regardless. */
        const val HOLD_MILLIS = 10_000L
    }
}

/** Partial wake locks, injected so [HeldWakeTimer] is testable. */
fun interface WakeLocks {
    /** Hold the CPU awake for at most [timeoutMillis]. */
    fun acquire(timeoutMillis: Long): HeldWakeLock
}

/** One held wake lock; releasing it more than once is harmless. */
fun interface HeldWakeLock {
    fun release()
}

/** [WakeLocks] on a `PARTIAL_WAKE_LOCK`, one non-counted lock per hold. */
class PowerManagerWakeLocks(context: Context) : WakeLocks {
    private val power: PowerManager = context.getSystemService(PowerManager::class.java)

    override fun acquire(timeoutMillis: Long): HeldWakeLock {
        val lock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, TAG).apply {
            setReferenceCounted(false)
            acquire(timeoutMillis)
        }
        return HeldWakeLock {
            // A lock that already timed out is no longer held.
            if (lock.isHeld) lock.release()
        }
    }

    private companion object {
        const val TAG = "waddle:auto-away"
    }
}
