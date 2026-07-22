package social.waddle.android.feature.conversation

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.launch
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
import social.waddle.android.client.MentionCandidate
import social.waddle.android.client.MentionRef
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.SendResult
import social.waddle.android.client.StickerHash
import social.waddle.android.client.StickerItem
import social.waddle.android.client.StickerSendRef
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.store.TimelineStore
import social.waddle.android.client.store.UnreadStore
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMamPage
import social.waddle.android.client.testMessage
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleSendMessageOutcome

@OptIn(ExperimentalCoroutinesApi::class)
class ConversationViewModelTest {
    /** Fans fetched pages into the store like the session manager does. */
    private class FakeConversationIo(private val store: TimelineStore) : ConversationIo {
        val fetchCalls = mutableListOf<Pair<UInt, String?>>()
        val pages = ArrayDeque<WaddleMamPage>()
        var joinedCount = 0
        var sendResult: SendResult = SendResult(WaddleSendMessageOutcome.Sent("stanza-1"))
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

        override suspend fun send(body: String, extras: MessageSendExtras?): SendResult {
            sent += body
            sentExtras += extras
            return sendResult
        }

        val sentExtras = mutableListOf<MessageSendExtras?>()

        var markDisplayedCalls = 0

        override suspend fun markDisplayed() {
            markDisplayedCalls += 1
        }

        var reactionResult: VerbResult = VerbResult.Ok
        val reactionCalls = mutableListOf<Pair<String, String>>()

        override suspend fun toggleReaction(targetId: String, emoji: String): VerbResult {
            reactionCalls += targetId to emoji
            return reactionResult
        }

        var retractionResult: VerbResult = VerbResult.Ok
        val retractionCalls = mutableListOf<String>()

