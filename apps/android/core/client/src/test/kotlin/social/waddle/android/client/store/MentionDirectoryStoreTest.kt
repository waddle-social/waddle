package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Test
import social.waddle.client.ffi.WaddleUserSearchEntry

class MentionDirectoryStoreTest {
    @Test
    fun `records users per room keyed by normalised bare jid, first seen wins`() {
        val store = MentionDirectoryStore()

        store.record("General@MUC.waddle.test", listOf(entry("Carol@Waddle.Test", "carol")))
        store.record(
            "general@muc.waddle.test",
            listOf(entry("carol@waddle.test", "carol2"), entry("dave@waddle.test", "dave")),
        )

        assertEquals(
            listOf(entry("carol@waddle.test", "carol"), entry("dave@waddle.test", "dave")),
            store.usersIn("general@muc.waddle.test"),
        )
        assertEquals(emptyList<WaddleUserSearchEntry>(), store.usersIn("random@muc.waddle.test"))
    }

    @Test
    fun `skips unusable entries and clears with the session`() {
        val store = MentionDirectoryStore()

        store.record("general@muc.waddle.test", listOf(entry("waddle.test", "server"), entry("e@waddle.test", "")))
        assertEquals(emptyList<WaddleUserSearchEntry>(), store.usersIn("general@muc.waddle.test"))

        store.record("general@muc.waddle.test", listOf(entry("carol@waddle.test", "carol")))
        store.clear()
        assertEquals(emptyList<WaddleUserSearchEntry>(), store.usersIn("general@muc.waddle.test"))
    }

    private fun entry(jid: String, username: String) =
        WaddleUserSearchEntry(jid = jid, username = username, displayName = null)
}
