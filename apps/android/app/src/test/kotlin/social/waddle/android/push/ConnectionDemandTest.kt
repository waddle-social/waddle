package social.waddle.android.push

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.DesiredPresence
import social.waddle.android.client.calls.CallState
import social.waddle.client.ffi.WaddleCallMedia
import java.time.Instant

/** What holds the stream open, and that letting go waits out a grace period. */
@OptIn(ExperimentalCoroutinesApi::class)
class ConnectionDemandTest {
    private var clock: Instant = STARTED_AT

    private fun TestScope.demand() =
        ConnectionDemand(backgroundScope, graceMillis = GRACE, now = { clock }).also { runCurrent() }

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

    @Test
    fun `only ringing or live calls need the stream`() {
        val incoming = CallState.Incoming(
            from = "alice@waddle.test/phone",
            sid = "s",
            media = WaddleCallMedia(audio = true, video = false),
        )
        assertTrue(incoming.needsStream())
        val ended = CallState.Ended(sid = "s", reason = null)
        assertFalse("an ended call waits for dismissal forever", ended.needsStream())
        assertFalse(CallState.Idle.needsStream())
    }

    @Test
    fun `the foreground is available`() = runTest {
        val demand = demand()
        demand.setProcessStarted(true)
        runCurrent()
        assertEquals(DesiredPresence.Available, demand.presence.value)
    }

    @Test
    fun `backgrounding with stay-connected goes away idle since leaving`() = runTest {
        val demand = demand()
        demand.setStayConnected(true)
        demand.setProcessStarted(true)
        runCurrent()

        clock = LEFT_AT
        demand.setProcessStarted(false)
        clock = LEFT_AT.plusSeconds(LATER_SECONDS)
        runCurrent()
        assertEquals(DesiredPresence.Away(LEFT_AT), demand.presence.value)

        demand.setProcessStarted(true)
        runCurrent()
        assertEquals(DesiredPresence.Available, demand.presence.value)
    }

    @Test
    fun `a background cold start held for the user is away with no known idle`() = runTest {
        val demand = demand()
        demand.setStayConnected(true)
        runCurrent()
        assertEquals(DesiredPresence.Away(idleSince = null), demand.presence.value)
    }

    @Test
    fun `a lease-only stream advertises nothing`() = runTest {
        val demand = demand()
        demand.acquire()
        runCurrent()
        assertTrue(demand.wanted.value)
        assertEquals(DesiredPresence.None, demand.presence.value)

        // Push mode: leaving the app keeps the stream for the grace only.
        demand.setProcessStarted(true)
        runCurrent()
        demand.setProcessStarted(false)
        runCurrent()
        assertEquals(DesiredPresence.None, demand.presence.value)
    }

    @Test
    fun `a ringing call in the background is away but a live call is present`() = runTest {
        val demand = demand()
        clock = LEFT_AT
        demand.setProcessStarted(true)
        demand.setProcessStarted(false)
        demand.setCallActive(true)
        runCurrent()
        assertEquals("a ring is not presence", DesiredPresence.Away(LEFT_AT), demand.presence.value)

        // ADR-010: a live call is proof of presence and pauses auto-away.
        demand.setInCall(true)
        runCurrent()
        assertEquals(DesiredPresence.Available, demand.presence.value)

        clock = LEFT_AT.plusSeconds(LATER_SECONDS)
        demand.setInCall(false)
        demand.setStayConnected(true)
        demand.setCallActive(false)
        runCurrent()
        assertEquals(
            "idle runs from the end of the call",
            DesiredPresence.Away(LEFT_AT.plusSeconds(LATER_SECONDS)),
            demand.presence.value,
        )
    }

    private companion object {
        const val GRACE = 30_000L
        const val LATER_SECONDS = 600L
        val STARTED_AT: Instant = Instant.parse("2026-10-16T08:00:00Z")
        val LEFT_AT: Instant = Instant.parse("2026-10-16T09:15:30Z")
    }
}
