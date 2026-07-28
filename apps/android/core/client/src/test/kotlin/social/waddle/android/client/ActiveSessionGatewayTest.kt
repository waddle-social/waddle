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
import social.waddle.android.client.calls.LogoutCallTeardown
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
    fun `a readiness event revoked before publication cannot expose a client bridge or ready pipeline`() = runTest {
        val active = readySession()
        val oldAttempt = checkNotNull(active.beginAttempt())
        val oldClient = FakeWaddleClient()
        var readyPipelineStarts = 0

        active.revokeOutboundAuthority()

        assertFalse(
            active.publishReady(oldAttempt, oldClient, "alice@waddle.test/waddle-android-old") {
                readyPipelineStarts += 1
            },
        )
        assertEquals(0, readyPipelineStarts)
        assertEquals(ActiveSession.Invocation.NotConnected, active.invoke { true })
        assertEquals(null, active.bridge)
    }

    @Test
    fun `an old same-account readiness and end attempt cannot clobber the relogin client`() = runTest {
        val active = readySession()
        val oldAttempt = checkNotNull(active.beginAttempt())
        val oldClient = FakeWaddleClient()
        var oldReadyPipelineStarts = 0

        active.revokeOutboundAuthority()
        active.activateOwner("alice@waddle.test")
        val successorAttempt = checkNotNull(active.beginAttempt())
        val successor = FakeWaddleClient()
        assertTrue(active.publishReady(successorAttempt, successor, "alice@waddle.test/waddle-android-new") {})
        val successorBridge = active.bridge

        assertFalse(
            active.publishReady(oldAttempt, oldClient, "alice@waddle.test/waddle-android-old") {
                oldReadyPipelineStarts += 1
            },
        )
        active.endAttempt(oldAttempt, oldClient)

        assertEquals(0, oldReadyPipelineStarts)
        assertEquals(successorBridge, active.bridge)
        assertEquals(
            ActiveSession.Invocation.Completed(true),
            active.invoke { client -> client === successor },
        )
        assertTrue("old readiness cannot redirect ordinary verbs", oldClient.sendCalls.isEmpty())
    }

    @Test
    fun `correction that wins the fence completes before logout and cannot leak to relogin`() = runTest {
        val active = readySession()
        val oldClient = FakeWaddleClient()
        publishReady(active, oldClient)
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
        publishReady(active, successor)
        assertTrue("the completed correction stays on its original client", successor.correctionCalls.isEmpty())
    }

    @Test
    fun `revocation wins direct correction before a same-account relogin`() = runTest {
        val active = readySession()
        val oldClient = FakeWaddleClient()
        publishReady(active, oldClient)
        active.revokeOutboundAuthority()

        val result = active.invoke {
            it.sendCorrection("bob@waddle.test", "m1", "must not send", false, null)
        }
        assertEquals(ActiveSession.Invocation.NotConnected, result)
        assertTrue(oldClient.correctionCalls.isEmpty())

        val successor = FakeWaddleClient()
        active.advanceGeneration()
        active.activateOwner("alice@waddle.test")
        publishReady(active, successor)
        assertTrue(
            "a revoked correction cannot be redirected to the relogin client",
            successor.correctionCalls.isEmpty(),
        )
    }

    @Test
    fun `a profile-read lease cannot use a same-account relogin client`() = runTest {
        val active = readySession()
        val oldClient = FakeWaddleClient()
        oldClient.vcard4 = testVcard4(fullName = "Old profile")
        publishReady(active, oldClient)
        val oldLease = checkNotNull(active.captureOwnerLease())

        active.revokeOutboundAuthority()
        active.advanceGeneration()
        active.activateOwner("alice@waddle.test")
        val successor = FakeWaddleClient()
        successor.vcard4 = testVcard4(fullName = "Successor profile")
        publishReady(active, successor)

        val result = active.invokeIfCurrent(oldLease) {
            it.fetchVcard4(oldLease.ownerBareJid)
        }

        assertEquals(ActiveSession.LeaseInvocation.Stale, result)
        assertTrue(oldClient.fetchVcard4Calls.isEmpty())
        assertTrue(successor.fetchVcard4Calls.isEmpty())
    }

    @Test
    fun `normal call signal is fenced across logout and rejected after revocation`() = runTest {
        val active = readySession()
        val oldClient = FakeWaddleClient()
        publishReady(active, oldClient)
        val signaling = ClientCallSignaling(active)
        val connection = checkNotNull(signaling.captureActiveConnection())
        val releaseCall = CompletableDeferred<Unit>()
        oldClient.callProposeStall = releaseCall

        val call = async {
            connection.propose("bob@waddle.test", "call-1", WaddleCallMedia(audio = true, video = false))
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
            connection.propose("bob@waddle.test", "call-2", WaddleCallMedia(audio = true, video = false)),
        )
        assertEquals(1, oldClient.callVerbs.size)

        val successor = FakeWaddleClient()
        active.advanceGeneration()
        active.activateOwner("alice@waddle.test")
        publishReady(active, successor)
        assertTrue("the revoked call must never redirect onto a relogin client", successor.callVerbs.isEmpty())
    }

    @Test
    fun `parked DM and Muji call connection cannot signal a same-account successor`() = runTest {
        val active = readySession()
        val oldClient = FakeWaddleClient()
        publishReady(active, oldClient)
        val connection = checkNotNull(ClientCallSignaling(active).captureActiveConnection())

        active.revokeOutboundAuthority()
        active.advanceGeneration()
        active.activateOwner("alice@waddle.test")
        val successor = FakeWaddleClient()
        publishReady(active, successor)

        assertFalse(connection.propose("bob@waddle.test", "parked-dm", WaddleCallMedia(audio = true, video = false)))
        assertFalse(
            connection.updateMujiPresence(
            social.waddle.android.client.calls.MujiPresenceUpdate(
                roomJid = "room@muc.waddle.test", nick = "alice", active = false, preparing = true,
                video = false, flags = social.waddle.client.ffi.WaddleInCallPresenceFlags(false, false),
            ),
        )
        )
        assertTrue(oldClient.callVerbs.isEmpty())
        assertTrue(successor.callVerbs.isEmpty())
    }

    @Test
    fun `parked call connection cannot signal a different-account successor`() = runTest {
        val active = readySession()
        val oldClient = FakeWaddleClient()
        publishReady(active, oldClient)
        val connection = checkNotNull(ClientCallSignaling(active).captureActiveConnection())

        active.revokeOutboundAuthority()
        active.advanceGeneration()
        active.activateOwner("carol@waddle.test")
        val successor = FakeWaddleClient()
        publishReady(active, successor)

        assertFalse(connection.proceed("bob@waddle.test/phone", "parked-dm"))
        assertFalse(
            connection.mujiSessionInitiate(
                "room@muc.waddle.test",
                "carol@waddle.test/device",
                "parked-muji",
                false,
            ),
        )
        assertTrue(oldClient.callVerbs.isEmpty())
        assertTrue(successor.callVerbs.isEmpty())
    }

    @Test
    fun `retired logout capability exposes cleanup verbs but no normal signaling verbs`() = runTest {
        val active = readySession()
        val client = FakeWaddleClient()
        publishReady(active, client)

        val retired = checkNotNull(active.revokeOutboundAuthority())
        val teardown: LogoutCallTeardown = ClientCallSignaling.forRetiredConnection(retired)

        assertTrue(teardown.retractForLogout("bob@waddle.test", "logout-call"))
        assertTrue(RecordedCallVerb.Retract("bob@waddle.test", "logout-call") in client.callVerbs)
        assertEquals(
            setOf(
                "retractForLogout",
                "rejectForLogout",
                "cancelAcceptingCallForLogout",
                "terminateCallForLogout",
                "finishTerminatedCallForLogout",
                "leaveMujiForLogout",
                "terminateMujiForLogout",
            ),
            LogoutCallTeardown::class.java.methods
                .filter { it.declaringClass != Any::class.java }
                .map { it.name }
                .toSet(),
        )
    }

    private suspend fun readySession(): ActiveSession {
        val active = ActiveSession()
        active.advanceGeneration()
        active.activateOwner("alice@waddle.test")
        return active
    }

    private suspend fun publishReady(active: ActiveSession, client: FakeWaddleClient) {
        val attempt = checkNotNull(active.beginAttempt())
        assertTrue(active.publishReady(attempt, client, "alice@waddle.test/waddle-android-test") {})
    }
}
