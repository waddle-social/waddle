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
    fun `a sidebar page replaces the open conversation`() {
        val backStack = NavBackStack<NavKey>(WaddleNavKey.Home, general)

        backStack.showConversation(WaddleNavKey.People)

        assertEquals(listOf(WaddleNavKey.Home, WaddleNavKey.People), backStack.toList())
        assertNull(openConversationJidOf(backStack))
    }

    @Test
    fun `folding makes a tab root opened from the sidebar its tab`() {
        val backStack = NavBackStack<NavKey>(WaddleNavKey.Home, WaddleNavKey.DmList, alice)

        backStack.adaptTo(sidebarPane = false)

        assertEquals(listOf(WaddleNavKey.DmList, alice), backStack.toList())
    }

    @Test
    fun `folding keeps a page that is not a tab on the Home tab`() {
        val backStack = NavBackStack<NavKey>(WaddleNavKey.Home, WaddleNavKey.Rooms, general)

        backStack.adaptTo(sidebarPane = false)

        assertEquals(listOf(WaddleNavKey.Home, WaddleNavKey.Rooms, general), backStack.toList())
    }

    @Test
    fun `unfolding puts the sidebar under another tab's stack`() {
        val backStack = NavBackStack<NavKey>(WaddleNavKey.People, alice)

        backStack.adaptTo(sidebarPane = true)

        assertEquals(listOf(WaddleNavKey.Home, WaddleNavKey.People, alice), backStack.toList())
        backStack.adaptTo(sidebarPane = true)
        assertEquals(3, backStack.size)
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