        override suspend fun sendRetraction(targetId: String): VerbResult {
            retractionCalls += targetId
            return retractionResult
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
        isGroupchat = true,
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
        io.sendResult = SendResult(WaddleSendMessageOutcome.Sent("client-origin-id"))
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
    fun `sendSticker sends the desc body with the sticker extras`() = runTest {
        val viewModel = createViewModel()
        runCurrent()

        viewModel.sendSticker(
            StickerItem(
                desc = "🐧",
                mediaType = "image/webp",
                sizeBytes = 1234L,
                width = 512,
                height = 256,
                hashes = listOf(StickerHash(algo = "sha-256", valueB64 = "aGFzaA==")),
                sources = listOf("https://upload.waddle.test/penguin.webp"),
            ),
            packId = "pack-1",
        )
        runCurrent()

        assertEquals(listOf("🐧"), io.sent)
        val sticker = io.sentExtras.single()?.sticker
        assertEquals(
            StickerSendRef(
                packId = "pack-1",
                desc = "🐧",
                url = "https://upload.waddle.test/penguin.webp",
                mediaType = "image/webp",
                sizeBytes = 1234L,
                width = 512,
                height = 256,
                hashes = listOf(StickerHash(algo = "sha-256", valueB64 = "aGFzaA==")),
            ),
            sticker,
        )
        // The optimistic row shows the desc while the echo is pending.
        assertTrue(viewModel.uiState.value.rows.single() is ConversationRow.Unconfirmed)
    }

    @Test
    fun `sendSticker without a source or desc never dispatches`() = runTest {
        val viewModel = createViewModel()
        runCurrent()

        viewModel.sendSticker(StickerItem(desc = "🐧", sources = emptyList()), packId = "pack-1")
        viewModel.sendSticker(
            StickerItem(desc = " ", sources = listOf("https://upload.waddle.test/x.webp")),
            packId = "pack-1",
        )
        runCurrent()

        assertTrue(io.sent.isEmpty())
    }

    @Test
    fun `delivery ack marks the pending row without hiding it`() = runTest {
        io.sendResult = SendResult(WaddleSendMessageOutcome.Sent("client-origin-id"))
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
    fun `failed outcomes and delivery failures mark the pending row`() = runTest {
        // Permanent (non-queueable) failure: no queue id accompanies it.
        io.sendResult = SendResult(WaddleSendMessageOutcome.StanzaError)
        val viewModel = createViewModel()
        runCurrent()

        viewModel.send("rejected message")
        runCurrent()
        val failedRow = viewModel.uiState.value.rows.single() as ConversationRow.Unconfirmed
        assertTrue(failedRow.message.failed)

        // Retry with a working transport that later reports a 0198 failure.
        io.sendResult = SendResult(WaddleSendMessageOutcome.Sent("s-late-fail"))
        viewModel.retry(failedRow.message.localId)
        runCurrent()
        assertEquals(listOf("rejected message", "rejected message"), io.sent)
        val retried = viewModel.uiState.value.rows.single() as ConversationRow.Unconfirmed
        assertFalse(retried.message.failed)

        events.emit(XmppEvent.DeliveryFailed("s-late-fail"))
        runCurrent()
        val lateFailed = viewModel.uiState.value.rows.single() as ConversationRow.Unconfirmed
        assertTrue(lateFailed.message.failed)
    }

    @Test
    fun `queued send renders as queued and reconciles with the replayed echo`() = runTest {
        // The manager persisted the offline send under "q-1"; the queue
        // replay reuses that id as the XEP-0359 origin-id.
        io.sendResult = SendResult(WaddleSendMessageOutcome.NotConnected, queuedId = "q-1")
        val viewModel = createViewModel()
        runCurrent()

        viewModel.send("typed on the subway")
        runCurrent()
        val queuedRow = viewModel.uiState.value.rows.single() as ConversationRow.Unconfirmed
        assertTrue("queued, not failed", queuedRow.message.queued)
        assertFalse(queuedRow.message.failed)
        assertEquals("q-1", queuedRow.message.stanzaId)

        // Reconnect: the drain sent the message and the room echoed it.
        store.onLiveMessage(
            testMessage(
                stanzaId = "room-assigned-id",
                originId = "q-1",
                from = "$ROOM_JID/icepuma",
                to = OWN_JID,
                body = "typed on the subway",
                messageType = "groupchat",
                isMuc = true,
            ),
        )
        runCurrent()

        val rows = viewModel.uiState.value.rows
        assertEquals("echo replaces the queued row", 1, rows.size)
        assertTrue(rows.single() is ConversationRow.Stored)
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

    @Test
    fun `dm action targets prefer the author-assigned id over the local stanza id`() = runTest {
        val dmStore = TimelineStore().apply { setOwnBareJid(OWN_JID) }
        val dmViewModel = ConversationViewModel(
            conversationJid = "alice@waddle.test",
            isGroupchat = false,
            timeline = dmStore.timeline("alice@waddle.test"),
            events = events,
            unreadStore = unreadStore,
            io = FakeConversationIo(dmStore),
            clock = { 1_000L },
        )
        dmStore.onLiveMessage(
            testMessage(id = "author-id", originId = "origin-id", stanzaId = "local-archive-id"),
        )
        runCurrent()

        val item = dmStore.timeline("alice@waddle.test").value.single()
        // The peer never saw our archive's stanza id — the action id
        // must be author-assigned or the mutation cannot apply remotely.
        assertEquals("origin-id", dmViewModel.actionTargetIdOf(item))
        assertEquals("origin-id", dmViewModel.threadIdFor(item))
    }

    @Test
    fun `thread screens never mark the parent conversation displayed`() = runTest {
        val threadViewModel = ConversationViewModel(
            conversationJid = ROOM_JID,
            isGroupchat = true,
            timeline = store.timeline(ROOM_JID),
            events = events,
            unreadStore = unreadStore,
            io = io,
            threadId = "t1",
            clock = { 1_000L },
        )
        runCurrent()

        threadViewModel.onConversationVisible()
        store.onLiveMessage(
            testMessage(
                id = "id-s9",
                stanzaId = "s9",
                from = "$ROOM_JID/alice",
                to = null,
                messageType = "groupchat",
                isMuc = true,
                thread = "t1",
            ),
        )
        runCurrent()

        assertEquals(0, io.markDisplayedCalls)
        // The active-conversation marker stays untouched too: a live
        // message to the parent feed must still count as unread.
        unreadStore.onLiveMessage(ROOM_JID, isMine = false)
        assertEquals(mapOf(ROOM_JID to 1), unreadStore.counts.value)
    }

    @Test
    fun `refused reactions and retractions surface action failures`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        // Own MUC message with a room-assigned stanza id: actionable.
        store.onLiveMessage(
            testMessage(
                id = "id-s1",
                stanzaId = "s1",
                stanzaIdBy = ROOM_JID,
                from = "$ROOM_JID/icepuma",
                to = null,
                body = "mine",
                messageType = "groupchat",
                isMuc = true,
            ),
        )
        runCurrent()
        val failures = mutableListOf<VerbResult.Failure>()
        backgroundScope.launch { viewModel.actionFailures.collect(failures::add) }
        runCurrent()
        val item = store.timeline(ROOM_JID).value.single()

        io.reactionResult = VerbResult.Rejected
        viewModel.toggleReaction(item, "👍")
        runCurrent()
        io.retractionResult = VerbResult.NotConnected
        viewModel.retract(item)
        runCurrent()

        assertEquals(listOf("s1" to "👍"), io.reactionCalls)
        assertEquals(listOf("s1"), io.retractionCalls)
        // Both refusals reach the screen (they used to fail silently).
        assertEquals(listOf(VerbResult.Rejected, VerbResult.NotConnected), failures)
    }

    @Test
    fun `send threads mention refs into the wire extras`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        val mentions = listOf(MentionRef(uri = "xmpp:bob@waddle.test", begin = 3u, end = 7u))

        viewModel.send("hi @bob", mentions)
        runCurrent()

        assertEquals(listOf("hi @bob"), io.sent)
        assertEquals(mentions, io.sentExtras.single()?.mentions)
    }

    @Test
    fun `mention candidates flow through to the composer state`() = runTest {
        val candidates = listOf(
            MentionCandidate(display = "bob", uri = "xmpp:bob@waddle.test", isBroadcast = false),
        )
        val viewModel = ConversationViewModel(
            conversationJid = ROOM_JID,
            isGroupchat = true,
            timeline = store.timeline(ROOM_JID),
            events = events,
            unreadStore = unreadStore,
            io = io,
            mentionCandidates = flowOf(candidates),
            clock = { 1_000L },
        )
        runCurrent()

        assertEquals(candidates, viewModel.mentionCandidates.value)
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
