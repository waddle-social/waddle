package social.waddle.android.feature.conversation

import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.font.FontWeight
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testMessage
import social.waddle.android.feature.search.highlighted
import java.time.Instant
import java.time.ZoneOffset

/** The thread chip's faces, count and recency, and the search match highlight. */
class ThreadActivityTest {
    private val room = "room@muc.waddle.test"

    private fun item(id: String, nick: String, at: String, thread: String? = null, jid: String? = null) = TimelineItem(
        id = id,
        conversationJid = room,
        from = "$room/$nick",
        body = "hi",
        timestamp = at,
        isMine = false,
        source = TimelineSource.Live(
            testMessage(
                id = id,
                stanzaId = id,
                stanzaIdBy = room,
                from = "$room/$nick",
                messageType = "groupchat",
                isMuc = true,
                thread = thread,
                timestamp = at,
            ),
        ),
        authorJid = jid,
    )

    @Test
    fun `repliers are deduped, most recent first, with the last reply time`() {
        val root = item("root", "alice", "2026-10-16T09:00:00Z")
        val items = listOf(
            root,
            item("r1", "bob", "2026-10-16T09:01:00Z", thread = "root", jid = "bob@waddle.test"),
            item("r2", "carol", "2026-10-16T09:02:00Z", thread = "root", jid = "carol@waddle.test"),
            item("r3", "bob", "2026-10-16T09:03:00Z", thread = "root", jid = "bob@waddle.test"),
        )
        val activity = threadActivityOf(items, selfBareJid = null).getValue("root")

        assertEquals(3, activity.replyCount)
        assertEquals(listOf("bob", "carol"), activity.participants.map { it.name })
        assertEquals(0, activity.moreParticipants)
        assertEquals(Instant.parse("2026-10-16T09:03:00Z"), activity.lastReplyAt)
    }

    @Test
    fun `more than five repliers collapse into a count`() {
        val replies = (1..7).map { n -> item("r$n", "p$n", "2026-10-16T09:0$n:00Z", thread = "root") }
        val activity = threadActivityOf(listOf(item("root", "alice", "2026-10-16T09:00:00Z")) + replies, null)
            .getValue("root")

        assertEquals(MAX_THREAD_FACES, activity.participants.size)
        assertEquals(2, activity.moreParticipants)
        assertEquals("p7", activity.participants.first().name)
    }

    @Test
    fun `feed roots carry their thread activity`() {
        val root = item("root", "alice", "2026-10-16T09:00:00Z")
        val reply = item("r1", "bob", "2026-10-16T09:01:00Z", thread = "root")
        val entries = feedEntriesOf(
            items = listOf(root, reply),
            pending = emptyList(),
            context = FeedContext(
                threadId = null,
                selfBareJid = null,
                pinnedIds = emptySet(),
                unreadAtOpen = 0,
                zone = ZoneOffset.UTC,
            ),
        )
        val rootEntry = entries.single()

        assertEquals(1, rootEntry.replyCount)
        assertEquals(listOf("bob"), rootEntry.threadActivity?.participants?.map { it.name })
    }

    @Test
    fun `a message without replies has no thread activity`() {
        assertNull(threadActivityOf(listOf(item("solo", "alice", "2026-10-16T09:00:00Z")), null)["solo"])
    }

    @Test
    fun `search highlights every case-insensitive match`() {
        val style = SpanStyle(fontWeight = FontWeight.SemiBold)
        val text = highlighted("MAM latency and mam pages", "mam", style)

        assertEquals("MAM latency and mam pages", text.text)
        assertEquals(listOf(0 to 3, 16 to 19), text.spanStyles.map { it.start to it.end })
    }

    @Test
    fun `a blank query highlights nothing`() {
        assertEquals(0, highlighted("anything", "  ", SpanStyle()).spanStyles.size)
    }
}
