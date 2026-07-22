package social.waddle.android.feature.conversation

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
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
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.store.TimelineStore
import social.waddle.android.client.store.UnreadStore
import social.waddle.android.client.testMessage

/** XEP-0425 moderation through the conversation view model. */
@OptIn(ExperimentalCoroutinesApi::class)
class ConversationModerationTest {
    private class ModeratingIo : ConversationIo {
        val moderationCalls = mutableListOf<Pair<String, String?>>()
        var moderationResult: VerbResult = VerbResult.Ok

        override suspend fun fetchHistory(maxMessages: UInt, beforeId: String?) = null

        override suspend fun send(
            body: String,
            extras: social.waddle.android.client.MessageSendExtras?,
        ) = social.waddle.android.client.SendResult(
            social.waddle.client.ffi.WaddleSendMessageOutcome.Sent("s-out"),
        )

        override suspend fun sendModeration(targetId: String, reason: String?): VerbResult {
            moderationCalls += targetId to reason
            return moderationResult
        }
    }

    private val store = TimelineStore().apply { setOwnBareJid(OWN_JID) }
    private val events = MutableSharedFlow<XmppEvent>(extraBufferCapacity = 16)
    private val io = ModeratingIo()
    private val canModerate = MutableStateFlow(false)

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun createViewModel() = ConversationViewModel(
        conversationJid = ROOM_JID,
        isGroupchat = true,
        timeline = store.timeline(ROOM_JID),
        events = events,
        unreadStore = UnreadStore(),
        io = io,
        canModerate = canModerate,
    )

    private fun seedOtherUsersMessage(stanzaId: String) {
        store.onLiveMessage(
            testMessage(
                id = "id-$stanzaId",
                stanzaId = stanzaId,
                stanzaIdBy = ROOM_JID,
                from = "$ROOM_JID/mallory",
                to = null,
                messageType = "groupchat",
                isMuc = true,
            ),
        )
    }

    @Test
    fun `canModerate flows into the ui state`() = runTest {
        val viewModel = createViewModel()
        runCurrent()
        assertFalse(viewModel.uiState.value.canModerate)

        canModerate.value = true
        runCurrent()
        assertTrue(viewModel.uiState.value.canModerate)
    }

    @Test
    fun `moderate sends the XEP-0425 request for another user's message`() = runTest {
        seedOtherUsersMessage("s1")
        val viewModel = createViewModel()
        runCurrent()
        val item = store.timeline(ROOM_JID).value.single()

        viewModel.moderate(item)
        runCurrent()

        assertEquals(listOf("s1" to null), io.moderationCalls)
    }

    @Test
    fun `moderate refuses own messages`() = runTest {
        store.onLiveMessage(
            testMessage(
                id = "id-own",
                stanzaId = "s-own",
                stanzaIdBy = ROOM_JID,
                from = "$ROOM_JID/icepuma",
                to = null,
                messageType = "groupchat",
                isMuc = true,
            ),
        )
        val viewModel = createViewModel()
        runCurrent()
        val item = store.timeline(ROOM_JID).value.single()
        assertTrue(item.isMine)

        viewModel.moderate(item)
        runCurrent()

        assertTrue(io.moderationCalls.isEmpty())
    }

    private companion object {
        const val ROOM_JID = "general@muc.waddle.test"
        const val OWN_JID = "icepuma@waddle.test"
    }
}
