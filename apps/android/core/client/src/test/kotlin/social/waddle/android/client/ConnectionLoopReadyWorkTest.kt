package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent

/**
 * Ready work (presence, queued sends, topology, joins, catch-up) belongs
 * to its attempt: a stream that drops while that work is in flight ends
 * the attempt at once, cancels the work, and the next stream starts the
 * pipeline again from the top.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ConnectionLoopReadyWorkTest {
    @Test
    fun `a drop mid-pipeline ends the attempt at once and the next stream starts over`() = runTest {
        val factory = FakeClientFactory()
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        prefs.setJoinedRooms(setOf(ROOM))
        val manager = XmppSessionManager(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        // Only the first stream's topology discovery stalls, parking its
        // pipeline after the presence and before the room join.
        factory.onCreate = { client -> if (factory.clients.isEmpty()) client.topology.delayMillis = STALL_MILLIS }

        manager.login(testSessionInfo())
        runCurrent()
        factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        val first = factory.clients.single()
        assertEquals(ConnectionState.Ready, manager.connectionState.value)
        assertEquals(1, first.presence.calls.size)
        assertTrue("the pipeline is parked in discovery", first.joinRoomCalls.isEmpty())

        factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        // No virtual time has passed: the attempt did not wait for the
        // parked pipeline before leaving Ready.
        assertEquals(
            ConnectionState.Reconnecting(attempt = 1, nextDelayMs = RECONNECT_DELAY_MILLIS),
            manager.connectionState.value,
        )
        assertEquals("the dropped stream was closed", 1, first.disconnectCalls)

        advanceTimeBy(RECONNECT_DELAY_MILLIS)
        runCurrent()
        factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        val second = factory.clients.last()
        assertEquals(2, factory.clients.size)
        assertEquals(ConnectionState.Ready, manager.connectionState.value)
        assertEquals(
            "the new stream advertises first",
            listOf(RecordedPresence(status = null, show = null, idleSince = null, joinsBefore = 0, sendsBefore = 0)),
            second.presence.calls.toList(),
        )
        assertEquals(1, second.topology.calls)
        assertEquals(listOf(ROOM to "icepuma"), second.joinRoomCalls.toList())

        // Long after the stall would have ended, the old pipeline never
        // resumed: it was cancelled with its attempt.
        advanceTimeBy(STALL_MILLIS * 2)
        runCurrent()
        assertEquals(0, first.topology.calls)
        assertTrue(first.joinRoomCalls.isEmpty())
        assertEquals(1, first.presence.calls.size)
        manager.logout()
    }

    private companion object {
        const val ROOM = "general@muc.waddle.test"
        const val STALL_MILLIS = 60_000L

        /** First backoff step with `PinnedRandom(0.5)`. */
        const val RECONNECT_DELAY_MILLIS = 1_000L
    }
}
