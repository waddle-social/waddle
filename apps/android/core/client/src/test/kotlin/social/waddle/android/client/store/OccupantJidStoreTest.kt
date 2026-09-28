package social.waddle.android.client.store

import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage
import social.waddle.android.client.testPresence
import social.waddle.client.ffi.WaddleMucRole

/** Current nick → real JID lookup, and stored-row author resolution. */
class OccupantJidStoreTest {
    private val room = "room@muc.waddle.test"
    private val self = "me@waddle.test"

    private fun liveRow(from: String, mine: Boolean = false, authorJid: String? = null) = TimelineItem(
        id = "m1",
        conversationJid = room,
        from = from,
        body = "hi",
        timestamp = null,
        isMine = mine,
        source = TimelineSource.Live(testMessage(from = from, messageType = "groupchat", isMuc = true)),
        authorJid = authorJid,
    )

    @Test
    fun `occupant presence maps the nick to the real bare JID and survives a leave`() {
        val store = OccupantJidStore()
        store.onPresence(testPresence(from = "$room/alice", mucJid = "alice@waddle.test/phone"))
        store.onPresence(testPresence(from = "$room/alice", presenceType = "unavailable"))

        assertEquals("alice@waddle.test", store.jidFor(room, "alice"))
    }

    @Test
    fun `a reused nick re-points the lookup`() {
        val store = OccupantJidStore()
        store.onPresence(testPresence(from = "$room/alice", mucJid = "alice@waddle.test/phone"))
        store.onPresence(testPresence(from = "$room/alice", mucJid = "bob@waddle.test/x"))

        assertEquals("bob@waddle.test", store.jidFor(room, "alice"))
    }

    @Test
    fun `an available occupant presence without a real JID forgets the old holder`() {
        val store = OccupantJidStore()
        store.onPresence(
            testPresence(from = "$room/alice", mucJid = "alice@waddle.test/phone", mucRole = WaddleMucRole.PARTICIPANT),
        )
        // The room turned semi-anonymous (or we were demoted): the nick's
        // current holder is unknown and must not inherit Alice.
        store.onPresence(testPresence(from = "$room/alice", mucJid = null, mucRole = WaddleMucRole.PARTICIPANT))

        assertNull(store.jidFor(room, "alice"))
    }

    @Test
    fun `leaves and non-occupant presences keep the known mapping`() {
        val store = OccupantJidStore()
        store.onPresence(
            testPresence(from = "$room/alice", mucJid = "alice@waddle.test/phone", mucRole = WaddleMucRole.PARTICIPANT),
        )
        store.onPresence(
            testPresence(from = "$room/alice", presenceType = "unavailable", mucRole = WaddleMucRole.NONE),
        )
        // A plain contact presence carries no muc#user payload at all.
        store.onPresence(testPresence(from = "$room/alice", mucJid = null))

        assertEquals("alice@waddle.test", store.jidFor(room, "alice"))
    }

    @Test
    fun `semi-anonymous presence maps nothing`() {
        val store = OccupantJidStore()
        store.onPresence(testPresence(from = "$room/alice", mucJid = null))

        assertNull(store.jidFor(room, "alice"))
    }

    @Test
    fun `room rows resolve only through the JID stamped when stored`() {
        assertNull(authorBareJidOf(liveRow("$room/alice"), self))
        val stamped = liveRow("$room/alice", authorJid = "alice@waddle.test")
        assertEquals("alice@waddle.test", authorBareJidOf(stamped, self))
    }

    @Test
    fun `own rows and 1-1 rows resolve without a stamp`() {
        assertEquals("me@waddle.test", authorBareJidOf(liveRow("$room/me", mine = true), "Me@waddle.test/phone"))
        val dm = TimelineItem(
            id = "d1",
            conversationJid = "bob@waddle.test",
            from = "Bob@waddle.test/laptop",
            body = "yo",
            timestamp = null,
            isMine = false,
            source = TimelineSource.Live(testMessage(from = "Bob@waddle.test/laptop")),
        )
        assertEquals("bob@waddle.test", authorBareJidOf(dm, self))
    }

    @Test
    fun `archived rows are stamped with their normalized muc-user JID`() {
        val store = TimelineStore()
        store.setOwnBareJid(self)
        store.onArchivedMessage(
            testArchivedMessage(
                from = "$room/gone",
                to = self,
                messageType = "groupchat",
                authorRealJid = "Gone@Waddle.test/web",
            ),
        )

        assertEquals("gone@waddle.test", store.timeline(room).value.single().authorJid)
    }

