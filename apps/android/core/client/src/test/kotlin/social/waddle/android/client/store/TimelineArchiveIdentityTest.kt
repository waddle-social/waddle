package social.waddle.android.client.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage

class TimelineArchiveIdentityTest {
    private val room = "room@muc.waddle.test"
    private val store = TimelineStore().apply { setOwnBareJid("me@waddle.test") }

    @Test
    fun `anonymous room archive replay reconciles by its opaque archive uid`() {
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "Archive UID", id = "first-wire", from = "$room/old-nick", to = null,
                messageType = "groupchat", body = "original",
            ),
        )
        val original = store.timeline(room).value.single()
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "Archive UID", id = "replay-wire", from = "$room/new-nick", to = null,
                messageType = "groupchat", body = "replay",
            ),
        )

        assertEquals(listOf(original), store.timeline(room).value)
    }

    @Test
    fun `empty archive uids cannot reconcile anonymous sender aliases`() {
        repeat(2) { index ->
            store.onArchivedMessage(
                testArchivedMessage(
                    mamId = "", id = "reused", originId = "reused", from = "$room/sam", to = null,
                    messageType = "groupchat", body = "message-$index",
                ),
            )
        }

        assertEquals(listOf("message-0", "message-1"), store.timeline(room).value.map { it.body })
    }

    @Test
    fun `archive uids preserve case and whitespace as opaque nonempty strings`() {
        val uids = listOf("UID", "uid", " UID", "UID ", " ", "  ")
        uids.forEach { uid ->
            store.onArchivedMessage(
                testArchivedMessage(
                    mamId = uid, id = "reused", from = "$room/sam", to = null,
                    messageType = "groupchat", body = uid,
                ),
            )
        }
        val originals = store.timeline(room).value
        uids.forEach { uid ->
            store.onArchivedMessage(
                testArchivedMessage(
                    mamId = uid, id = "different-wire", from = "$room/sam", to = null,
                    messageType = "groupchat", body = "replay",
                ),
            )
        }

        assertEquals(uids, originals.map { it.body })
        assertEquals(originals, store.timeline(room).value)
    }

    @Test
    fun `the same archive uid in different rooms identifies distinct messages`() {
        val otherRoom = "other@muc.waddle.test"
        for (conversation in listOf(room, otherRoom)) {
            store.onArchivedMessage(
                testArchivedMessage(
                    mamId = "same-uid", id = "same-wire", from = "$conversation/sam", to = null,
                    messageType = "groupchat", body = conversation,
                ),
            )
        }

        assertEquals(room, store.timeline(room).value.single().body)
        assertEquals(otherRoom, store.timeline(otherRoom).value.single().body)
    }

    @Test
    fun `sender controlled wire ids cannot impersonate an archive uid`() {
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "archive-only", id = "original-wire", from = "$room/sam", to = null,
                messageType = "groupchat", body = "original",
            ),
        )
        assertTrue(
            store.onLiveMessage(
                testMessage(
                    id = "archive-only", originId = "archive-only", from = "$room/sam", to = null,
                    messageType = "groupchat", body = "forged live",
                ),
            ),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "different-uid", id = "archive-only", originId = "archive-only", from = "$room/sam",
                to = null, messageType = "groupchat", body = "forged archive",
            ),
        )

        assertEquals(
            listOf("original", "forged archive", "forged live"),
            store.timeline(room).value.map { it.body },
        )
    }

    @Test
    fun `the same archive uid cannot override conflicting room assigned ids`() {
        for (id in listOf("room-1", "room-2")) {
            store.onArchivedMessage(
                testArchivedMessage(
                    mamId = "contradictory-uid", id = "alias", originId = "alias", stanzaId = id,
                    stanzaIdBy = room, from = "$room/sam", to = null, messageType = "groupchat", body = id,
                    authorRealJid = "sam@waddle.test",
                ),
            )
        }

        assertEquals(listOf("room-1", "room-2"), store.timeline(room).value.map { it.body })
    }

    @Test
    fun `ambiguous archive uids fail closed but unique room identity still wins`() {
        for (id in listOf("room-1", "room-2")) {
            store.onArchivedMessage(
                testArchivedMessage(
                    mamId = "contradictory-uid", stanzaId = id, stanzaIdBy = room, from = "$room/sam",
                    to = null, messageType = "groupchat", body = id,
                ),
            )
        }
        val originals = store.timeline(room).value
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "contradictory-uid", stanzaId = "room-1", stanzaIdBy = room, from = "$room/renamed",
                to = null, messageType = "groupchat", body = "canonical replay",
            ),
        )
        assertEquals(originals, store.timeline(room).value)
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "contradictory-uid", id = "unknown-wire", from = "$room/sam", to = null,
                messageType = "groupchat", body = "unresolved",
            ),
        )

        assertEquals(listOf("room-1", "room-2", "unresolved"), store.timeline(room).value.map { it.body })
    }

    @Test
    fun `canonical room identity wins over a different row sharing the incoming archive uid`() {
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "incoming-uid", id = "earlier-wire", from = "$room/sam", to = null,
                messageType = "groupchat", body = "earlier",
            ),
        )
        store.onLiveMessage(
            testMessage(
                id = "canonical-wire", stanzaId = "room-id", stanzaIdBy = room, from = "$room/sam",
                to = null, messageType = "groupchat", body = "canonical",
            ),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "incoming-uid", stanzaId = "room-id", stanzaIdBy = room, from = "$room/renamed",
                to = null, messageType = "groupchat", body = "canonical archive",
            ),
        )

        val rows = store.timeline(room).value
        assertEquals(listOf("earlier", "canonical"), rows.map { it.body })
        assertTrue(rows.single { it.body == "canonical" }.source is TimelineSource.Live)
    }

    @Test
    fun `archive uid survives a live source upgrade without entering wire identities`() {
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "archive-only", id = "wire", originId = "origin", stanzaId = "room-id",
                stanzaIdBy = room, from = "$room/sam", to = null, messageType = "groupchat",
            ),
        )
        val presentationId = store.timeline(room).value.single().presentationId
        assertFalse(
            store.onLiveMessage(
                testMessage(
                    id = "wire", originId = "origin", stanzaId = "room-id", stanzaIdBy = room,
                    from = "$room/sam", to = null, messageType = "groupchat", body = "live body",
                ),
            ),
        )
        val upgraded = store.timeline(room).value.single()
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "archive-only", id = "different-wire", from = "$room/renamed", to = null,
                messageType = "groupchat", body = "uid replay",
            ),
        )

        assertEquals(listOf(upgraded), store.timeline(room).value)
        assertEquals(presentationId, upgraded.presentationId)
        assertEquals("room-id", upgraded.id)
        assertEquals(setOf("wire", "origin", "room-id"), upgraded.identityIds)
        assertTrue(upgraded.source is TimelineSource.Live)
    }

    @Test
    fun `live twins learn the archive uid even without any visible metadata change`() {
        store.onLiveMessage(
            testMessage(
                id = "wire", originId = "origin", timestamp = "2026-07-15T10:00:00Z", from = "$room/sam",
                to = null, messageType = "groupchat",
            ),
            authorJid = "sam@waddle.test",
        )
        val original = store.timeline(room).value.single()
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "learned-uid", id = "wire", originId = "origin", from = "$room/sam", to = null,
                messageType = "groupchat", authorRealJid = "sam@waddle.test",
            ),
        )
        assertEquals(listOf(original), store.timeline(room).value)
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "learned-uid", id = "different-wire", from = "$room/renamed", to = null,
                messageType = "groupchat",
            ),
        )

        assertEquals(listOf(original), store.timeline(room).value)
        assertEquals(setOf("wire", "origin"), original.identityIds)
    }

    @Test
    fun `a live row remembers its first nonempty archive uid across later proven twins`() {
        store.onLiveMessage(
            testMessage(
                id = "wire", originId = "origin", timestamp = "2026-07-15T10:00:00Z", from = "$room/sam",
                to = null, messageType = "groupchat", body = "live",
            ),
            authorJid = "sam@waddle.test",
        )
        val original = store.timeline(room).value.single()
        for (uid in listOf("", "first-uid", "later-uid")) {
            store.onArchivedMessage(
                testArchivedMessage(
                    mamId = uid, id = "wire", originId = "origin", from = "$room/sam", to = null,
                    messageType = "groupchat", authorRealJid = "sam@waddle.test",
                ),
            )
        }
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "first-uid", id = "first-replay", from = "$room/renamed", to = null,
                messageType = "groupchat", body = "first replay",
            ),
        )
        assertEquals(listOf(original), store.timeline(room).value)
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "later-uid", id = "later-replay", from = "$room/renamed", to = null,
                messageType = "groupchat", body = "unproven later uid",
            ),
        )

        assertEquals(listOf("live", "unproven later uid"), store.timeline(room).value.map { it.body })
    }

    @Test
    fun `direct messages retain sender continuity despite matching archive uids`() {
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "same-uid", id = "same-wire", from = "alice@waddle.test", to = "me@waddle.test",
                body = "peer",
            ),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "same-uid", id = "same-wire", from = "me@waddle.test", to = "alice@waddle.test",
                body = "mine",
            ),
        )

        assertEquals(listOf("peer", "mine"), store.timeline("alice@waddle.test").value.map { it.body })
    }

    @Test
    fun `personal archive uid from a room bare jid cannot suppress a room archive row`() {
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "42", id = "direct-wire", from = room, to = "me@waddle.test",
                messageType = "chat", body = "personal archive", mucUser = false,
            ),
        )
        store.onArchivedMessage(
            testArchivedMessage(
                mamId = "42", id = "room-wire", from = "$room/sam", to = null,
                messageType = "groupchat", body = "room archive", mucUser = false,
            ),
        )

        assertEquals(listOf("personal archive", "room archive"), store.timeline(room).value.map { it.body })
    }
}
