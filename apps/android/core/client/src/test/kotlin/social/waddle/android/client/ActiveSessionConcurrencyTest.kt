package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.session.ActiveSession

/**
 * Ordinary operations share the transport; only account revocation is
 * exclusive. A slow history query must not hold up a send, and logout
 * must still never return while an operation is using the old client.
 */
class ActiveSessionConcurrencyTest {
    @Test
    fun `a stalled operation does not block another`() = runTest {
        val active = readySession()
        val stall = CompletableDeferred<Unit>()

        val slow = async { active.invoke { stall.await() } }
        runCurrent()
        val fast = async { active.invoke { "sent" } }
        runCurrent()

        assertTrue("fast op completes while the slow one is in flight", fast.isCompleted)
        assertEquals(ActiveSession.Invocation.Completed("sent"), fast.await())
        assertFalse(slow.isCompleted)
        stall.complete(Unit)
        slow.await()
    }

    @Test
    fun `revocation waits for in-flight operations and blocks new ones`() = runTest {
        val active = readySession()
        val stall = CompletableDeferred<Unit>()
        val inFlight = async { active.invoke { stall.await() } }
        runCurrent()

        val logout = async { active.revokeOutboundAuthority() }
        runCurrent()
        assertFalse("revocation waits for the in-flight op", logout.isCompleted)

        val late = async { active.invoke { "late" } }
        runCurrent()
        assertFalse("ops queued behind a pending revocation do not start", late.isCompleted)

        stall.complete(Unit)
        runCurrent()
        assertTrue(logout.isCompleted)
        assertEquals("the queued op sees the revoked session", ActiveSession.Invocation.NotConnected, late.await())
        inFlight.await()
    }

    @Test
    fun `ending a dead attempt does not wait for calls timing out on it`() = runTest {
        val active = readySession()
        val attempt = checkNotNull(active.beginAttempt())
        val client = FakeWaddleClient()
        assertTrue(active.publishReady(attempt, client, "alice@waddle.test/waddle-android-test") {})
        val stall = CompletableDeferred<Unit>()
        val hanging = async { active.invoke { stall.await() } }
        runCurrent()

        val ended = async { active.endAttempt(attempt, client) }
        runCurrent()

        assertTrue("attempt teardown is not fenced behind ordinary ops", ended.isCompleted)
        assertEquals(ActiveSession.Invocation.NotConnected, active.invoke { true })
        stall.complete(Unit)
        hanging.await()
    }

    private suspend fun readySession(): ActiveSession {
        val active = ActiveSession()
        active.advanceGeneration()
        active.activateOwner("alice@waddle.test")
        val attempt = checkNotNull(active.beginAttempt())
        assertTrue(active.publishReady(attempt, FakeWaddleClient(), "alice@waddle.test/waddle-android-test") {})
        return active
    }
}
