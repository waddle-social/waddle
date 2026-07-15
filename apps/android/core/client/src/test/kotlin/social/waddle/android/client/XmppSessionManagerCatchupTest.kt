package social.waddle.android.client

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.prefs.ResumeCursor
import social.waddle.android.client.prefs.SessionPrefs
import social.waddle.client.ffi.WaddleClientEvent

/**
 * Reconnect catch-up + per-conversation resume cursors (web
 * `waddle.chat.resume-cursors` / reconnect-catchup parity): cursors
 * persist from the fan-out, and a fresh (non-resumed) stream refetches
 * the newest MAM page per joined room plus the most recent DMs.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class XmppSessionManagerCatchupTest {
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
    fun `live messages persist an advance-only resume cursor`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        harness.factory.emit(
            WaddleClientEvent.Message(
                testMessage(
                    stanzaId = "s-new",
                    from = "alice@waddle.test",
                    to = "icepuma@waddle.test",
                    timestamp = "2026-07-15T12:00:00Z",
                ),
            ),
        )
        runCurrent()
        assertEquals(
            ResumeCursor(stanzaId = "s-new", timestamp = "2026-07-15T12:00:00Z"),
            harness.prefs.resumeCursors.first()["alice@waddle.test"],
        )

        // An older MAM backfill row must never move the cursor backwards.
        harness.factory.emit(
            WaddleClientEvent.MamResult(
                testArchivedMessage(
                    mamId = "mam-old",
                    stanzaId = "s-old",
                    from = "alice@waddle.test",
                    to = "icepuma@waddle.test",
                    messageType = "chat",
                    timestamp = "2026-07-15T09:00:00Z",
                ),
            ),
        )
        runCurrent()
        assertEquals(
            "s-new",
            harness.prefs.resumeCursors.first()["alice@waddle.test"]?.stanzaId,
        )

        harness.manager.logout()
    }

    @Test
    fun `first session of the process catches up every joined room`() = runTest {
        val harness = Harness(this)
        harness.prefs.setJoinedRooms(setOf("general@muc.waddle.test", "dev@muc.waddle.test"))

        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        assertEquals(
            setOf(
                Triple("general@muc.waddle.test", XmppSessionManager.CATCHUP_PAGE_SIZE, null as String?),
                Triple("dev@muc.waddle.test", XmppSessionManager.CATCHUP_PAGE_SIZE, null as String?),
            ),
            client.fetchHistoryCalls.toSet(),
        )

        harness.manager.logout()
    }

    @Test
    fun `dm catch-up is bounded to the most recent cursors`() = runTest {
        val harness = Harness(this)
        harness.prefs.setResumeCursors(
            mapOf(
                "a@waddle.test" to ResumeCursor("s-a", "2026-07-15T08:00:00Z"),
                "b@waddle.test" to ResumeCursor("s-b", "2026-07-15T11:00:00Z"),
                "c@waddle.test" to ResumeCursor("s-c", "2026-07-15T10:00:00Z"),
                "d@waddle.test" to ResumeCursor("s-d", "2026-07-15T09:00:00Z"),
            ),
        )

        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val fetched = harness.factory.clients.single().fetchHistoryCalls.map { it.first }
        assertEquals(
            "newest ${XmppSessionManager.CATCHUP_DM_LIMIT} DMs only, oldest left out",
            listOf("b@waddle.test", "c@waddle.test", "d@waddle.test"),
            fetched,
        )

        harness.manager.logout()
    }

    @Test
    fun `a resumed stream skips catch-up`() = runTest {
        val harness = Harness(this)
        harness.prefs.setJoinedRooms(setOf("general@muc.waddle.test"))
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertEquals(
            "first session of the process catches up",
            1,
            harness.factory.clients.single().fetchHistoryCalls.size,
        )

        // Persist a resume snapshot, drop, and reconnect: the next
        // attempt presents the snapshot, so the manager assumes the
        // stream resumed and 0198 replay covered the gap.
        harness.factory.emit(WaddleClientEvent.ResumeStateChanged(testResumeState()))
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(1_000L)
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertEquals(2, harness.factory.clients.size)
        assertTrue(
            "resumed session must not refetch",
            harness.factory.clients.last().fetchHistoryCalls.isEmpty(),
        )

        harness.manager.logout()
    }

    @Test
    fun `a fresh stream without a snapshot reruns catch-up`() = runTest {
        val harness = Harness(this)
        harness.prefs.setJoinedRooms(setOf("general@muc.waddle.test"))
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        // The FFI cleared the resume state (rejected/expired): the next
        // attempt carries no snapshot and is definitely a fresh stream.
        harness.factory.emit(WaddleClientEvent.ResumeStateChanged(null))
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(1_000L)
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertEquals(2, harness.factory.clients.size)
        assertEquals(
            "fresh stream refetches the joined room",
            listOf(Triple("general@muc.waddle.test", XmppSessionManager.CATCHUP_PAGE_SIZE, null as String?)),
            harness.factory.clients.last().fetchHistoryCalls,
        )

        harness.manager.logout()
    }
}
