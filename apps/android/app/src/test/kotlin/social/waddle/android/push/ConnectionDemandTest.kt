package social.waddle.android.push

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** What holds the stream open, and that letting go waits out a grace period. */
@OptIn(ExperimentalCoroutinesApi::class)
class ConnectionDemandTest {
    private fun TestScope.demand() = ConnectionDemand(backgroundScope, graceMillis = GRACE).also { runCurrent() }

    @Test
    fun `a background cold start wants nothing`() = runTest {
        val demand = demand()
        advanceTimeBy(GRACE * 2)
        assertFalse(demand.wanted.value)
    }

    @Test
    fun `foreground wants the stream at once and lets go after the grace`() = runTest {
        val demand = demand()

        demand.setProcessStarted(true)
        runCurrent()
        assertTrue(demand.wanted.value)

        demand.setProcessStarted(false)
        advanceTimeBy(GRACE - 1)
        runCurrent()
        assertTrue("a quick app switch keeps the stream", demand.wanted.value)
        advanceTimeBy(2)
        runCurrent()
        assertFalse(demand.wanted.value)
    }

    @Test
    fun `returning within the grace keeps the stream without a gap`() = runTest {
        val demand = demand()
        demand.setProcessStarted(true)
        runCurrent()
        demand.setProcessStarted(false)
        advanceTimeBy(GRACE / 2)
        demand.setProcessStarted(true)
        advanceTimeBy(GRACE * 2)
        runCurrent()
        assertTrue(demand.wanted.value)
    }

    @Test
    fun `stay-connected and calls hold the stream in the background`() = runTest {
        val demand = demand()
        demand.setStayConnected(true)
        runCurrent()
        assertTrue(demand.wanted.value)

        demand.setStayConnected(false)
        demand.setCallActive(true)
        advanceTimeBy(GRACE * 2)
        runCurrent()
        assertTrue("a ringing or live call needs signalling", demand.wanted.value)
    }

    @Test
    fun `a lease holds the stream until released once`() = runTest {
        val demand = demand()
        val first = demand.acquire()
        val second = demand.acquire()
        runCurrent()
        assertTrue(demand.wanted.value)

        first.release()
        first.release()
        advanceTimeBy(GRACE * 2)
        runCurrent()
        assertTrue("a double release must not drop another holder", demand.wanted.value)

        second.release()
        advanceTimeBy(GRACE * 2)
        runCurrent()
        assertFalse(demand.wanted.value)
    }

    private companion object {
        const val GRACE = 30_000L
    }
}
