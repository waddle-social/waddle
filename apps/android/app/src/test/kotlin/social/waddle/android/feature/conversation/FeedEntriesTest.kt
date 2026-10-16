package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneOffset

/** Feed derivation: author groups, avatars, separators and dividers. */
class FeedEntriesTest {
    private val room = "room@muc.waddle.test"
    private val self = "me@waddle.test"

    private fun row(
        id: String,
        nick: String,
        timestamp: String? = "2026-07-15T10:00:00Z",
        mine: Boolean = false,
        authorJid: String? = null,
    ) = ConversationRow.Stored(
        TimelineItem(
            id = id,
            conversationJid = room,
            from = "$room/$nick",
            body = "hi",
            timestamp = timestamp,
            isMine = mine,
            source = TimelineSource.Live(
                testMessage(id = id, from = "$room/$nick", messageType = "groupchat", isMuc = true),
            ),
            authorJid = authorJid,
        ),
    )

    /** (author JID, starts a group) for [row] — the avatar the row shows, if any. */
    private data class Gutter(val jid: String?, val visible: Boolean)

    private fun gutters(rows: List<ConversationRow.Stored>, unreadAtOpen: Int = 0): Map<String, Gutter> =
        feedEntriesOf(
            items = rows.map { it.item },
            pending = emptyList(),
            context = FeedContext(
                threadId = null,
                selfBareJid = self,
                pinnedIds = emptySet(),
                unreadAtOpen = unreadAtOpen,
                zone = ZoneOffset.UTC,
            ),
        ).associate { it.key to Gutter(it.authorJid, it.startsGroup) }

    private fun avatarOf(avatars: Map<String, Gutter>, row: ConversationRow.Stored) = avatars[rowKeyOf(row)]

    @Test
    fun `sender id collisions under one nick retain independent avatar rows`() {
        val earlier = row("reused", "sam")
        val later = row("reused", "sam", authorJid = "later@waddle.test")
        val avatars = gutters(listOf(earlier, later))

        assertEquals(2, avatars.size)
        assertEquals(Gutter(null, visible = true), avatarOf(avatars, earlier))
        assertEquals(Gutter("later@waddle.test", visible = true), avatarOf(avatars, later))
    }

    @Test
    fun `ambiguous quoted aliases resolve no loaded author`() {
        val earlier = row("reused", "sam")
        val later = row("reused", "sam", authorJid = "later@waddle.test")

        assertNull(quotedMessagesByIdentity(listOf(earlier, later))["reused"])
    }

    @Test
    fun `room assigned quote identity wins over another rows sender chosen alias`() {
        val canonical = ConversationRow.Stored(
            row("wire", "sam").item.copy(
                source = TimelineSource.Live(
                    testMessage(
                        id = "wire", stanzaId = "room-id", stanzaIdBy = room,
                        from = "$room/sam", to = null, messageType = "groupchat",
                    ),
                ),
            ),
        )
        val conflicting = row("room-id", "sam", authorJid = "later@waddle.test")

        assertEquals(canonical.item, quotedMessagesByIdentity(listOf(canonical, conflicting))["room-id"])
    }

    @Test
    fun `consecutive rows of one author within five minutes share one avatar`() {
        val stamp = "alice@waddle.test"
        val first = row("1", "alice", "2026-07-15T10:00:00Z", authorJid = stamp)
        val second = row("2", "alice", "2026-07-15T10:04:59Z", authorJid = stamp)
        val later = row("3", "alice", "2026-07-15T10:10:00Z", authorJid = stamp)
        val other = row("4", "bob", "2026-07-15T10:10:30Z")
        val avatars = gutters(listOf(first, second, later, other))

        assertEquals(Gutter("alice@waddle.test", visible = true), avatarOf(avatars, first))
        assertEquals(Gutter("alice@waddle.test", visible = false), avatarOf(avatars, second))
        assertTrue(avatarOf(avatars, later)!!.visible)
        // Unknown room author: initials, never a nick-derived guess.
        assertEquals(Gutter(null, visible = true), avatarOf(avatars, other))
    }