    @Test
    fun `case variants of the room and real JID share one normalized entry`() = runTest {
        val store = OccupantJidStore()
        store.onPresence(testPresence(from = "Room@MUC.waddle.test/alice", mucJid = "Alice@Waddle.Test/x"))
        store.onPresence(testPresence(from = "room@muc.waddle.test/bob", mucJid = "BOB@waddle.test"))

        assertEquals(
            mapOf("alice" to "alice@waddle.test", "bob" to "bob@waddle.test"),
            store.jidsIn("ROOM@muc.waddle.test").first(),
        )
        assertEquals(setOf(room), store.jids.value.keys)
        // Nicks stay case-sensitive (XEP-0045 resourceparts).
        assertNull(store.jidFor(room, "Alice"))
    }

    private fun twinLive(timestamp: String?, stanzaId: String = "t1", originId: String? = null) = testMessage(
        id = stanzaId,
        stanzaId = stanzaId,
        stanzaIdBy = room,
        originId = originId,
        from = "$room/alice",
        to = self,
        messageType = "groupchat",
        isMuc = true,
        timestamp = timestamp,
    )

    private fun twinArchived(
        stanzaId: String = "t1",
        originId: String? = null,
        authorRealJid: String = "alice@waddle.test/web",
    ) = testArchivedMessage(
        mamId = "m-$stanzaId",
        id = stanzaId,
        stanzaId = stanzaId,
        stanzaIdBy = room,
        originId = originId,
        from = "$room/alice",
        to = self,
        messageType = "groupchat",
        authorRealJid = authorRealJid,
    )

    @Test
    fun `a live twin replacing its archived row keeps the archived author stamp`() {
        val store = TimelineStore()
        store.setOwnBareJid(self)
        store.onArchivedMessage(twinArchived())
        // Unstamped live copy (delayed / no presence) must not erase it.
        store.onLiveMessage(twinLive(timestamp = "2026-07-15T10:00:00Z"), authorJid = null)

        val row = store.timeline(room).value.single()
        assertTrue(row.source is TimelineSource.Live)
        assertEquals("alice@waddle.test", row.authorJid)
    }

    @Test
    fun `an archived twin attributes an unstamped live row`() {
        val store = TimelineStore()
        store.setOwnBareJid(self)
        store.onLiveMessage(twinLive(timestamp = "2026-07-15T10:00:00Z"), authorJid = null)
        store.onArchivedMessage(twinArchived())

        val row = store.timeline(room).value.single()
        assertTrue(row.source is TimelineSource.Live)
        assertEquals("alice@waddle.test", row.authorJid)
    }

    @Test
    fun `an archived twin never overrides a live row's own stamp`() {
        val store = TimelineStore()
        store.setOwnBareJid(self)
        store.onLiveMessage(twinLive(timestamp = null), authorJid = "alice.live@waddle.test")
        store.onArchivedMessage(twinArchived())

        assertEquals("alice.live@waddle.test", store.timeline(room).value.single().authorJid)
    }

    @Test
    fun `a reused origin id never lends a stamp`() {
        val store = TimelineStore()
        store.setOwnBareJid(self)
        // Alice's message as delayed room history: no stamp.
        store.onLiveMessage(twinLive(timestamp = "2026-07-15T10:00:00Z", stanzaId = "s1", originId = "x"), null)
        // Mallory takes the nick and reuses Alice's origin id, live and archived.
        store.onLiveMessage(twinLive(timestamp = null, stanzaId = "s2", originId = "x"), "mallory@waddle.test")
        store.onArchivedMessage(twinArchived(stanzaId = "s3", originId = "x", authorRealJid = "mallory@waddle.test"))

        // Both merged into Alice's row on the shared origin id, unstamped.
        val rows = store.timeline(room).value
        assertEquals(listOf("s1"), rows.map { it.id })
        assertNull(rows.single().authorJid)

        // The genuine archive copy (the room's own stanza id) still stamps it.
        store.onArchivedMessage(twinArchived(stanzaId = "s1", originId = "x", authorRealJid = "alice@waddle.test"))
        assertEquals("alice@waddle.test", store.timeline(room).value.single().authorJid)
    }

    @Test
    fun `a live twin never fills an archived row's stamp`() {
        val store = TimelineStore()
        store.setOwnBareJid(self)
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "m-s1",
                id = "s1",
                stanzaId = "s1",
                stanzaIdBy = room,
                from = "$room/alice",
                to = self,
                messageType = "groupchat",
            ),
        )
        store.onLiveMessage(twinLive(timestamp = "2026-07-15T10:00:00Z", stanzaId = "s1"), "erin@waddle.test")

        val row = store.timeline(room).value.single()
        assertTrue(row.source is TimelineSource.Live)
        assertNull(row.authorJid)
    }
}
