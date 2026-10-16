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
import social.waddle.client.ffi.WaddleTopology

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
        harness.manager.deliverArchivedPage(
            harness.factory.clients.last(),
            "alice@waddle.test",
            isGroupchat = false,
            testArchivedMessage(
                mamId = "mam-old",
                stanzaId = "s-old",
                from = "alice@waddle.test",
                to = "icepuma@waddle.test",
                messageType = "chat",
                timestamp = "2026-07-15T09:00:00Z",
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
    fun `session-ready retries a terminate-pending group-call session once`() = runTest {
        val harness = Harness(this)
        // A previous resource's group-call leave never got its mixer
        // terminate onto the wire; the flagged entry survives in the
        // session prefs.
        harness.prefs.mucCallSessions.markTerminatePending(
            "channel@muc.waddle.test",
            "c-owed",
            "icepuma@waddle.test/waddle-android-old",
        )

        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        assertEquals(
            listOf<RecordedCallVerb>(
                RecordedCallVerb.MujiSessionTerminate("channel@muc.waddle.test", "c-owed"),
            ),
            client.callVerbs,
        )
        assertTrue(
            harness.prefs.mucCallSessions
                .terminatePendingEntries("icepuma@waddle.test/waddle-android-new")
                .isEmpty(),
        )

        harness.manager.logout()
    }

    @Test
    fun `each native stream reruns catch-up`() = runTest {
        val harness = Harness(this)
        harness.prefs.setJoinedRooms(setOf("general@muc.waddle.test"))
        harness.manager.login(testSessionInfo())
        runCurrent()

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(1_000L)
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertEquals(2, harness.factory.clients.size)
        assertEquals(
            "each native stream refetches the joined room",
            listOf(Triple("general@muc.waddle.test", XmppSessionManager.CATCHUP_PAGE_SIZE, null as String?)),
            harness.factory.clients.last().fetchHistoryCalls,
        )

        harness.manager.logout()
    }

    // ── XEP-0402 bookmark-driven rejoin ──────────────────────────────

    @Test
    fun `each completed ready pipeline is counted`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        assertEquals(0L, harness.manager.readyPipelineRuns.value)

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertEquals(
            "markers queued before the stream are out once this advances",
            1L,
            harness.manager.readyPipelineRuns.value,
        )
        harness.manager.logout()
    }

    @Test
    fun `queued dm sends replay before topology discovery answers`() = runTest {
        val harness = Harness(this)
        harness.factory.onCreate = { client -> client.topology.delayMillis = TOPOLOGY_STALL_MILLIS }
        harness.manager.login(testSessionInfo())
        runCurrent()
        assertTrue(harness.manager.sendChatMessage("alice@waddle.test", "typed offline").queued)

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        assertEquals("discovery still parked", 0, client.topology.calls)
        assertEquals(listOf("alice@waddle.test" to "typed offline"), client.sendCalls)

        harness.manager.logout()
    }

    @Test
    fun `queued room sends join their room first and replay before the remaining joins`() = runTest {
        val harness = Harness(this)
        harness.prefs.setJoinedRooms(setOf("quiet@muc.waddle.test", "busy@muc.waddle.test"))
        harness.factory.onCreate = { client ->
            client.topology.delayMillis = TOPOLOGY_STALL_MILLIS
            client.topology.result = WaddleTopology(
                spaces = emptyList(),
                channels = listOf(testChannel("general@muc.waddle.test", autojoin = true)),
            )
        }
        harness.manager.login(testSessionInfo())
        runCurrent()
        assertTrue(harness.manager.sendGroupchatMessage("busy@muc.waddle.test", "typed offline").queued)

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        assertEquals(listOf("busy@muc.waddle.test"), client.joinRoomCalls.map { it.first })
        assertEquals(listOf("busy@muc.waddle.test" to "typed offline"), client.sendCalls)

        advanceTimeBy(TOPOLOGY_STALL_MILLIS + 1)
        runCurrent()

        assertEquals(
            setOf("busy@muc.waddle.test", "quiet@muc.waddle.test", "general@muc.waddle.test"),
            client.joinRoomCalls.map { it.first }.toSet(),
        )
        assertEquals("the priority room joins once", 3, client.joinRoomCalls.size)

        harness.manager.logout()
    }

    @Test
    fun `the two replay passes partition the queue`() = runTest {
        val harness = Harness(this)
        harness.factory.onCreate = { client ->
            client.topology.result = WaddleTopology(
                spaces = emptyList(),
                channels = listOf(testChannel("general@muc.waddle.test", autojoin = true)),
            )
        }
        harness.manager.login(testSessionInfo())
        runCurrent()
        // An autojoin-only room is not known before discovery, so its
        // send waits for the second pass; the DM goes in the first.
        assertTrue(harness.manager.sendGroupchatMessage("general@muc.waddle.test", "room").queued)
        assertTrue(harness.manager.sendChatMessage("alice@waddle.test", "dm").queued)

        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val client = harness.factory.clients.single()
        assertEquals(
            "each row replays exactly once, the room after its join",
            listOf("alice@waddle.test" to "dm", "general@muc.waddle.test" to "room"),
            client.sendCalls,
        )
        assertEquals(listOf("general@muc.waddle.test"), client.joinRoomCalls.map { it.first })

        harness.manager.logout()
    }

    @Test
    fun `join set unions autojoin channels with persisted-intent rooms`() = runTest {
        val harness = Harness(this)
        harness.prefs.setJoinedRooms(setOf("intent@muc.waddle.test"))
        harness.factory.onCreate = { client ->
            client.topology.result = WaddleTopology(
                spaces = emptyList(),
                channels = listOf(
                    testChannel("general@muc.waddle.test", autojoin = true),
                    testChannel("muted@muc.waddle.test", autojoin = false),
                    // Group DMs join too (their live messages must
                    // flow) — they are only excluded from LISTS.
                    testChannel("gdm@muc.waddle.test", autojoin = true, isGroupDm = true),
                ),
            )
        }

        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        val joined = harness.factory.clients.single().joinRoomCalls.map { it.first }.toSet()
        assertEquals(
            setOf("general@muc.waddle.test", "gdm@muc.waddle.test", "intent@muc.waddle.test"),
            joined,
        )
        // The autojoin group DM is joined but never listed as a channel.
        assertEquals(
            listOf("general@muc.waddle.test", "muted@muc.waddle.test"),
            harness.manager.roomStore.channels.value.map { it.roomJid },
        )
        assertEquals(
            listOf("gdm@muc.waddle.test"),
            harness.manager.roomStore.groupDms.value.map { it.roomJid },
        )
        // Autojoin rooms are session state, not user intent: the
        // persisted overlay must stay exactly as the user left it.
        assertEquals(setOf("intent@muc.waddle.test"), harness.prefs.joinedRooms.first())

        harness.manager.logout()
    }

    @Test
    fun `failed topology discovery degrades the join set to persisted rooms`() = runTest {
        val harness = Harness(this)
        harness.prefs.setJoinedRooms(setOf("intent@muc.waddle.test"))
        harness.factory.onCreate = { client ->
            client.topology.failure = IllegalStateException("disco down")
        }

        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertEquals(
            listOf("intent@muc.waddle.test" to "icepuma"),
            harness.factory.clients.single().joinRoomCalls,
        )

        harness.manager.logout()
    }

    // ── XEP-0492 notify-settings hydrate ─────────────────────────────

    @Test
    fun `a fresh stream hydrates notification settings from both carriers`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
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
        harness.factory.emit(WaddleClientEvent.Connected)
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
    fun `each native stream refetches notify settings`() = runTest {
        val harness = Harness(this)
        harness.manager.login(testSessionInfo())
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertEquals(1, harness.factory.clients.single().fetchUserBookmarksCalls)

        harness.factory.emit(WaddleClientEvent.Disconnected)
        runCurrent()
        advanceTimeBy(1_000L)
        runCurrent()
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()

        assertEquals(2, harness.factory.clients.size)
        assertEquals(
            "each native stream refetches bookmarks",
            1,
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
        harness.factory.emit(WaddleClientEvent.Connected)
        runCurrent()
        assertTrue(harness.manager.notifySettingsStore.entries.value.isNotEmpty())

        harness.manager.logout()

        assertTrue(harness.manager.notifySettingsStore.entries.value.isEmpty())
    }

    private companion object {
        const val TOPOLOGY_STALL_MILLIS = 10_000L
    }
}
