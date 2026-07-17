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
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.store.ConversationKind
import social.waddle.client.ffi.WaddleClientEvent
import social.waddle.client.ffi.WaddleDmBookmarkItem
import social.waddle.client.ffi.WaddleNotifyMode
import social.waddle.client.ffi.WaddleSessionReadyKind

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
            userPrefs = UserPrefs(InMemoryPreferencesDataStore()),
            reconnectPolicy = ReconnectPolicy(PinnedRandom(0.5)),
            dispatcher = StandardTestDispatcher(testScope.testScheduler),
        )
    }

    @Test
    fun `live messages persist an advance-only resume cursor`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emitReady()
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
        harness.factory.emitReady()
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
        harness.factory.emitReady()
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

        harness.factory.emitReady()
        runCurrent()
        assertEquals(
            "first session of the process catches up",
            1,
            harness.factory.clients.single().fetchHistoryCalls.size,
        )

        // Persist a resume snapshot, drop, and reconnect: the next
        // attempt presents the snapshot, so the manager assumes the
        // stream resumed and 0198 replay covered the gap.
        harness.factory.emitResumeStateChanged(testResumeState())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(1_000L)
        runCurrent()
        harness.factory.emitReady(WaddleSessionReadyKind.RESUMED)
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

        harness.factory.emitReady()
        runCurrent()

        // The FFI cleared the resume state (rejected/expired): the next
        // attempt carries no snapshot and is definitely a fresh stream.
        harness.factory.emitResumeStateChanged(null)
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(1_000L)
        runCurrent()
        harness.factory.emitReady()
        runCurrent()

        assertEquals(2, harness.factory.clients.size)
        assertEquals(
            "fresh stream refetches the joined room",
            listOf(Triple("general@muc.waddle.test", XmppSessionManager.CATCHUP_PAGE_SIZE, null as String?)),
            harness.factory.clients.last().fetchHistoryCalls,
        )

        harness.manager.logout()
    }

    // ── XEP-0492 notify-settings hydrate ─────────────────────────────

    @Test
    fun `a fresh stream hydrates notification settings from both carriers`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emitReady()
        runCurrent()

        val client = harness.factory.clients.single()
        assertEquals(1, client.fetchUserBookmarksCalls)
        assertEquals(1, client.fetchDmBookmarksCalls)
        harness.manager.logout()
    }

    @Test
    fun `hydrated overrides resolve while defaults cover the rest`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.clients.single().dmBookmarks = listOf(
            WaddleDmBookmarkItem(
                jid = "bob@waddle.test",
                notifyMode = WaddleNotifyMode.NEVER,
                richPayloadOptIn = false,
            ),
        )
        harness.factory.emitReady()
        runCurrent()

        val store = harness.manager.notifySettingsStore
        assertEquals(
            WaddleNotifyMode.NEVER,
            store.modeFor("bob@waddle.test", ConversationKind.DIRECT_CHAT),
        )
        // Un-hydrated conversations resolve to the §3 defaults.
        assertEquals(
            WaddleNotifyMode.ALWAYS,
            store.modeFor("carol@waddle.test", ConversationKind.DIRECT_CHAT),
        )
        harness.manager.logout()
    }

    @Test
    fun `a resumed stream skips the notify-settings refetch`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emitReady()
        runCurrent()
        assertEquals(1, harness.factory.clients.single().fetchUserBookmarksCalls)

        harness.factory.emitResumeStateChanged(testResumeState())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(1_000L)
        runCurrent()
        harness.factory.emitReady(WaddleSessionReadyKind.RESUMED)
        runCurrent()

        assertEquals(2, harness.factory.clients.size)
        assertEquals(
            "resumed session must not refetch bookmarks",
            0,
            harness.factory.clients.last().fetchUserBookmarksCalls,
        )
        harness.manager.logout()
    }

    @Test
    fun `logout clears hydrated notification settings`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.clients.single().dmBookmarks = listOf(
            WaddleDmBookmarkItem(
                jid = "bob@waddle.test",
                notifyMode = WaddleNotifyMode.NEVER,
                richPayloadOptIn = false,
            ),
        )
        harness.factory.emitReady()
        runCurrent()
        assertTrue(harness.manager.notifySettingsStore.entries.value.isNotEmpty())

        harness.manager.logout()

        assertTrue(harness.manager.notifySettingsStore.entries.value.isEmpty())
    }
}
