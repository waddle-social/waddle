package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.client.ffi.WaddleChatState

class ChatStateStoreTest {
    private var now = 0L
    private val store = ChatStateStore(clock = { now })

    private fun names() = store.composing.value["room@muc.waddle.test"].orEmpty()

    @Test
    fun `composing adds the sender and other states remove immediately`() {
        store.onChatState("room@muc.waddle.test", "alice", WaddleChatState.COMPOSING, isMine = false)
        assertEquals(listOf("alice"), names())

        store.onChatState("room@muc.waddle.test", "alice", WaddleChatState.PAUSED, isMine = false)
        assertTrue(names().isEmpty())
    }

    @Test
    fun `own chat states never show`() {
        store.onChatState("room@muc.waddle.test", "me", WaddleChatState.COMPOSING, isMine = true)
        assertTrue(names().isEmpty())
    }

    @Test
    fun `typing expires after five seconds on sweep`() {
        store.onChatState("room@muc.waddle.test", "alice", WaddleChatState.COMPOSING, isMine = false)
        now = 4_999
        assertTrue("still composing", store.sweep())
        assertEquals(listOf("alice"), names())

        now = 5_000
        assertFalse("expired; nothing left to tick for", store.sweep())
        assertTrue(names().isEmpty())
    }

    @Test
    fun `a fresh composing re-arms the expiry`() {
        store.onChatState("room@muc.waddle.test", "alice", WaddleChatState.COMPOSING, isMine = false)
        now = 4_000
        store.onChatState("room@muc.waddle.test", "alice", WaddleChatState.COMPOSING, isMine = false)
        now = 8_000
        assertTrue(store.sweep())
        assertEquals(listOf("alice"), names())
    }

    @Test
    fun `a delivered message clears its sender's typing`() {
        store.onChatState("room@muc.waddle.test", "alice", WaddleChatState.COMPOSING, isMine = false)
        store.onLiveMessage("room@muc.waddle.test", "alice")
        assertTrue(names().isEmpty())
    }

    @Test
    fun `senders sort and conversations stay isolated`() {
        store.onChatState("room@muc.waddle.test", "carol", WaddleChatState.COMPOSING, isMine = false)
        store.onChatState("room@muc.waddle.test", "alice", WaddleChatState.COMPOSING, isMine = false)
        store.onChatState("alice@waddle.test", "alice", WaddleChatState.COMPOSING, isMine = false)

        assertEquals(listOf("alice", "carol"), names())
        assertEquals(listOf("alice"), store.composing.value["alice@waddle.test"])
    }

    @Test
    fun `clear drops everything`() {
        store.onChatState("room@muc.waddle.test", "alice", WaddleChatState.COMPOSING, isMine = false)
        store.clear()
        assertTrue(store.composing.value.values.all { it.isEmpty() })
    }
}
