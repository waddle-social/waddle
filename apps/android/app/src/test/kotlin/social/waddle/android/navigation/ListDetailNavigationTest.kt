package social.waddle.android.navigation

import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** Back-stack rules behind the list-detail layout. */
class ListDetailNavigationTest {
    private val general = WaddleNavKey.Channel(roomJid = "general@muc.waddle.test", name = "General")
    private val alice = WaddleNavKey.Dm(peerJid = "alice@waddle.test", name = "Alice")

    @Test
    fun `opening from the lists replaces the open conversation`() {
        val backStack = NavBackStack<NavKey>(WaddleNavKey.Home, general)

        backStack.showConversation(alice)

        assertEquals(listOf(WaddleNavKey.Home, alice), backStack.toList())
    }

    @Test
    fun `opening from the lists drops a thread and its conversation`() {
        val thread = WaddleNavKey.Thread(
            conversationJid = general.roomJid,
            isGroupchat = true,
            threadId = "t-1",
        )
        val backStack = NavBackStack<NavKey>(WaddleNavKey.Home, general, thread)

        backStack.showConversation(alice)

        assertEquals(listOf(WaddleNavKey.Home, alice), backStack.toList())
    }

    @Test
    fun `opening from the root is a plain push`() {
        val backStack = NavBackStack<NavKey>(WaddleNavKey.Home)

        backStack.showConversation(general)

        assertEquals(listOf(WaddleNavKey.Home, general), backStack.toList())
    }

    @Test
    fun `the open conversation follows the top of the stack`() {
        assertEquals(general.roomJid, openConversationJidOf(listOf(WaddleNavKey.Home, general)))
        assertEquals(alice.peerJid, openConversationJidOf(listOf(WaddleNavKey.Home, alice)))
        assertEquals(
            general.roomJid,
            openConversationJidOf(
                listOf(
                    WaddleNavKey.Home,
                    general,
                    WaddleNavKey.Thread(conversationJid = general.roomJid, isGroupchat = true, threadId = "t-1"),
                ),
            ),
        )
        assertNull(openConversationJidOf(listOf(WaddleNavKey.Home)))
        assertNull(openConversationJidOf(listOf(WaddleNavKey.Home, WaddleNavKey.Settings)))
    }
}