    @Test
    fun `own rows sit on the same edge with the account avatar and break the previous group`() {
        val a1 = row("1", "alice")
        val mine = row("2", "me", mine = true)
        val a2 = row("3", "alice")
        val avatars = gutters(listOf(a1, mine, a2))

        // A room row is attributed by its stamp only; unstamped, it gets initials.
        assertEquals(Gutter(null, visible = true), avatarOf(avatars, mine))
        assertTrue(avatarOf(avatars, a2)!!.visible)
    }

    @Test
    fun `a departed author keeps the JID stamped on their row`() {
        val archived = ConversationRow.Stored(
            TimelineItem(
                id = "m1",
                conversationJid = room,
                from = "$room/gone",
                body = "old",
                timestamp = "2026-07-14T09:00:00Z",
                isMine = false,
                source = TimelineSource.Archived(
                    testArchivedMessage(
                        from = "$room/gone",
                        messageType = "groupchat",
                        authorRealJid = "gone@waddle.test/web",
                    ),
                ),
                authorJid = "gone@waddle.test",
            ),
        )
        val avatars = gutters(listOf(archived))

        assertEquals("gone@waddle.test", avatarOf(avatars, archived)?.jid)
    }

    @Test
    fun `a room quote resolves only from the loaded original`() {
        val original = row("1", "alice", authorJid = "alice@waddle.test")
        fun reply(to: String?) = row("2", "bob").item.copy(
            source = TimelineSource.Live(
                testMessage(id = "2", from = "$room/bob", messageType = "groupchat", isMuc = true)
                    .copy(replyToId = "1", replyToSender = to),
            ),
        )

        assertEquals("alice@waddle.test", quotedAuthorJidOf(reply("$room/alice"), original.item, self))
        // Original not loaded: the sender's `to` claim is never trusted in a room.
        assertNull(quotedAuthorJidOf(reply("$room/alice"), null, self))
        assertNull(quotedAuthorJidOf(reply("Dave@Waddle.test/x"), null, self))
        assertNull(quotedAuthorJidOf(reply(null), null, self))
    }

    @Test
    fun `a 1-1 quote target is trusted only when it names a participant`() {
        val peer = "peer@waddle.test"
        fun reply(to: String?) = TimelineItem(
            id = "d2",
            conversationJid = peer,
            from = "$peer/phone",
            body = "re",
            timestamp = null,
            isMine = false,
            source = TimelineSource.Live(
                testMessage(id = "d2", from = "$peer/phone").copy(replyToId = "d1", replyToSender = to),
            ),
        )

        assertEquals(self, quotedAuthorJidOf(reply("Me@Waddle.test/laptop"), null, self))
        assertEquals(peer, quotedAuthorJidOf(reply("$peer/tablet"), null, self))
        // A third party named by the sender gets initials, not their face.
        assertNull(quotedAuthorJidOf(reply("mallory@evil.test"), null, self))
        assertNull(quotedAuthorJidOf(reply(null), null, self))
    }

    @Test
    fun `differing resolved authors split a group even under one nick`() {
        val alice = row("1", "bob", "2026-07-15T10:00:00Z", authorJid = "alice@waddle.test")
        // Alice wrote as "bob"; the nick then passed to Bob within 5 min.
        val bob = row("2", "bob", "2026-07-15T10:01:00Z", authorJid = "bob@waddle.test")
        val avatars = gutters(listOf(alice, bob))

        assertEquals(Gutter("bob@waddle.test", visible = true), avatarOf(avatars, bob))
    }

    @Test
    fun `consecutive unknown-author rows from one nick share one avatar`() {
        val rows = (1..5).map { row("$it", "carol", "2026-07-15T10:0$it:00Z") }
        val avatars = gutters(rows)

        assertEquals(Gutter(null, visible = true), avatarOf(avatars, rows.first()))
        rows.drop(1).forEach { assertEquals(Gutter(null, visible = false), avatarOf(avatars, it)) }
    }

