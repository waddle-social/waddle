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
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.MarkupRef
import social.waddle.android.client.MarkupRefType
import social.waddle.android.client.MentionRef
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.SendResult
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.store.TimelineStore
import social.waddle.android.client.store.UnreadStore
import social.waddle.client.ffi.WaddleLinkPreviewLookup
import social.waddle.client.ffi.WaddleLinkPreviewLookupPreview
import social.waddle.client.ffi.WaddleLinkPreviewLookupStatus
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleSendMessageOutcome

/**
 * The composer send pipeline: markdown → markup spans, link-preview
 * token attach (fresh only), and the GIF plain-body fast path.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ConversationSendPipelineTest {

    private class RecordingIo : ConversationIo {
        val sent = mutableListOf<Pair<String, MessageSendExtras?>>()
        val lookupCalls = mutableListOf<String>()
        var lookup: WaddleLinkPreviewLookup? = null

        override suspend fun fetchHistory(maxMessages: UInt, beforeId: String?): WaddleMamPage? = null

        override suspend fun send(body: String, extras: MessageSendExtras?): SendResult {
            sent += body to extras
            return SendResult(WaddleSendMessageOutcome.Sent("stanza-1"))
        }

        override suspend fun lookupLinkPreview(url: String): WaddleLinkPreviewLookup? {
            lookupCalls += url
            return lookup
        }
    }

    private val store = TimelineStore().apply { setOwnBareJid("me@waddle.test") }
    private val events = MutableSharedFlow<XmppEvent>(extraBufferCapacity = 16)
    private val io = RecordingIo()

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun createViewModel() = ConversationViewModel(
        conversationJid = "room@muc.waddle.test",
        isGroupchat = true,
        timeline = store.timeline("room@muc.waddle.test"),
        events = events,
        unreadStore = UnreadStore(),
        io = io,
        clock = { NOW_MS },
    )

    private fun readyLookup(expiresAt: String) = WaddleLinkPreviewLookup(
        status = WaddleLinkPreviewLookupStatus.READY,
        preview = WaddleLinkPreviewLookupPreview(
            token = "tok-1",
            originalUrl = "https://example.com/a",
            normalizedUrl = "https://example.com/a",
            expiresAt = expiresAt,
            title = null,
            description = null,
            image = null,
            playerEmbed = null,
        ),
    )

    @Test
    fun `markdown converts to markup spans and a clean wire body`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        viewModel.send("**bold** words")
        runCurrent()

        val (body, extras) = io.sent.single()
        assertEquals("bold words", body)
        assertEquals(
            listOf(MarkupRef(MarkupRefType.BOLD, 0u, 4u)),
            extras?.markup,
        )
    }

    @Test
    fun `mentions survive the markdown pass rebased`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        viewModel.send("**b** @alice", listOf(MentionRef("xmpp:alice@waddle.test", 6u, 12u)))
        runCurrent()

        val extras = io.sent.single().second
        assertEquals(
            listOf(MentionRef("xmpp:alice@waddle.test", 2u, 8u)),
            extras?.mentions,
        )
    }

    @Test
    fun `an eligible url attaches a fresh preview token`() = runTest {
        io.lookup = readyLookup(expiresAt = FUTURE_EXPIRY)
        val viewModel = createViewModel()
        runCurrent()
        viewModel.send("see https://example.com/a now")
        runCurrent()

        assertEquals(listOf("https://example.com/a"), io.lookupCalls)
        assertEquals("tok-1", io.sent.single().second?.linkPreviewToken)
    }

    @Test
    fun `an expired preview token is never attached`() = runTest {
        io.lookup = readyLookup(expiresAt = PAST_EXPIRY)
        val viewModel = createViewModel()
        runCurrent()
        viewModel.send("see https://example.com/a now")
        runCurrent()

        assertNull(io.sent.single().second?.linkPreviewToken)
    }

    @Test
    fun `bodies without urls never trigger a lookup`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        viewModel.send("plain words")
        runCurrent()

        assertTrue(io.lookupCalls.isEmpty())
        assertNull(io.sent.single().second)
    }

    @Test
    fun `gif sends are plain bodies without markup or lookups`() = runTest {
        io.lookup = readyLookup(expiresAt = FUTURE_EXPIRY)
        val viewModel = createViewModel()
        runCurrent()
        viewModel.sendGif("https://media0.giphy.com/g1/giphy.gif")
        runCurrent()

        val (body, extras) = io.sent.single()
        assertEquals("https://media0.giphy.com/g1/giphy.gif", body)
        assertNull(extras)
        assertTrue(io.lookupCalls.isEmpty())
    }

    private companion object {
        const val NOW_MS = 1_750_000_000_000L
        const val FUTURE_EXPIRY = "2026-09-01T00:00:00Z"
        const val PAST_EXPIRY = "2020-01-01T00:00:00Z"
    }
}
