package social.waddle.android.feature.conversation

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.store.TimelineStore
import social.waddle.android.client.store.UnreadStore
import social.waddle.android.testArchivedMessage
import social.waddle.android.testMamPage
import social.waddle.android.testMessage
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleSendMessageOutcome

@OptIn(ExperimentalCoroutinesApi::class)
class ConversationViewModelTest {
    /** Fans fetched pages into the store like the session manager does. */
    private class FakeConversationIo(private val store: TimelineStore) : ConversationIo {
        val fetchCalls = mutableListOf<Pair<UInt, String?>>()
        val pages = ArrayDeque<WaddleMamPage>()
        var joinedCount = 0
        var sendOutcome: WaddleSendMessageOutcome = WaddleSendMessageOutcome.Sent("stanza-1")
        val sent = mutableListOf<String>()

        override suspend fun ensureJoined() {
            joinedCount += 1
        }

        override suspend fun fetchHistory(maxMessages: UInt, beforeId: String?): WaddleMamPage? {
            fetchCalls += maxMessages to beforeId
            val page = pages.removeFirstOrNull() ?: return null
            page.messages.forEach(store::onArchivedMessage)
            return page
        }

        override suspend fun send(body: String): WaddleSendMessageOutcome {
            sent += body
            return sendOutcome
        }
    }

    private val store = TimelineStore().apply { setOwnBareJid(OWN_JID) }
    private val events = MutableSharedFlow<XmppEvent>(extraBufferCapacity = 16)
    private val unreadStore = UnreadStore()
    private val io = FakeConversationIo(store)

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun createViewModel(): ConversationViewModel = ConversationViewModel(
        conversationJid = ROOM_JID,
        timeline = store.timeline(ROOM_JID),
        events = events,
        unreadStore = unreadStore,
        io = io,
        clock = { 1_000L },
    )

    @Test
    fun `init joins and fetches the newest page`() = runTest {
        io.pages += testMamPage(
            messages = listOf(archived(mamId = "mam-1", stanzaId = "s1")),
            isComplete = false,
        )
        val viewModel = createViewModel()
        runCurrent()

        assertEquals(1, io.joinedCount)
        assertEquals(listOf(50u to null), io.fetchCalls)
        val state = viewModel.uiState.value
        assertEquals(1, state.rows.size)
        assertTrue(state.rows.single() is ConversationRow.Stored)
        assertFalse(state.reachedHistoryStart)
    }

    @Test
    fun `loadOlder pages with the before cursor and stops at the archive start`() = runTest {
        io.pages += testMamPage(
            messages = listOf(archived(mamId = "mam-2", stanzaId = "s2")),
            isComplete = false,
        )
        val viewModel = createViewModel()
        runCurrent()

        // Older page replays s2 (MAM refetch) plus the genuinely older s1.
        io.pages += testMamPage(
            messages = listOf(
                archived(mamId = "mam-1", stanzaId = "s1", timestamp = "2026-07-15T09:00:00Z"),
                archived(mamId = "mam-2", stanzaId = "s2"),
            ),
            firstId = "mam-1",
            isComplete = true,
        )
        viewModel.loadOlder()
        runCurrent()

        assertEquals(listOf(50u to null, 50u to "mam-2"), io.fetchCalls)
        val state = viewModel.uiState.value
        assertEquals("replayed s2 must dedupe", 2, state.rows.size)
        assertTrue(state.reachedHistoryStart)

        viewModel.loadOlder()
        runCurrent()
        assertEquals("no fetch past the archive start", 2, io.fetchCalls.size)
    }

    @Test
    fun `loadOlder is single flight`() = runTest {
        io.pages += testMamPage(
            messages = listOf(archived(mamId = "mam-1", stanzaId = "s1")),
            isComplete = false,
        )
        val viewModel = createViewModel()
        viewModel.loadOlder()
        viewModel.loadOlder()
        runCurrent()

        assertEquals(1, io.fetchCalls.size)
    }

    @Test
    fun `session ready refetches the newest page`() = runTest {
        io.pages += testMamPage(
            messages = listOf(archived(mamId = "mam-1", stanzaId = "s1")),
            isComplete = true,
        )
        val viewModel = createViewModel()
        runCurrent()
        assertTrue(viewModel.uiState.value.reachedHistoryStart)

        io.pages += testMamPage(
            messages = listOf(archived(mamId = "mam-2", stanzaId = "s2")),
            isComplete = true,
        )
        events.emit(XmppEvent.SessionReady)
        runCurrent()

        assertEquals(listOf(50u to null, 50u to null), io.fetchCalls)
        assertEquals(2, viewModel.uiState.value.rows.size)
    }

