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
    fun `a quoted author resolves from the original, else only a real-JID reply target`() {
        val original = row("1", "alice", authorJid = "alice@waddle.test")
        fun reply(to: String?) = row("2", "bob").item.copy(
            source = TimelineSource.Live(
                testMessage(id = "2", from = "$room/bob", messageType = "groupchat", isMuc = true)
                    .copy(replyToId = "1", replyToSender = to),
            ),
        )

        assertEquals("alice@waddle.test", quotedAuthorJidOf(reply("$room/alice"), original.item, self))
        // Original not loaded: whoever holds that nick now may not have written it.
        assertNull(quotedAuthorJidOf(reply("$room/alice"), null, self))
        // A real JID target is used as-is (normalized).
        assertEquals("dave@waddle.test", quotedAuthorJidOf(reply("Dave@Waddle.test/x"), null, self))
        assertNull(quotedAuthorJidOf(reply(null), null, self))
    }

    @Test
    fun `a reused nick or an unknown author never continues the previous group`() {
        val alice = row("1", "bob", "2026-07-15T10:00:00Z", authorJid = "alice@waddle.test")
        // Alice wrote as "bob"; the nick then passed to Bob within 5 min.
        val bob = row("2", "bob", "2026-07-15T10:01:00Z", authorJid = "bob@waddle.test")
        val unknown1 = row("3", "carol", "2026-07-15T10:02:00Z")
        val unknown2 = row("4", "carol", "2026-07-15T10:03:00Z")
        val avatars = messageAvatarsOf(listOf(alice, bob, unknown1, unknown2), self)

        assertEquals(MessageAvatar("bob@waddle.test", visible = true), avatarOf(avatars, bob))
        assertEquals(MessageAvatar(null, visible = true), avatarOf(avatars, unknown1))
        assertEquals(MessageAvatar(null, visible = true), avatarOf(avatars, unknown2))
    }
}