    @Test
    fun `an unknown and a known author under one nick split`() {
        val unknown = row("1", "dana", "2026-07-15T10:00:00Z")
        val known = row("2", "dana", "2026-07-15T10:01:00Z", authorJid = "dana@waddle.test")
        val unknownAgain = row("3", "dana", "2026-07-15T10:02:00Z")
        val avatars = gutters(listOf(unknown, known, unknownAgain))

        assertEquals(Gutter("dana@waddle.test", visible = true), avatarOf(avatars, known))
        assertEquals(Gutter(null, visible = true), avatarOf(avatars, unknownAgain))
    }

    @Test
    fun `a row matching our nick but stamped with another JID gets their avatar`() {
        val theirs = row("1", "me", mine = true, authorJid = "bob@waddle.test")
        val ours = row("2", "me", mine = true, authorJid = self)
        val avatars = gutters(listOf(theirs, ours))

        assertEquals(Gutter("bob@waddle.test", visible = true), avatarOf(avatars, theirs))
        assertEquals(Gutter(self, visible = true), avatarOf(avatars, ours))
    }

    @Test
    fun `a new day starts a group and carries a separator`() {
        val late = row("1", "alice", "2026-07-15T23:58:00Z", authorJid = "alice@waddle.test")
        val early = row("2", "alice", "2026-07-16T00:01:00Z", authorJid = "alice@waddle.test")
        val entries = feedEntriesOf(
            items = listOf(late.item, early.item),
            pending = emptyList(),
            context = FeedContext(null, self, emptySet(), unreadAtOpen = 0, zone = ZoneOffset.UTC),
        )

        assertEquals(LocalDate.of(2026, 7, 15), entries[0].daySeparator)
        assertEquals(LocalDate.of(2026, 7, 16), entries[1].daySeparator)
        assertTrue("a day boundary always restarts the header", entries[1].startsGroup)
    }

    @Test
    fun `the unread divider sits above the oldest unread incoming row and splits the group`() {
        val a1 = row("1", "alice", "2026-07-15T10:00:00Z", authorJid = "alice@waddle.test")
        val a2 = row("2", "alice", "2026-07-15T10:01:00Z", authorJid = "alice@waddle.test")
        val mine = row("3", "me", "2026-07-15T10:02:00Z", mine = true, authorJid = self)
        val a3 = row("4", "alice", "2026-07-15T10:03:00Z", authorJid = "alice@waddle.test")
        val entries = feedEntriesOf(
            items = listOf(a1, a2, mine, a3).map { it.item },
            pending = emptyList(),
            context = FeedContext(null, self, emptySet(), unreadAtOpen = 2, zone = ZoneOffset.UTC),
        )

        assertEquals(listOf(false, true, false, false), entries.map { it.unreadDivider })
        assertTrue(entries[1].startsGroup)
    }

    @Test
    fun `new messages while the screen is open do not move the unread divider`() {
        val a1 = row("1", "alice", "2026-07-15T10:00:00Z", authorJid = "alice@waddle.test")
        val a2 = row("2", "alice", "2026-07-15T10:01:00Z", authorJid = "alice@waddle.test")
        val a3 = row("3", "alice", "2026-07-15T10:02:00Z", authorJid = "alice@waddle.test")
        val anchor = UnreadAnchor(unreadAtOpen = 2, openedAt = Instant.parse("2026-07-15T10:02:30Z"))
        val context = FeedContext(
            threadId = null,
            selfBareJid = self,
            pinnedIds = emptySet(),
            unreadAtOpen = 2,
            zone = ZoneOffset.UTC,
            unreadAnchor = anchor,
        )
        val atOpen = feedEntriesOf(listOf(a1, a2, a3).map { it.item }, emptyList(), context)
        assertEquals(listOf(false, true, false), atOpen.map { it.unreadDivider })

        val a4 = row("4", "alice", "2026-07-15T10:03:00Z", authorJid = "alice@waddle.test")
        val later = feedEntriesOf(listOf(a1, a2, a3, a4).map { it.item }, emptyList(), context)

        assertEquals(
            "the divider stays above the first message that was unread at open",
            listOf(false, true, false, false),
            later.map { it.unreadDivider },
        )
    }

