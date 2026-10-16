package social.waddle.android.client

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.client.ffi.WaddleClientEvent
import java.time.Instant

/**
 * Own presence (RFC 6121 §4) across stream churn: every ready stream is
 * fresh on Android, so it advertises first, before queued sends and room
 * joins, through one serial collector bound to that attempt's lease.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerPresenceTest {
    private class Harness(testScope: TestScope, initial: DesiredPresence = DesiredPresence.Available) {
        val factory = FakeClientFactory()
        val prefs = SessionPrefs(InMemoryPreferencesDataStore())
        val desired = MutableStateFlow(initial)
        val wanted = MutableStateFlow(true)
        val manager = XmppSessionManager(
            sessionPrefs = prefs,
            clientFactory = factory,
            networkSignal = FakeNetworkSignal(),
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
            connectionWanted = wanted,
            desiredPresence = desired,
        )

        val client: FakeWaddleClient get() = factory.clients.last()

        /** What the current client was asked to advertise, as (show, idleSince). */
        fun advertised(): List<Pair<String?, String?>> = client.presence.calls.map { it.show to it.idleSince }
    }

    private suspend fun TestScope.ready(harness: Harness) {
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
    }

    @Test
    fun `presence leads the ready pipeline once per ready stream`() = runTest {
        val harness = Harness(this)
        harness.prefs.setJoinedRooms(setOf(ROOM))
        harness.manager.login(testSessionInfo())
        runCurrent()
        // Queued while not ready: the drain must follow the presence.
        harness.manager.sendChatMessage(PEER, "queued before ready")
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val first = harness.client
        assertEquals(
            listOf(RecordedPresence(status = null, show = null, idleSince = null, joinsBefore = 0, sendsBefore = 0)),
            first.presence.calls.toList(),
        )
        assertEquals("joins follow the presence", listOf(ROOM to "icepuma"), first.joinRoomCalls.toList())
        assertEquals(
            "the queued send drained after it",
            listOf(PEER to "queued before ready"),
            first.sendCalls.toList(),
        )

        // A dropped stream reconnects as a brand new one: advertise again,
        // exactly once, on the new client.
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        assertEquals(
            ConnectionState.Reconnecting(attempt = 1, nextDelayMs = RECONNECT_DELAY_MILLIS),
            harness.manager.connectionState.value,
        )
        advanceTimeBy(RECONNECT_DELAY_MILLIS)
        runCurrent()
        assertEquals(ConnectionState.Connecting, harness.manager.connectionState.value)
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertEquals(2, harness.factory.clients.size)
        assertEquals(1, first.presence.calls.size)
        assertEquals(listOf<Pair<String?, String?>>(null to null), harness.advertised())
        assertEquals(0, harness.client.presence.calls.single().joinsBefore)
        harness.manager.logout()
    }

    @Test
    fun `joins wait for the first presence to reach the client`() = runTest {
        val harness = Harness(this)
        harness.prefs.setJoinedRooms(setOf(ROOM))
        val gate = CompletableDeferred<Unit>()
        harness.factory.onCreate = { it.presence.stall = gate }

        ready(harness)
        assertEquals(1, harness.client.presence.calls.size)
        assertTrue("no join overtakes the presence", harness.client.joinRoomCalls.isEmpty())

        gate.complete(Unit)
        runCurrent()
        assertEquals(listOf(ROOM to "icepuma"), harness.client.joinRoomCalls.toList())
        harness.manager.logout()
    }

    @Test
    fun `waiting for an automatic step returns once its presence is sent`() = runTest {
        val harness = Harness(this)
        val notReady = async { harness.manager.awaitPresenceAdvertised(DesiredPresence.Away(LEFT_AT)) }
        runCurrent()
        assertTrue("no ready stream: nothing to wait for", notReady.isCompleted)

        ready(harness)
        val gate = CompletableDeferred<Unit>()
        harness.client.presence.stall = gate
        harness.desired.value = DesiredPresence.Away(LEFT_AT)
        val sent = async { harness.manager.awaitPresenceAdvertised(DesiredPresence.Away(LEFT_AT)) }
        runCurrent()
        assertFalse("the send is still in flight", sent.isCompleted)

        gate.complete(Unit)
        runCurrent()
        assertTrue(sent.isCompleted)
        harness.manager.logout()
    }

    @Test
    fun `the latest desired presence wins over changes made during a send`() = runTest {
        val harness = Harness(this)
        val gate = CompletableDeferred<Unit>()
        harness.factory.onCreate = { it.presence.stall = gate }
        ready(harness)

        harness.desired.value = DesiredPresence.Away(LEFT_AT)
        runCurrent()
        harness.desired.value = DesiredPresence.Away(LEFT_AGAIN_AT)
        runCurrent()
        assertEquals("one send in flight at a time", 1, harness.client.presence.calls.size)

        harness.client.presence.stall = null
        gate.complete(Unit)
        runCurrent()

        assertEquals(
            listOf(null to null, "away" to "2026-10-16T09:20:00Z"),
            harness.advertised(),
        )
        harness.manager.logout()
    }

    @Test
    fun `auto-away sends away then xa with the same idle, and returning sends available`() = runTest {
        val harness = Harness(this)
        ready(harness)

        harness.desired.value = DesiredPresence.Away(LEFT_AT)
        runCurrent()
        harness.desired.value = DesiredPresence.ExtendedAway(LEFT_AT)
        runCurrent()
        harness.desired.value = DesiredPresence.Available
        runCurrent()

        assertEquals(
            listOf(
                null to null,
                // XEP-0319: a XEP-0082 UTC DateTime, whole seconds.
                "away" to "2026-10-16T09:15:30Z",
                // RFC 6121 §4.7.2.1 extended away, idle still since leaving.
                "xa" to "2026-10-16T09:15:30Z",
                // XEP-0319 §2.2: back from idle omits <idle/>.
                null to null,
            ),
            harness.advertised(),
        )
        assertEquals(listOf(null, null, null, null), harness.client.presence.calls.map { it.status })
        harness.manager.logout()
    }

    @Test
    fun `away without a known departure carries no idle`() = runTest {
        val harness = Harness(this, initial = DesiredPresence.Away(idleSince = null))
        ready(harness)

        assertEquals(listOf<Pair<String?, String?>>("away" to null), harness.advertised())
        harness.manager.logout()
    }

    @Test
    fun `a lease-only stream advertises nothing but still runs the pipeline`() = runTest {
        val harness = Harness(this, initial = DesiredPresence.None)
        harness.prefs.setJoinedRooms(setOf(ROOM))
        ready(harness)

        assertTrue(harness.client.presence.calls.isEmpty())
        assertEquals(listOf(ROOM to "icepuma"), harness.client.joinRoomCalls.toList())

        // The user opens the app while the lease still holds the stream.
        harness.desired.value = DesiredPresence.Available
        runCurrent()
        assertEquals(listOf<Pair<String?, String?>>(null to null), harness.advertised())
        harness.manager.logout()
    }

    @Test
    fun `dropping to lease-only keeps the last presence instead of sending`() = runTest {
        val harness = Harness(this)
        ready(harness)

        harness.desired.value = DesiredPresence.None
        runCurrent()
        harness.desired.value = DesiredPresence.Available
        runCurrent()

        assertEquals("nothing new reaches the wire", listOf<Pair<String?, String?>>(null to null), harness.advertised())
        harness.manager.logout()
    }

    @Test
    fun `a released stream sends nothing and the next stream advertises the latest`() = runTest {
        val harness = Harness(this)
        ready(harness)
        val released = harness.client

        harness.wanted.value = false
        runCurrent()
        harness.desired.value = DesiredPresence.Away(LEFT_AT)
        runCurrent()
        assertEquals(1, released.presence.calls.size)

        harness.wanted.value = true
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertEquals(1, released.presence.calls.size)
        assertEquals(listOf("away" to "2026-10-16T09:15:30Z"), harness.advertised())
        harness.manager.logout()
    }

    @Test
    fun `a change parked behind an in-flight send is dropped once the lease is stale`() = runTest {
        val harness = Harness(this)
        val gate = CompletableDeferred<Unit>()
        harness.factory.onCreate = { it.presence.stall = gate }
        ready(harness)
        val retired = harness.client

        harness.desired.value = DesiredPresence.Away(LEFT_AT)
        runCurrent()
        // Logout waits for the in-flight send, then revokes the lease.
        val logout = async { harness.manager.logout() }
        runCurrent()
        gate.complete(Unit)
        runCurrent()
        logout.await()

        harness.desired.value = DesiredPresence.Available
        runCurrent()
        assertEquals(
            "only the send that owned the transport went out",
            listOf<Pair<String?, String?>>(null to null),
            retired.presence.calls.map { it.show to it.idleSince },
        )
    }

    private companion object {
        const val ROOM = "general@muc.waddle.test"
        const val PEER = "alice@waddle.test"

        /** First backoff step with `PinnedRandom(0.5)`. */
        const val RECONNECT_DELAY_MILLIS = 1_000L

        val LEFT_AT: Instant = Instant.parse("2026-10-16T09:15:30.250Z")
        val LEFT_AGAIN_AT: Instant = Instant.parse("2026-10-16T09:20:00Z")
    }
}
