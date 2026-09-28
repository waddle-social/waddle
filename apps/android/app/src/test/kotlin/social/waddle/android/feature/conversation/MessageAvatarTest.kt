package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testArchivedMessage
import social.waddle.android.client.testMessage

/** Timeline avatar gutters: sender groups and author JID resolution. */
class MessageAvatarTest {
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

    private fun avatarOf(avatars: Map<String, MessageAvatar>, row: ConversationRow.Stored) =
        avatars[avatarKeyOf(row.item)]

    @Test
    fun `consecutive rows of one author within five minutes share one avatar`() {
        val stamp = "alice@waddle.test"
        val first = row("1", "alice", "2026-07-15T10:00:00Z", authorJid = stamp)
        val second = row("2", "alice", "2026-07-15T10:04:59Z", authorJid = stamp)
        val later = row("3", "alice", "2026-07-15T10:10:00Z", authorJid = stamp)
        val other = row("4", "bob", "2026-07-15T10:10:30Z")
        val avatars = messageAvatarsOf(listOf(first, second, later, other), self)

        assertEquals(MessageAvatar("alice@waddle.test", visible = true), avatarOf(avatars, first))
        assertEquals(MessageAvatar("alice@waddle.test", visible = false), avatarOf(avatars, second))
        assertTrue(avatarOf(avatars, later)!!.visible)
        // Unknown room author: initials, never a nick-derived guess.
        assertEquals(MessageAvatar(null, visible = true), avatarOf(avatars, other))
    }

    @Test
    fun `own rows get no gutter and break the previous group`() {
        val a1 = row("1", "alice")
        val mine = row("2", "me", mine = true)
        val a2 = row("3", "alice")
        val avatars = messageAvatarsOf(listOf(a1, mine, a2), self)

        assertNull(avatarOf(avatars, mine))
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
        val avatars = messageAvatarsOf(listOf(archived), self)

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
        val avatars = messageAvatarsOf(listOf(alice, bob), self)

        assertEquals(MessageAvatar("bob@waddle.test", visible = true), avatarOf(avatars, bob))
    }

    @Test
    fun `consecutive unknown-author rows from one nick share one avatar`() {
        val rows = (1..5).map { row("$it", "carol", "2026-07-15T10:0$it:00Z") }
        val avatars = messageAvatarsOf(rows, self)

        assertEquals(MessageAvatar(null, visible = true), avatarOf(avatars, rows.first()))
        rows.drop(1).forEach { assertEquals(MessageAvatar(null, visible = false), avatarOf(avatars, it)) }
    }

    @Test
    fun `an unknown and a known author under one nick split`() {
        val unknown = row("1", "dana", "2026-07-15T10:00:00Z")
        val known = row("2", "dana", "2026-07-15T10:01:00Z", authorJid = "dana@waddle.test")
        val unknownAgain = row("3", "dana", "2026-07-15T10:02:00Z")
        val avatars = messageAvatarsOf(listOf(unknown, known, unknownAgain), self)

        assertEquals(MessageAvatar("dana@waddle.test", visible = true), avatarOf(avatars, known))
        assertEquals(MessageAvatar(null, visible = true), avatarOf(avatars, unknownAgain))
    }
}