    @Test
    fun `the unread divider waits for the newest history before pinning`() {
        val a1 = row("1", "alice", "2026-07-15T10:00:00Z", authorJid = "alice@waddle.test")
        val a2 = row("2", "alice", "2026-07-15T10:01:00Z", authorJid = "alice@waddle.test")
        val a3 = row("3", "alice", "2026-07-15T10:02:00Z", authorJid = "alice@waddle.test")
        val anchor = UnreadAnchor(unreadAtOpen = 1, openedAt = Instant.parse("2026-07-15T10:02:30Z"))
        val loading = FeedContext(
            threadId = null,
            selfBareJid = self,
            pinnedIds = emptySet(),
            unreadAtOpen = 1,
            zone = ZoneOffset.UTC,
            unreadAnchor = anchor,
            newestHistoryLoaded = false,
        )
        // Stale cache: the one unread message (3) has not been fetched yet.
        feedEntriesOf(listOf(a1, a2).map { it.item }, emptyList(), loading)

        val loaded = loading.copy(newestHistoryLoaded = true)
        val hydrated = feedEntriesOf(listOf(a1, a2, a3).map { it.item }, emptyList(), loaded)
        assertEquals(
            "the divider lands on the unread message the history brought in",
            listOf(false, false, true),
            hydrated.map { it.unreadDivider },
        )

        val a4 = row("4", "alice", "2026-07-15T10:03:00Z", authorJid = "alice@waddle.test")
        val later = feedEntriesOf(listOf(a1, a2, a3, a4).map { it.item }, emptyList(), loaded)
        assertEquals(listOf(false, false, true, false), later.map { it.unreadDivider })
    }

    @Test
    fun `a message arriving before history loads does not shift the unread divider`() {
        val a1 = row("1", "alice", "2026-07-15T10:00:00Z", authorJid = "alice@waddle.test")
        val a2 = row("2", "alice", "2026-07-15T10:01:00Z", authorJid = "alice@waddle.test")
        val a3 = row("3", "alice", "2026-07-15T10:02:00Z", authorJid = "alice@waddle.test")
        val anchor = UnreadAnchor(unreadAtOpen = 2, openedAt = Instant.parse("2026-07-15T10:02:30Z"))
        val loading = FeedContext(
            threadId = null,
            selfBareJid = self,
            pinnedIds = emptySet(),
            unreadAtOpen = 2,
            zone = ZoneOffset.UTC,
            unreadAnchor = anchor,
            newestHistoryLoaded = false,
        )
        // Sent after the screen opened, delivered while history is still loading.
        val a4 = row("4", "alice", "2026-07-15T10:03:00Z", authorJid = "alice@waddle.test")
        feedEntriesOf(listOf(a1, a2, a3, a4).map { it.item }, emptyList(), loading)

        val loaded = feedEntriesOf(
            listOf(a1, a2, a3, a4).map { it.item },
            emptyList(),
            loading.copy(newestHistoryLoaded = true),
        )

        assertEquals(
            "the divider stays above the first message unread at open",
            listOf(false, true, false, false),
            loaded.map { it.unreadDivider },
        )
    }

    @Test
    fun `undelayed live rows group by local receipt time`() {
        val first = row("1", "alice", timestamp = null, authorJid = "alice@waddle.test")
        val second = row("2", "alice", timestamp = null, authorJid = "alice@waddle.test")
        val avatars = gutters(listOf(first, second))

        assertTrue(avatarOf(avatars, first)!!.visible)
        assertEquals(false, avatarOf(avatars, second)!!.visible)
    }
}
