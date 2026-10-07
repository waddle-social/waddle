package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage
import social.waddle.client.ffi.WaddleStanzaId

class TimelineStoreTest {
    private val store = TimelineStore()

    @Before
    fun setUp() {
        store.setOwnBareJid("me@waddle.test")
    }

    @Test
    fun `rejection survives archived to live upgrade and does not affect a colliding peer id`() {
        val peer = "alice@waddle.test"
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "mam-1", id = "s-1", originId = "s-1",
                stanzaId = "archive-1", from = "me@waddle.test", to = peer, body = "mine",
            ),
        )
        store.onLiveMessage(
            testMessage(
                id = "s-1", originId = "s-1", stanzaId = null,
                from = peer, to = "me@waddle.test", body = "peer",
            ),
        )
        store.rejectOutbound(peer, "s-1")
        store.onLiveMessage(
            testMessage(
                id = "s-1", originId = "s-1", stanzaId = null,
                from = "me@waddle.test", to = peer, body = "mine",
            ),
        )
        val rows = store.timeline(peer).value
        assertEquals(2, rows.size)
        assertTrue(rows.single { it.isMine }.rejected)
        assertTrue(rows.single { it.isMine }.source is TimelineSource.Live)
        assertFalse(rows.single { !it.isMine }.rejected)
    }

    @Test
    fun `muc user traffic never becomes a timeline row`() {
        val room = "room@muc.waddle.test"
        // Live private message from an occupant, live invite from the bare
        // room, and an archived private message (history pages hit the store).
        val insertedPm = store.onLiveMessage(
            testMessage(id = "pm-1", from = "$room/alice", to = "me@waddle.test", mucUser = true),
        )
        val insertedInvite = store.onLiveMessage(
            testMessage(id = "inv-1", from = room, to = "me@waddle.test", messageType = "normal", mucUser = true),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "mam-1",
                id = "pm-2",
                from = "$room/alice",
                to = "me@waddle.test",
                mucUser = true,
            ),
        )

        assertFalse(insertedPm)
        assertFalse(insertedInvite)
        assertEquals(emptyList<TimelineItem>(), store.timeline(room).value)

        // Control: the same addressing without the marker is an ordinary row.
        assertTrue(
            store.onLiveMessage(testMessage(id = "pm-3", from = "$room/alice", to = "me@waddle.test")),
        )
        assertEquals(1, store.timeline(room).value.size)
    }

    @Test
    fun `bodyless call anchor still inserts as a feed row`() {
        val inserted = store.onLiveMessage(
            testMessage(
                id = "call-anchor-1",
                stanzaId = "call-anchor-1",
                body = null,
                from = "alice@waddle.test",
                thread = "c-sid-1",
                callThread = social.waddle.client.ffi.WaddleCallThreadAnchor(
                    kind = "dm",
                    sid = "c-sid-1",
                    media = listOf("audio"),
                    initiator = "alice@waddle.test/phone",
                    started = "2026-07-15T10:00:00Z",
                ),
            ),
        )

        assertTrue(inserted)
        val items = store.timeline("alice@waddle.test").value
        assertEquals(1, items.size)
        assertEquals("", items[0].body)
        assertTrue(items[0].hasCallThread)
        assertTrue(items[0].isFeedVisible)
        assertEquals("c-sid-1", items[0].callAnchor?.sid)
    }

    @Test
    fun `bodyless message without call payload is still dropped`() {
        assertFalse(store.onLiveMessage(testMessage(stanzaId = "s-drop", body = null)))
        assertTrue(store.timeline("alice@waddle.test").value.isEmpty())
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
    fun `a nick handover cannot overwrite an unknown archived author through reused sender ids`() {
        val room = "room@muc.waddle.test"
        store.onArchivedMessage(
            testArchivedMessage(
                id = "reused", originId = "reused", from = "$room/sam", to = null,
                messageType = "groupchat", body = "earlier participant",
            ),
        )
        assertTrue(
            store.onLiveMessage(
                testMessage(
                    id = "reused", originId = "reused", from = "$room/sam", to = null,
                    messageType = "groupchat", body = "later participant",
                ),
                authorJid = "later@waddle.test",
            ),
        )

        val rows = store.timeline(room).value
        assertEquals(listOf("earlier participant", "later participant"), rows.map { it.body })
        assertTrue(rows[0].source is TimelineSource.Archived)
        assertEquals("2026-07-15T10:00:00Z", rows[0].timestamp)
        assertEquals(null, rows[0].authorJid)
        assertEquals(2, rows.map { it.presentationId }.toSet().size)
    }

    @Test
    fun `unproven or changed real authors keep sender id collisions separate in either ingest order`() {
        val room = "room@muc.waddle.test"
        for ((archivedAuthor, liveAuthor) in listOf(
            null to null,
            null to "later@waddle.test",
            "earlier@waddle.test" to null,
            "earlier@waddle.test" to "later@waddle.test",
        )) {
            for (archiveFirst in listOf(true, false)) {
                store.clear()
                val archive = testArchivedMessage(
                    id = "reused", originId = "reused", from = "$room/sam", to = null,
                    messageType = "groupchat", body = "earlier", authorRealJid = archivedAuthor,
                )
                val live = testMessage(
                    id = "reused", originId = "reused", from = "$room/sam", to = null,
                    messageType = "groupchat", body = "later", timestamp = "2026-07-15T11:00:00Z",
                )
                if (archiveFirst) store.onArchivedMessage(archive)
                assertTrue(store.onLiveMessage(live, liveAuthor))
                if (!archiveFirst) store.onArchivedMessage(archive)

                assertEquals(listOf("earlier", "later"), store.timeline(room).value.map { it.body })
            }
        }
    }

    @Test
    fun `room assigned identity reconciles renamed twins even behind a sender injected stanza id`() {
        val room = "room@muc.waddle.test"
        for (archiveFirst in listOf(true, false)) {
            store.clear()
            val archive = testArchivedMessage(
                id = "archive-wire", stanzaId = "injected-archive", stanzaIdBy = "attacker@waddle.test",
                from = "$room/old-nick", to = null, messageType = "groupchat", authorRealJid = "sam@waddle.test",
            ).copy(
                stanzaIds = listOf(
                    WaddleStanzaId(id = "injected-archive", by = "attacker@waddle.test"),
                    WaddleStanzaId(id = "room-id", by = room.uppercase()),
                )
            )
            val live = testMessage(
                id = "live-wire", stanzaId = "injected-live", stanzaIdBy = "attacker@waddle.test",
                from = "$room/new-nick", to = null, messageType = "groupchat",
            ).copy(
                stanzaIds = listOf(
                    WaddleStanzaId(id = "injected-live", by = "attacker@waddle.test"),
                    WaddleStanzaId(id = "room-id", by = room),
                )
            )
            if (archiveFirst) store.onArchivedMessage(archive)
            assertEquals(!archiveFirst, store.onLiveMessage(live))
            if (!archiveFirst) store.onArchivedMessage(archive)

            val row = store.timeline(room).value.single()
            assertTrue(row.source is TimelineSource.Live)
            assertEquals("2026-07-15T10:00:00Z", row.timestamp)
            assertEquals("sam@waddle.test", row.authorJid)
        }
    }

    @Test
    fun `different room assigned ids never collapse even for the same real author and sender aliases`() {
        val room = "room@muc.waddle.test"
        store.onArchivedMessage(
            testArchivedMessage(
                id = "reused", originId = "reused", stanzaId = "room-1", stanzaIdBy = room,
                from = "$room/sam", to = null, messageType = "groupchat", body = "first",
                authorRealJid = "sam@waddle.test",
            ),
        )
        assertTrue(
            store.onLiveMessage(
                testMessage(
                    id = "reused", originId = "reused", stanzaId = "room-2", stanzaIdBy = room,
                    from = "$room/sam", to = null, messageType = "groupchat", body = "second",
                ),
                authorJid = "sam@waddle.test",
            )
        )

        assertEquals(listOf("first", "second"), store.timeline(room).value.map { it.body })
    }

    @Test
    fun `sender injected stanza ids never reconcile unknown room authors`() {
        val room = "room@muc.waddle.test"
        store.onArchivedMessage(
            testArchivedMessage(
                id = "reused", originId = "reused", stanzaId = "injected", stanzaIdBy = "attacker@waddle.test",
                from = "$room/sam", to = null, messageType = "groupchat", body = "earlier",
            ),
        )
        assertTrue(
            store.onLiveMessage(
                testMessage(
                    id = "reused", originId = "reused", stanzaId = "injected", stanzaIdBy = "attacker@waddle.test",
                    from = "$room/sam", to = null, messageType = "groupchat", body = "later",
                ),
            )
        )

        assertEquals(listOf("earlier", "later"), store.timeline(room).value.map { it.body })
    }

    @Test
    fun `same real author reconciles sender aliases across a nickname change`() {
        val room = "room@muc.waddle.test"
        store.onArchivedMessage(
            testArchivedMessage(
                id = "archive-wire", originId = "origin", from = "$room/old-nick", to = null,
                messageType = "groupchat", authorRealJid = "Sam@waddle.test/web",
            ),
        )
        val presentationId = store.timeline(room).value.single().presentationId
        assertFalse(
            store.onLiveMessage(
                testMessage(
                    id = "live-wire", originId = "origin", stanzaId = "room-id", stanzaIdBy = room,
                    from = "$room/new-nick", to = null, messageType = "groupchat",
                ),
                authorJid = "SAM@WADDLE.TEST/phone",
            )
        )

        val row = store.timeline(room).value.single()
        assertTrue(row.source is TimelineSource.Live)
        assertEquals("sam@waddle.test", row.authorJid)
        assertEquals(presentationId, row.presentationId)
        assertEquals("room-id", row.stanzaId)
        assertTrue("origin" in row.identityIds)
    }

    @Test
    fun `self presence proven own reflection reconciles its archive via origin id`() {
        val room = "room@muc.waddle.test"
        val ownStore = TimelineStore(actualOwnNickIn = { "actual-nick" }).apply {
            setOwnBareJid("me@waddle.test")
        }
        assertTrue(
            ownStore.onLiveMessage(
                testMessage(
                    id = "send-id", originId = "send-id", from = "$room/actual-nick", to = null,
                    messageType = "groupchat",
                )
            )
        )
        ownStore.onArchivedMessage(
            testArchivedMessage(
                id = "send-id", originId = "send-id", stanzaId = "room-id", stanzaIdBy = room,
                from = "$room/actual-nick", to = null, messageType = "groupchat", authorRealJid = "me@waddle.test",
            )
        )

        val row = ownStore.timeline(room).value.single()
        assertEquals("me@waddle.test", row.authorJid)
        assertEquals("2026-07-15T10:00:00Z", row.timestamp)
        assertTrue(row.isMine)
    }

    @Test
    fun `room assigned identity wins over an earlier matching author alias`() {
        val room = "room@muc.waddle.test"
        store.onArchivedMessage(
            testArchivedMessage(
                id = "old", originId = "alias", from = "$room/sam", to = null,
                messageType = "groupchat", body = "earlier", authorRealJid = "sam@waddle.test",
            )
        )
        store.onLiveMessage(
            testMessage(
                id = "canonical", stanzaId = "room-id", stanzaIdBy = room,
                from = "$room/sam", to = null, messageType = "groupchat", body = "canonical",
            ),
            "sam@waddle.test"
        )
        assertFalse(
            store.onLiveMessage(
                testMessage(
                    id = "replay", originId = "alias", stanzaId = "room-id", stanzaIdBy = room,
                    from = "$room/sam", to = null, messageType = "groupchat", body = "replayed canonical",
                ),
                "sam@waddle.test"
            )
        )

        assertEquals(listOf("earlier", "canonical"), store.timeline(room).value.map { it.body })
    }

    @Test
    fun `ambiguous verified author aliases cannot select an arbitrary room message`() {
        val room = "room@muc.waddle.test"
        for (id in listOf("room-1", "room-2")) {
            store.onLiveMessage(
                testMessage(
                    id = id, originId = "alias", stanzaId = id, stanzaIdBy = room,
                    from = "$room/sam", to = null, messageType = "groupchat", body = id,
                ),
                "sam@waddle.test"
            )
        }
        store.onArchivedMessage(
            testArchivedMessage(
                id = "archive", originId = "alias", from = "$room/sam", to = null,
                messageType = "groupchat", body = "unresolved", authorRealJid = "sam@waddle.test",
            )
        )

        assertEquals(3, store.timeline(room).value.size)
        assertEquals(setOf("room-1", "room-2", "unresolved"), store.timeline(room).value.map { it.body }.toSet())
    }

    @Test
    fun `trusted fallback retains room identity for a later replay without author mapping`() {
        val room = "room@muc.waddle.test"
        store.onLiveMessage(
            testMessage(
                id = "send", originId = "send", from = "$room/sam", to = null, messageType = "groupchat",
            ),
            "sam@waddle.test"
        )
        val presentationId = store.timeline(room).value.single().presentationId
        store.onArchivedMessage(
            testArchivedMessage(
                id = "send", originId = "send", stanzaId = "room-id", stanzaIdBy = room,
                from = "$room/sam", to = null, messageType = "groupchat", authorRealJid = "sam@waddle.test",
            )
        )
        assertFalse(
            store.onLiveMessage(
                testMessage(
                    id = "send", originId = "send", stanzaId = "room-id", stanzaIdBy = room,
                    from = "$room/sam", to = null, messageType = "groupchat",
                )
            )
        )

        assertEquals(1, store.timeline(room).value.size)
        assertEquals("room-id", store.timeline(room).value.single().assignedStanzaId(room)?.id)
        assertEquals(presentationId, store.timeline(room).value.single().presentationId)
        assertEquals("send", store.timeline(room).value.single().id)
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
    fun `live room twin without occupant mapping keeps the archived real author`() {
        val room = "room@muc.waddle.test"
        val occupant = "$room/sam"
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "mam-1",
                stanzaId = "room-1",
                stanzaIdBy = room,
                from = occupant,
                to = null,
                messageType = "groupchat",
                authorRealJid = "Sam.Old@waddle.test/web",
            ),
        )
        store.onLiveMessage(
            testMessage(
                stanzaId = "room-1",
                stanzaIdBy = room,
                from = occupant,
                to = null,
                messageType = "groupchat",
                isMuc = true,
            ),
            authorJid = null,
        )

        val items = store.timeline(room).value
        assertEquals(1, items.size)
        assertTrue(items[0].source is TimelineSource.Live)
        assertEquals("sam.old@waddle.test", items[0].authorJid)
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