    @Test
    fun `optimistic send dedupes against the room echo by origin id`() = runTest {
        // Realistic MUC reflection: the room stamps a FRESH XEP-0359
        // stanza-id; only the origin-id round-trips the client id the
        // send returned. Matching on the collapsed timeline key alone
        // rendered every sent channel message twice.
        io.sendOutcome = WaddleSendMessageOutcome.Sent("client-origin-id")
        val viewModel = createViewModel()
        runCurrent()

        viewModel.send("hello room")
        runCurrent()
        assertEquals(listOf("hello room"), io.sent)
        val pendingRow = viewModel.uiState.value.rows.single()
        assertTrue(pendingRow is ConversationRow.Unconfirmed)
        assertFalse((pendingRow as ConversationRow.Unconfirmed).message.failed)

        store.onLiveMessage(
            testMessage(
                stanzaId = "room-assigned-stanza-id",
                originId = "client-origin-id",
                from = "$ROOM_JID/icepuma",
                to = OWN_JID,
                body = "hello room",
                messageType = "groupchat",
                isMuc = true,
            ),
        )
        runCurrent()

        val rows = viewModel.uiState.value.rows
        assertEquals("echo replaces the pending row", 1, rows.size)
        assertTrue(rows.single() is ConversationRow.Stored)
    }

    @Test
    fun `delivery ack marks the pending row without hiding it`() = runTest {
        io.sendOutcome = WaddleSendMessageOutcome.Sent("client-origin-id")
        val viewModel = createViewModel()
        runCurrent()

        viewModel.send("hello room")
        runCurrent()
        assertEquals(1, viewModel.uiState.value.rows.size)

        events.emit(XmppEvent.DeliveryAcked("client-origin-id"))
        runCurrent()

        // Deleting on ack would vanish DM sends (no reflection to the
        // sending resource) — the row stays, flagged delivered.
        val row = viewModel.uiState.value.rows.single()
        assertTrue(row is ConversationRow.Unconfirmed)
        assertTrue((row as ConversationRow.Unconfirmed).message.acked)
        assertFalse(row.message.failed)

        // The MUC echo (matching identity) is what finally replaces it.
        store.onLiveMessage(
            testMessage(
                stanzaId = "room-assigned-id",
                originId = "client-origin-id",
                from = "$ROOM_JID/icepuma",
                to = OWN_JID,
                body = "hello room",
                messageType = "groupchat",
                isMuc = true,
            ),
        )
        runCurrent()
        val rows = viewModel.uiState.value.rows
        assertEquals(1, rows.size)
        assertTrue(rows.single() is ConversationRow.Stored)
    }

    @Test
    fun `acked dm send stays visible with no reflection`() = runTest {
        io.sendOutcome = WaddleSendMessageOutcome.Sent("dm-origin-id")
        val viewModel = createViewModel()
        runCurrent()

        viewModel.send("hi there")
        runCurrent()
        events.emit(XmppEvent.DeliveryAcked("dm-origin-id"))
        runCurrent()

        // 1:1 chats are never reflected back to the sending resource:
        // the acked optimistic row is the message's only representation
        // until the next MAM fetch and must not disappear.
        val row = viewModel.uiState.value.rows.single()
        assertTrue(row is ConversationRow.Unconfirmed)
        assertTrue((row as ConversationRow.Unconfirmed).message.acked)
        assertEquals("hi there", row.message.body)
    }

    @Test
    fun `failed outcomes and delivery failures mark the pending row`() = runTest {
        io.sendOutcome = WaddleSendMessageOutcome.NotConnected
        val viewModel = createViewModel()
        runCurrent()

        viewModel.send("offline message")
        runCurrent()
        val failedRow = viewModel.uiState.value.rows.single() as ConversationRow.Unconfirmed
        assertTrue(failedRow.message.failed)

        // Retry with a working transport that later reports a 0198 failure.
        io.sendOutcome = WaddleSendMessageOutcome.Sent("s-late-fail")
        viewModel.retry(failedRow.message.localId)
        runCurrent()
        assertEquals(listOf("offline message", "offline message"), io.sent)
        val retried = viewModel.uiState.value.rows.single() as ConversationRow.Unconfirmed
        assertFalse(retried.message.failed)

        events.emit(XmppEvent.DeliveryFailed("s-late-fail"))
        runCurrent()
        val lateFailed = viewModel.uiState.value.rows.single() as ConversationRow.Unconfirmed
        assertTrue(lateFailed.message.failed)
    }

    @Test
    fun `visible conversation suppresses and clears unread`() = runTest {
        val viewModel = createViewModel()
        runCurrent()

        unreadStore.onLiveMessage(ROOM_JID, isMine = false)
        assertEquals(mapOf(ROOM_JID to 1), unreadStore.counts.value)

        viewModel.onConversationVisible()
        assertTrue(unreadStore.counts.value.isEmpty())
        unreadStore.onLiveMessage(ROOM_JID, isMine = false)
        assertTrue("active conversation never counts", unreadStore.counts.value.isEmpty())

        viewModel.onConversationHidden()
        unreadStore.onLiveMessage(ROOM_JID, isMine = false)
        assertEquals(mapOf(ROOM_JID to 1), unreadStore.counts.value)
    }

    private fun archived(
        mamId: String,
        stanzaId: String,
        timestamp: String = "2026-07-15T10:00:00Z",
    ) = testArchivedMessage(
        mamId = mamId,
        stanzaId = stanzaId,
        timestamp = timestamp,
        from = "$ROOM_JID/alice",
        to = OWN_JID,
        messageType = "groupchat",
    )

    private companion object {
        const val ROOM_JID = "general@muc.waddle.test"
        const val OWN_JID = "icepuma@waddle.test"
    }
}
