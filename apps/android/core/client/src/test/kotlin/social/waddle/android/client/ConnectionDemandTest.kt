package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent

/**
 * With push delivery the stream is only held while something needs it.
 * The server suppresses DM pushes while it sees a session, so dropping
 * demand must close the stream cleanly and keep it closed.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ConnectionDemandTest {
    private class Harness(testScope: TestScope, initiallyWanted: Boolean) {
        val factory = FakeClientFactory()
        val network = FakeNetworkSignal()
        val wanted = MutableStateFlow(initiallyWanted)
        val manager = XmppSessionManager(
            sessionPrefs = SessionPrefs(InMemoryPreferencesDataStore()),
            clientFactory = factory,
            networkSignal = network,
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
            connectionWanted = wanted,
        )
    }

    @Test
    fun `nothing connects while nothing wants the stream`() = runTest {
        val harness = Harness(this, initiallyWanted = false)

        harness.manager.login(testSessionInfo())
        runCurrent()

        assertEquals(ConnectionState.Paused, harness.manager.connectionState.value)
        assertTrue("a background cold start must not connect", harness.factory.clients.isEmpty())

        harness.wanted.value = true
        runCurrent()
        assertEquals(1, harness.factory.clients.size)
        assertEquals(ConnectionState.Connecting, harness.manager.connectionState.value)
        harness.manager.logout()
    }

    @Test
    fun `dropping demand closes the ready stream and parks without backoff`() = runTest {
        val harness = Harness(this, initiallyWanted = true)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertEquals(ConnectionState.Ready, harness.manager.connectionState.value)
        val released = harness.factory.clients.single()

        harness.wanted.value = false
        runCurrent()

        assertEquals("the stream is closed cleanly", 1, released.disconnectCalls)
        assertEquals(ConnectionState.Paused, harness.manager.connectionState.value)
        assertEquals("no reconnect while released", 1, harness.factory.clients.size)

        harness.wanted.value = true
        runCurrent()
        assertEquals("demand returns: reconnect at once", 2, harness.factory.clients.size)
        harness.manager.logout()
    }

    @Test
    fun `dropping demand mid-connect abandons the attempt`() = runTest {
        val harness = Harness(this, initiallyWanted = true)
        harness.manager.login(testSessionInfo())
        runCurrent()
        val connecting = harness.factory.clients.single()
        assertEquals(ConnectionState.Connecting, harness.manager.connectionState.value)

        harness.wanted.value = false
        runCurrent()

        assertEquals(1, connecting.disconnectCalls)
        assertEquals(ConnectionState.Paused, harness.manager.connectionState.value)
        harness.manager.logout()
    }

    @Test
    fun `dropping demand ends a backoff wait`() = runTest {
        val harness = Harness(this, initiallyWanted = true)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        assertTrue(harness.manager.connectionState.value is ConnectionState.Reconnecting)

        harness.wanted.value = false
        runCurrent()

        assertEquals(ConnectionState.Paused, harness.manager.connectionState.value)
        assertEquals(1, harness.factory.clients.size)
        harness.manager.logout()
    }

    @Test
    fun `android never asks the server to hold its session`() = runTest {
        val harness = Harness(this, initiallyWanted = true)
        harness.manager.login(testSessionInfo())
        runCurrent()

        assertFalse(harness.factory.configs.single().resumable)
        harness.manager.logout()
    }
}
