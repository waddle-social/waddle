package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage
import social.waddle.android.client.testPresence

/** Room author → real bare JID resolution: retained, never guessed. */
class OccupantJidStoreTest {
    private val room = "room@muc.waddle.test"

    private fun liveRow(from: String, mine: Boolean = false) = TimelineItem(
        id = "m1",
        conversationJid = room,
        from = from,
        body = "hi",
        timestamp = null,
        isMine = mine,
        source = TimelineSource.Live(testMessage(from = from, messageType = "groupchat", isMuc = true)),
    )

    private fun archivedRow(from: String, authorRealJid: String?) = TimelineItem(
        id = "m1",
        conversationJid = room,
        from = from,
        body = "hi",
        timestamp = null,
        isMine = false,
        source = TimelineSource.Archived(
            testArchivedMessage(from = from, messageType = "groupchat", authorRealJid = authorRealJid),
        ),
    )

    @Test
    fun `occupant presence maps the nick to the real bare JID`() {
        val store = OccupantJidStore()
        store.onPresence(testPresence(from = "$room/alice", mucJid = "alice@waddle.test/phone"))

        assertEquals(mapOf(room to mapOf("alice" to "alice@waddle.test")), store.jids.value)
    }

    @Test
    fun `a departed author keeps the last known mapping`() {
        val store = OccupantJidStore()
        store.onPresence(testPresence(from = "$room/alice", mucJid = "alice@waddle.test/phone"))
        store.onPresence(
            testPresence(from = "$room/alice", presenceType = "unavailable", mucJid = "alice@waddle.test/phone"),
        )
        store.onPresence(testPresence(from = "$room/alice", presenceType = "unavailable"))

        assertEquals(
            "alice@waddle.test",
            authorBareJidOf(liveRow("$room/alice"), store.jids.value[room].orEmpty(), ownBareJid = "me@waddle.test"),
        )
    }

    @Test
    fun `live presence re-points a reused nick but an archive row only fills gaps`() {
        val store = OccupantJidStore()
        store.onArchivedAuthor("$room/sam", "sam.old@waddle.test")
        store.onArchivedAuthor("$room/sam", "sam.older@waddle.test")
        assertEquals("sam.old@waddle.test", store.jids.value[room]?.get("sam"))

        store.onPresence(testPresence(from = "$room/sam", mucJid = "sam.new@waddle.test/x"))
        store.onArchivedAuthor("$room/sam", "sam.old@waddle.test")
        assertEquals("sam.new@waddle.test", store.jids.value[room]?.get("sam"))
    }

    @Test
    fun `an unknown room author resolves to null, never a nick-derived guess`() {
        assertNull(authorBareJidOf(liveRow("$room/alice"), emptyMap(), ownBareJid = "me@waddle.test"))
        // Semi-anonymous room: presence without a real JID maps nothing.
        val store = OccupantJidStore()
        store.onPresence(testPresence(from = "$room/alice", mucJid = null, mucRole = null))
        assertNull(authorBareJidOf(liveRow("$room/alice"), store.jids.value[room].orEmpty(), "me@waddle.test"))
    }

    @Test
    fun `the archived real JID wins over the nick map for that row`() {
        val row = archivedRow("$room/alice", authorRealJid = "alice.then@waddle.test/web")

        assertEquals(
            "alice.then@waddle.test",
            authorBareJidOf(row, mapOf("alice" to "alice.now@waddle.test"), ownBareJid = "me@waddle.test"),
        )
    }

    @Test
    fun `own rows and 1-1 rows resolve without the nick map`() {
        assertEquals(
            "me@waddle.test",
            authorBareJidOf(liveRow("$room/me", mine = true), emptyMap(), ownBareJid = "me@waddle.test/phone"),
        )
        val dm = TimelineItem(
            id = "d1",
            conversationJid = "bob@waddle.test",
            from = "bob@waddle.test/laptop",
            body = "yo",
            timestamp = null,
            isMine = false,
            source = TimelineSource.Live(testMessage(from = "bob@waddle.test/laptop")),
        )
        assertEquals("bob@waddle.test", authorBareJidOf(dm, emptyMap(), ownBareJid = "me@waddle.test"))
    }
}
