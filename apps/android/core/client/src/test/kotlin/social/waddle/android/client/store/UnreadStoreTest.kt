package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class UnreadStoreTest {
    private val store = UnreadStore()

    @Test
    fun `live messages increment per conversation keyed by bare jid`() {
        store.onLiveMessage("alice@waddle.test/phone", isMine = false)
        store.onLiveMessage("alice@waddle.test", isMine = false)
        store.onLiveMessage("general@muc.waddle.test", isMine = false)

        assertEquals(
            mapOf("alice@waddle.test" to 2, "general@muc.waddle.test" to 1),
            store.counts.value,
        )
    }

    @Test
    fun `own messages never count`() {
        store.onLiveMessage("alice@waddle.test", isMine = true)

        assertTrue(store.counts.value.isEmpty())
    }

    @Test
    fun `the active conversation never accrues unread`() {
        store.setActiveConversation("alice@waddle.test/phone")
        store.onLiveMessage("alice@waddle.test", isMine = false)
        store.onLiveMessage("bob@waddle.test", isMine = false)

        assertEquals(mapOf("bob@waddle.test" to 1), store.counts.value)
        assertTrue(store.isActiveConversation("alice@waddle.test"))
        assertFalse(store.isActiveConversation("bob@waddle.test"))
    }

    @Test
    fun `clearing the active conversation resumes counting`() {
        store.setActiveConversation("alice@waddle.test")
        store.setActiveConversation(null)
        store.onLiveMessage("alice@waddle.test", isMine = false)

        assertEquals(mapOf("alice@waddle.test" to 1), store.counts.value)
    }

    @Test
    fun `compare-and-clear only clears the matching conversation`() {
        store.setActiveConversation("bob@waddle.test")

        // Screen A's late pause must not clobber screen B's marker.
        store.clearActiveConversationIf("alice@waddle.test")
        assertTrue(store.isActiveConversation("bob@waddle.test"))

        store.clearActiveConversationIf("bob@waddle.test/resource")
        assertFalse(store.isActiveConversation("bob@waddle.test"))
    }

    @Test
    fun `clear removes only that conversation's count`() {
        store.onLiveMessage("alice@waddle.test", isMine = false)
        store.onLiveMessage("bob@waddle.test", isMine = false)

        store.clear("alice@waddle.test/phone")

        assertEquals(mapOf("bob@waddle.test" to 1), store.counts.value)
    }

    @Test
    fun `set unless active recomputes counts outright`() {
        store.onLiveMessage("alice@waddle.test", isMine = false)

        store.setUnlessActive("alice@waddle.test", 5)
        assertEquals(mapOf("alice@waddle.test" to 5), store.counts.value)

        store.setUnlessActive("alice@waddle.test", 0)
        assertTrue(store.counts.value.isEmpty())
    }

    @Test
    fun `set unless active skips the on-screen conversation`() {
        store.onLiveMessage("alice@waddle.test", isMine = false)
        store.setActiveConversation("alice@waddle.test")

        store.setUnlessActive("alice@waddle.test", 9)

        assertEquals(
            "the local read path owns the on-screen badge",
            mapOf("alice@waddle.test" to 1),
            store.counts.value,
        )
    }

    @Test
    fun `clear all wipes every count`() {
        store.onLiveMessage("alice@waddle.test", isMine = false)
        store.onLiveMessage("bob@waddle.test", isMine = false)

        store.clearAll()

        assertTrue(store.counts.value.isEmpty())
    }
}
