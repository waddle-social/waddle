package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage

class TimelineStoreTest {
    private val store = TimelineStore()

    @Before
    fun setUp() {
        store.setOwnBareJid("me@waddle.test")
    }

    @Test
    fun `live then mam replay dedupes on stanza id and keeps the live record`() {
        store.onLiveMessage(
            testMessage(id = "orig-1", stanzaId = "stanza-1", body = "hi", from = "alice@waddle.test"),
        )
        store.onArchivedMessage(
            testArchivedMessage(mamId = "mam-1", id = "orig-1", stanzaId = "stanza-1", body = "hi"),
        )

        val items = store.timeline("alice@waddle.test").value
        assertEquals(1, items.size)
        assertEquals("stanza-1", items[0].id)
        assertTrue("live record must win", items[0].source is TimelineSource.Live)
    }

    @Test
    fun `mam then live replay dedupes and upgrades to the live record`() {
        store.onArchivedMessage(
            testArchivedMessage(mamId = "mam-1", stanzaId = "stanza-1", timestamp = "2026-07-15T09:00:00Z"),
        )
        store.onLiveMessage(testMessage(stanzaId = "stanza-1"))

        val items = store.timeline("alice@waddle.test").value
        assertEquals(1, items.size)
        assertTrue(items[0].source is TimelineSource.Live)
        assertEquals("archived timestamp survives the upgrade", "2026-07-15T09:00:00Z", items[0].timestamp)
    }

    @Test
    fun `orders by timestamp then insertion`() {
        store.onArchivedMessage(
            testArchivedMessage(mamId = "m1", stanzaId = "s1", timestamp = "2026-07-15T10:00:00Z", body = "second"),
        )
        store.onArchivedMessage(
            testArchivedMessage(mamId = "m2", stanzaId = "s2", timestamp = "2026-07-15T09:00:00Z", body = "first"),
        )
        // Same timestamp as s1: insertion order breaks the tie.
        store.onArchivedMessage(
            testArchivedMessage(mamId = "m3", stanzaId = "s3", timestamp = "2026-07-15T10:00:00Z", body = "third"),
        )
        // No timestamp: live messages are newest and go last.
        store.onLiveMessage(testMessage(stanzaId = "s4", body = "fourth"))

        val bodies = store.timeline("alice@waddle.test").value.map { it.body }
        assertEquals(listOf("first", "second", "third", "fourth"), bodies)
    }

    @Test
    fun `distinct conversations stay isolated`() {
        store.onLiveMessage(
            testMessage(stanzaId = "dm-1", from = "alice@waddle.test", messageType = "chat"),
        )
        store.onLiveMessage(
            testMessage(
                stanzaId = "muc-1",
                from = "room@muc.waddle.test/alice",
                to = null,
                messageType = "groupchat",
                isMuc = true,
            ),
        )

        assertEquals(1, store.timeline("alice@waddle.test").value.size)
        assertEquals(1, store.timeline("room@muc.waddle.test").value.size)
        assertEquals("muc-1", store.timeline("room@muc.waddle.test").value[0].id)
    }

    @Test
    fun `own messages are mine and route to the peer conversation`() {
        store.onLiveMessage(
            testMessage(stanzaId = "sent-1", from = "me@waddle.test/phone", to = "alice@waddle.test"),
        )

        val items = store.timeline("alice@waddle.test").value
        assertEquals(1, items.size)
        assertTrue(items[0].isMine)

        store.onLiveMessage(testMessage(stanzaId = "recv-1", from = "alice@waddle.test/web"))
        assertFalse(store.timeline("alice@waddle.test").value.last().isMine)
    }

    @Test
    fun `bodyless messages are skipped`() {
        store.onLiveMessage(testMessage(stanzaId = "cs-1", body = null))
        assertTrue(store.timeline("alice@waddle.test").value.isEmpty())
    }

    @Test
    fun `live overflow trims the oldest rows at the cap`() {
        val bounded = TimelineStore(maxItemsPerConversation = 3).apply {
            setOwnBareJid("me@waddle.test")
        }
        repeat(5) { index ->
            bounded.onLiveMessage(
                testMessage(
                    stanzaId = "s-$index",
                    body = "live-$index",
                    from = "alice@waddle.test",
                    timestamp = "2026-07-15T10:00:0${index}Z",
                ),
            )
        }

        val bodies = bounded.timeline("alice@waddle.test").value.map { it.body }
        assertEquals("newest cap-many survive, oldest dropped", listOf("live-2", "live-3", "live-4"), bodies)
    }

    @Test
    fun `mam backfill is never evicted while the user is paging`() {
        val bounded = TimelineStore(maxItemsPerConversation = 3).apply {
            setOwnBareJid("me@waddle.test")
        }
        // Live traffic fills the conversation to the cap...
        repeat(3) { index ->
            bounded.onLiveMessage(
                testMessage(
                    stanzaId = "live-$index",
                    body = "live-$index",
                    from = "alice@waddle.test",
                    timestamp = "2026-07-15T10:00:0${index}Z",
                ),
            )
        }
        // ...and an older MAM page merges in: the archived rows must all
        // land (only LIVE appends enforce the cap — class KDoc invariant).
        bounded.onArchivedMessage(
            testArchivedMessage(mamId = "m1", stanzaId = "old-1", body = "old-1", timestamp = "2026-07-15T09:00:00Z"),
        )
        bounded.onArchivedMessage(
            testArchivedMessage(mamId = "m2", stanzaId = "old-2", body = "old-2", timestamp = "2026-07-15T09:00:01Z"),
        )

        val bodies = bounded.timeline("alice@waddle.test").value.map { it.body }
        assertEquals(
            listOf("old-1", "old-2", "live-0", "live-1", "live-2"),
            bodies,
        )
    }

    @Test
    fun `live arrival while over the cap re-trims from the oldest end`() {
        val bounded = TimelineStore(maxItemsPerConversation = 3).apply {
            setOwnBareJid("me@waddle.test")
        }
        repeat(3) { index ->
            bounded.onLiveMessage(
                testMessage(
                    stanzaId = "live-$index",
                    body = "live-$index",
                    from = "alice@waddle.test",
                    timestamp = "2026-07-15T10:00:0${index}Z",
                ),
            )
        }
        bounded.onArchivedMessage(
            testArchivedMessage(mamId = "m1", stanzaId = "old-1", body = "old-1", timestamp = "2026-07-15T09:00:00Z"),
        )
        assertEquals(4, bounded.timeline("alice@waddle.test").value.size)

        bounded.onLiveMessage(
            testMessage(
                stanzaId = "live-3",
                body = "live-3",
                from = "alice@waddle.test",
                timestamp = "2026-07-15T10:00:03Z",
            ),
        )

        val bodies = bounded.timeline("alice@waddle.test").value.map { it.body }
        assertEquals(
            "oldest (backfilled) rows evict first; paging can re-fetch them",
            listOf("live-1", "live-2", "live-3"),
            bodies,
        )
    }

    @Test
    fun `clear empties published timelines`() {
        val timeline = store.timeline("alice@waddle.test")
        store.onLiveMessage(testMessage(stanzaId = "s1"))
        assertEquals(1, timeline.value.size)

        store.clear()
        assertTrue(timeline.value.isEmpty())
    }
}
