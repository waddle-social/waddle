package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.calls.ClientCallSignaling
import social.waddle.android.client.session.ActiveSession
import social.waddle.client.ffi.WaddleCallMedia
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * The gateway is the sole linearization point for ordinary client use. These
 * tests deliberately cover a direct message correction and normal call
 * signaling, not only durable outgoing sends.
 */
class ActiveSessionGatewayTest {
    @Test
    fun `correction that wins the fence completes before logout and cannot leak to relogin`() = runTest {
        val active = readySession()
        val oldClient = FakeWaddleClient()
        active.onReady(oldClient)
        val releaseCorrection = CompletableDeferred<Unit>()
        oldClient.correctionStall = releaseCorrection

        val correction = async {
            active.invoke {
                it.sendCorrection("bob@waddle.test", "m1", "fixed", false, null)
            }
        }
        runCurrent()
        assertEquals(listOf(Triple("bob@waddle.test", "m1", "fixed")), oldClient.correctionCalls)

        val logout = async { active.revokeOutboundAuthority() }
        runCurrent()
        assertFalse("logout must wait for the winning correction", logout.isCompleted)
        releaseCorrection.complete(Unit)
        assertEquals(
            ActiveSession.Invocation.Completed(WaddleSendMessageOutcome.Sent("corr-1")),
            correction.await(),
        )
        logout.await()

        val successor = FakeWaddleClient()
        active.advanceGeneration()
        active.activateOwner("alice@waddle.test")
        active.onReady(successor)
        assertTrue("the completed correction stays on its original client", successor.correctionCalls.isEmpty())
    }

    @Test
    fun `revocation wins direct correction before a same-account relogin`() = runTest {
        val active = readySession()
        val oldClient = FakeWaddleClient()
        active.onReady(oldClient)
        active.revokeOutboundAuthority()

        val result = active.invoke {
            it.sendCorrection("bob@waddle.test", "m1", "must not send", false, null)
        }
        assertEquals(ActiveSession.Invocation.NotConnected, result)
        assertTrue(oldClient.correctionCalls.isEmpty())

        val successor = FakeWaddleClient()
        active.advanceGeneration()
        active.activateOwner("alice@waddle.test")
        active.onReady(successor)
        assertTrue(
            "a revoked correction cannot be redirected to the relogin client",
            successor.correctionCalls.isEmpty(),
        )
    }

    @Test
    fun `normal call signal is fenced across logout and rejected after revocation`() = runTest {
        val active = readySession()
        val oldClient = FakeWaddleClient()
        active.onReady(oldClient)
        val signaling = ClientCallSignaling(active)
        val releaseCall = CompletableDeferred<Unit>()
        oldClient.callProposeStall = releaseCall

        val call = async {
            signaling.propose("bob@waddle.test", "call-1", WaddleCallMedia(audio = true, video = false))
        }
        runCurrent()
        assertTrue(oldClient.callVerbs.isNotEmpty())
        val logout = async { active.revokeOutboundAuthority() }
        runCurrent()
        assertFalse("logout must wait for a winning normal call signal", logout.isCompleted)
        releaseCall.complete(Unit)
        assertTrue(call.await())
        logout.await()

        assertFalse(
            "a call started after revocation must not find a retired client",
            signaling.propose("bob@waddle.test", "call-2", WaddleCallMedia(audio = true, video = false)),
        )
        assertEquals(1, oldClient.callVerbs.size)

        val successor = FakeWaddleClient()
        active.advanceGeneration()
        active.activateOwner("alice@waddle.test")
        active.onReady(successor)
        assertTrue("the revoked call must never redirect onto a relogin client", successor.callVerbs.isEmpty())
    }

    private suspend fun readySession(): ActiveSession {
        val active = ActiveSession()
        active.advanceGeneration()
        active.activateOwner("alice@waddle.test")
        return active
    }
}
