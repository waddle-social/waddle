package social.waddle.android.navigation

import org.junit.Assert.assertEquals
import org.junit.Test
import social.waddle.android.client.testChannel
import social.waddle.client.ffi.WaddleTopology

/** Notification-tap classification of conversation JIDs into nav keys. */
class ConversationNavKeysTest {
    private val topology = WaddleTopology(
        spaces = emptyList(),
        channels = listOf(
            testChannel("general@muc.waddle.test", name = "General"),
            testChannel("gdm-1@muc.waddle.test", name = "Alice, Bob", isGroupDm = true),
        ),
    )

    @Test
    fun `group dm rooms classify onto the dm surface`() {
        assertEquals(
            WaddleNavKey.GroupDm(roomJid = "gdm-1@muc.waddle.test", name = "Alice, Bob"),
            conversationNavKeyFor(topology, emptySet(), "gdm-1@muc.waddle.test/alice"),
        )
    }

    @Test
    fun `topology channels open as channels`() {
        assertEquals(
            WaddleNavKey.Channel(roomJid = "general@muc.waddle.test", name = "General"),
            conversationNavKeyFor(topology, emptySet(), "general@muc.waddle.test"),
        )
    }

    @Test
    fun `joined rooms outside the topology fall back to channel with localpart name`() {
        assertEquals(
            WaddleNavKey.Channel(roomJid = "offsite@muc.waddle.test", name = "offsite"),
            conversationNavKeyFor(topology, setOf("offsite@muc.waddle.test"), "offsite@muc.waddle.test"),
        )
    }

    @Test
    fun `unknown bare jids open as peer dms`() {
        assertEquals(
            WaddleNavKey.Dm(peerJid = "carol@waddle.test", name = "carol"),
            conversationNavKeyFor(topology, emptySet(), "carol@waddle.test/phone"),
        )
    }
}
