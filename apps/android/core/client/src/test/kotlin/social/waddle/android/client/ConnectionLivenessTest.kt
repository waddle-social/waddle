package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.session.ConnectionLoop
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * A ready stream must not be trusted on faith: half-open sockets after
 * Doze or a network handoff look exactly like a quiet, healthy session.
 * These drive the real [ConnectionLoop] through [XmppSessionManager]
 * with a fake transport whose XEP-0199 ping can succeed, fail or hang.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ConnectionLivenessTest {
    private class Harness(testScope: TestScope) {
        val factory = FakeClientFactory()
        val network = FakeNetworkSignal()
        val ticks = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = factory,
            networkSignal = network,
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
            livenessTicks = ticks,
        )

        val client: FakeWaddleClient get() = factory.clients.last()
    }

    private suspend fun TestScope.readyHarness(): Harness {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertEquals(ConnectionState.Ready, harness.manager.connectionState.value)
        return harness
    }

    @Test
    fun `network handoff probes the stream and keeps a live one`() = runTest {
        val harness = readyHarness()

        harness.network.handoff()
        runCurrent()

        assertEquals(1, harness.client.pingCalls)
        assertEquals(ConnectionState.Ready, harness.manager.connectionState.value)
        assertEquals("a live stream is kept", 1, harness.factory.clients.size)
        harness.manager.logout()
    }

    @Test
    fun `failed probe after a handoff reconnects immediately`() = runTest {
        val harness = readyHarness()
        val stale = harness.client
        stale.ping = { error("socket is half-open") }

        harness.network.handoff()
        runCurrent()

        assertEquals("no backoff wait for a stale stream", 2, harness.factory.clients.size)
        assertEquals(ConnectionState.Connecting, harness.manager.connectionState.value)
        assertEquals("the stale client is closed", 1, stale.disconnectCalls)
        harness.manager.logout()
    }

    @Test
    fun `unanswered probe is treated as a dead stream`() = runTest {
        val harness = readyHarness()
        val never = CompletableDeferred<Unit>()
        harness.client.ping = { never.await() }

        harness.manager.onAppForeground()
        runCurrent()
        assertEquals(1, harness.factory.clients.size)

        advanceTimeBy(ConnectionLoop.PING_TIMEOUT_MILLIS + 1)
        runCurrent()
        assertEquals(2, harness.factory.clients.size)
        harness.manager.logout()
    }

    @Test
    fun `foreground probes a ready stream`() = runTest {
        val harness = readyHarness()

        harness.manager.onAppForeground()
        runCurrent()

        assertEquals(1, harness.client.pingCalls)
        assertEquals(ConnectionState.Ready, harness.manager.connectionState.value)
        harness.manager.logout()
    }

    @Test
    fun `idle ready stream is probed on every cadence tick`() = runTest {
        val harness = readyHarness()
        assertEquals(0, harness.client.pingCalls)

        harness.ticks.emit(Unit)
        runCurrent()
        assertEquals(1, harness.client.pingCalls)

        harness.ticks.emit(Unit)
        runCurrent()
        assertEquals(2, harness.client.pingCalls)
        assertEquals(1, harness.factory.clients.size)
        harness.manager.logout()
    }

    @Test
    fun `losing the network while ready parks offline without probing`() = runTest {
        val harness = readyHarness()

        harness.network.state.value = false
        runCurrent()
        advanceTimeBy(5_000)
        runCurrent()

        assertEquals(ConnectionState.Offline, harness.manager.connectionState.value)
        assertEquals(0, harness.client.pingCalls)

        harness.network.state.value = true
        runCurrent()
        assertEquals(2, harness.factory.clients.size)
        harness.manager.logout()
    }

    @Test
    fun `transport error on send triggers a probe`() = runTest {
        val harness = readyHarness()
        harness.client.sendOutcome = WaddleSendMessageOutcome.TransportError

        harness.manager.sendChatMessage("peer@waddle.test", "hello")
        runCurrent()

        assertTrue("a refused send probes the stream", harness.client.pingCalls >= 1)
        harness.manager.logout()
    }

    @Test
    fun `handoff during backoff retries at once`() = runTest {
        val harness = readyHarness()
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        assertTrue(harness.manager.connectionState.value is ConnectionState.Reconnecting)

        harness.network.handoff()
        runCurrent()

        assertEquals(2, harness.factory.clients.size)
        harness.manager.logout()
    }
}
