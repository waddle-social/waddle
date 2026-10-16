package social.waddle.android.push

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/** The CPU stays awake from the auto-away alarm until its presence is sent. */
@OptIn(ExperimentalCoroutinesApi::class)
class HeldWakeTimerTest {
    private class FakeAlarm : WakeTimer {
        var onFire: (() -> Unit)? = null

        override fun schedule(atElapsedRealtimeMillis: Long, onFire: () -> Unit) {
            this.onFire = onFire
        }

        override fun cancel() {
            onFire = null
        }

        fun fire() = checkNotNull(onFire) { "no alarm armed" }.invoke()
    }

    private class FakeWakeLocks : WakeLocks {
        val holds = mutableListOf<Long>()
        var held = 0
            private set

        override fun acquire(timeoutMillis: Long): HeldWakeLock {
            holds += timeoutMillis
            held += 1
            var released = false
            return HeldWakeLock {
                if (!released) {
                    released = true
                    held -= 1
                }
            }
        }
    }

    private val alarm = FakeAlarm()
    private val locks = FakeWakeLocks()

    private fun TestScope.timer(settled: suspend () -> Unit) =
        HeldWakeTimer(alarm, locks, backgroundScope, settled, holdMillis = HOLD)

    @Test
    fun `the lock is held from the alarm until the presence is advertised`() = runTest {
        val advertised = CompletableDeferred<Unit>()
        var heldDuringFire = 0
        timer { advertised.await() }.schedule(atElapsedRealtimeMillis = 1L) { heldDuringFire = locks.held }

        alarm.fire()
        runCurrent()
        assertEquals("taken before the re-evaluation runs", 1, heldDuringFire)
        assertEquals(listOf(HOLD), locks.holds)
        assertEquals("still awake while the send is pending", 1, locks.held)

        advertised.complete(Unit)
        runCurrent()
        assertEquals("released once it went out", 0, locks.held)
    }

    @Test
    fun `the lock is released at the timeout when the send never settles`() = runTest {
        timer { CompletableDeferred<Unit>().await() }.schedule(1L) {}

        alarm.fire()
        advanceTimeBy(HOLD - 1)
        runCurrent()
        assertEquals(1, locks.held)
        advanceTimeBy(1)
        runCurrent()
        assertEquals(0, locks.held)
    }

    @Test
    fun `the lock is released when waiting for the send fails`() = runTest {
        timer { throw IOException("stream gone") }.schedule(1L) {}

        alarm.fire()
        runCurrent()

        assertEquals(0, locks.held)
    }

    @Test
    fun `the lock is released when the re-evaluation itself throws`() = runTest {
        timer {}.schedule(1L) { error("boom") }

        val thrown = runCatching { alarm.fire() }

        assertTrue(thrown.isFailure)
        assertEquals(0, locks.held)
    }

    @Test
    fun `cancel disarms the alarm`() = runTest {
        val timer = timer {}
        timer.schedule(1L) {}
        timer.cancel()

        assertEquals(null, alarm.onFire)
    }

    private companion object {
        const val HOLD = 10_000L
    }
}
