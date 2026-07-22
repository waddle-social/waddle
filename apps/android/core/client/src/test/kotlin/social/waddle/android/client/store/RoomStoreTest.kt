package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.testChannel
import social.waddle.client.ffi.WaddleTopology

class RoomStoreTest {
    private fun channel(roomJid: String, isGroupDm: Boolean = false) =
        testChannel(roomJid = roomJid, isGroupDm = isGroupDm)

    @Test
    fun `channels selector excludes group dm rooms`() {
        val store = RoomStore()
        store.setTopology(
            WaddleTopology(
                spaces = emptyList(),
                channels = listOf(
                    channel("general@muc.waddle.test"),
                    channel("gdm@muc.waddle.test", isGroupDm = true),
                ),
            ),
        )

        assertEquals(
            listOf("general@muc.waddle.test"),
            store.channels.value.map { it.roomJid },
        )
        // The raw topology keeps every room (the rejoin join set needs
        // autojoin group DMs too).
        assertEquals(2, store.topology.value.channels.size)
    }

    @Test
    fun `groupDms selector exposes group dm rooms for the dm surface`() {
        val store = RoomStore()
        store.setTopology(
            WaddleTopology(
                spaces = emptyList(),
                channels = listOf(
                    channel("general@muc.waddle.test"),
                    channel("gdm@muc.waddle.test", isGroupDm = true),
                ),
            ),
        )

        assertEquals(
            listOf("gdm@muc.waddle.test"),
            store.groupDms.value.map { it.roomJid },
        )
    }

    @Test
    fun `clear wipes the derived selectors with the topology`() {
        val store = RoomStore()
        store.setTopology(
            WaddleTopology(
                spaces = emptyList(),
                channels = listOf(
                    channel("general@muc.waddle.test"),
                    channel("gdm@muc.waddle.test", isGroupDm = true),
                ),
            ),
        )

        store.clear()

        assertTrue(store.channels.value.isEmpty())
        assertTrue(store.groupDms.value.isEmpty())
        assertTrue(store.topology.value.channels.isEmpty())
    }
}
