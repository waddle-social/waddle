package social.waddle.android.client

import app.cash.turbine.test
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.toSnapshot
import social.waddle.client.ffi.WaddleClientEvent

@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerTest {
    private class Harness(testScope: TestScope) {
        val factory = FakeClientFactory()
        val network = FakeNetworkSignal()
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val manager = XmppSessionManager(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = network,
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )
    }

    @Test
    fun `login goes connecting then ready on session ready`() = runTest {
        val harness = Harness(this)

        harness.manager.connectionState.test {
            assertEquals(ConnectionState.Idle, awaitItem())

            harness.manager.login(testSessionInfo())
            runCurrent()
            assertEquals(ConnectionState.Connecting, awaitItem())

            harness.factory.emit(WaddleClientEvent.Connected)
            runCurrent()
            assertEquals(ConnectionState.Ready, awaitItem())
        }

        assertEquals(WaddleAppState.Ready, harness.manager.appState.value)
        assertEquals("sess-1", harness.prefs.sessionId.first())
        assertEquals(1, harness.factory.clients.size)

        val config = harness.factory.configs.single()
        assertEquals("wss://waddle.test/xmpp", config.serverUrl)
        assertEquals("icepuma@waddle.test", config.jid)
        assertEquals("sess-1", config.accessToken)
        assertTrue(
            "resource ${config.resource} must be waddle-android-<8hex>",
            config.resource.matches(Regex("waddle-android-[0-9a-f]{8}")),
        )

        harness.manager.logout()
    }

    @Test
    fun `session drop retries with the backoff policy`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertEquals(ConnectionState.Ready, harness.manager.connectionState.value)

        harness.factory.emit(WaddleClientEvent.Error("websocket stream closed"))
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()

        // Attempt counter reset on ready → first retry uses attempt 0.
        assertEquals(
            ConnectionState.Reconnecting(attempt = 1, nextDelayMs = 1_000L),
            harness.manager.connectionState.value,
        )
        assertEquals(1, harness.factory.clients.size)

        advanceTimeBy(1_000L)
        runCurrent()
        assertEquals(ConnectionState.Connecting, harness.manager.connectionState.value)
        assertEquals("a fresh client per attempt", 2, harness.factory.clients.size)
        assertEquals(1, harness.factory.clients[0].disconnectCalls)

        harness.manager.logout()
    }

    @Test
    fun `attempt budget exhaustion is terminal`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        repeat(ReconnectPolicy.MAX_ATTEMPTS) { attempt ->
            harness.factory.emit(WaddleClientEvent.Disconnected)
            runCurrent()
            val state = harness.manager.connectionState.value
            assertTrue("expected Reconnecting after failure $attempt, got $state", state is ConnectionState.Reconnecting)
            assertEquals(attempt + 1, (state as ConnectionState.Reconnecting).attempt)
            advanceTimeBy(state.nextDelayMs)
            runCurrent()
        }

        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        assertEquals(ConnectionState.Failed, harness.manager.connectionState.value)
        assertEquals("app stays signed in on transport failure", WaddleAppState.Ready, harness.manager.appState.value)

        harness.manager.logout()
    }

    @Test
    fun `auth shaped error is terminal and signs out`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.factory.emit(WaddleClientEvent.Error("SASL failure: not-authorized"))
        runCurrent()

        assertEquals(ConnectionState.AuthFailed, harness.manager.connectionState.value)
        assertEquals(WaddleAppState.SignedOut, harness.manager.appState.value)
        assertEquals("session id cleared on auth failure", null, harness.prefs.sessionId.first())
        assertEquals("no retry after auth failure", 1, harness.factory.clients.size)
    }

    @Test
    fun `offline network parks the loop until connectivity returns`() = runTest {
        val harness = Harness(this)
        harness.network.state.value = false

        harness.manager.login(testSessionInfo())
        runCurrent()

        assertEquals(ConnectionState.Offline, harness.manager.connectionState.value)
        assertEquals("no attempt while offline", 0, harness.factory.clients.size)

        harness.network.state.value = true
        runCurrent()
        assertEquals(ConnectionState.Connecting, harness.manager.connectionState.value)
        assertEquals(1, harness.factory.clients.size)

        harness.manager.logout()
    }

    @Test
    fun `resume snapshots persist and feed the next attempt`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val state = testResumeState()
        harness.factory.emit(WaddleClientEvent.ResumeStateChanged(state))
        runCurrent()
        assertEquals(state.toSnapshot(), harness.prefs.smResume.first())

        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(1_000L)
        runCurrent()

        val resumed = harness.factory.configs.last().resumeState
        assertNotNull("second attempt must carry the persisted snapshot", resumed)
        assertEquals("prev-1", resumed?.previd)

        harness.factory.emit(WaddleClientEvent.ResumeStateChanged(null))
        runCurrent()
        assertEquals(null, harness.prefs.smResume.first())

        harness.manager.logout()
    }

    @Test
    fun `connect timeout burns the attempt`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        assertEquals(ConnectionState.Connecting, harness.manager.connectionState.value)

        advanceTimeBy(XmppSessionManager.CONNECT_TIMEOUT_MILLIS)
        runCurrent()

        assertEquals(
            ConnectionState.Reconnecting(attempt = 1, nextDelayMs = 1_000L),
            harness.manager.connectionState.value,
        )

        harness.manager.logout()
    }

    @Test
    fun `live events fan out to stores and the shared flow`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.manager.events.test {
            harness.factory.emit(WaddleClientEvent.Connected)
            runCurrent()
            assertEquals(XmppEvent.SessionReady, awaitItem())

            val message = testMessage(stanzaId = "s1", from = "alice@waddle.test", to = "icepuma@waddle.test")
            harness.factory.emit(WaddleClientEvent.Message(message))
            runCurrent()
            assertEquals(XmppEvent.Message(message), awaitItem())
        }

        assertEquals(1, harness.manager.timelineStore.timeline("alice@waddle.test").value.size)
        assertEquals(listOf("alice@waddle.test"), harness.manager.dmStore.peers.value)
        assertEquals(mapOf("alice@waddle.test" to 1), harness.manager.unreadStore.counts.value)

        harness.manager.logout()
    }

    @Test
    fun `logout clears state and stores`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        harness.factory.emit(WaddleClientEvent.Message(testMessage(stanzaId = "s1")))
        runCurrent()

        harness.manager.logout()

        assertEquals(ConnectionState.Idle, harness.manager.connectionState.value)
        assertEquals(WaddleAppState.SignedOut, harness.manager.appState.value)
        assertEquals(null, harness.prefs.sessionId.first())
        assertTrue(harness.manager.timelineStore.timeline("alice@waddle.test").value.isEmpty())
        assertEquals(1, harness.factory.clients.single().disconnectCalls)
    }
}
