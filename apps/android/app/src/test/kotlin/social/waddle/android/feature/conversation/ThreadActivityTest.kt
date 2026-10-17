package social.waddle.android.feature.conversation

import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.font.FontWeight
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import social.waddle.android.client.store.MessageTombstone
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testMessage
import social.waddle.android.feature.search.highlighted
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleSharedFile
import java.time.Instant
import java.time.ZoneOffset

/** The reply card's faces, count, recency and latest reply, and the search match highlight. */
class ThreadActivityTest {
    private val room = "room@muc.waddle.test"

    private fun message(id: String, nick: String, at: String, thread: String? = null) = testMessage(
        id = id,
        stanzaId = id,
        stanzaIdBy = room,
        from = "$room/$nick",
        body = "hi",
        messageType = "groupchat",
        isMuc = true,
        thread = thread,
        timestamp = at,
    )

    private fun itemOf(
        message: WaddleMessage,
        jid: String? = null,
        tombstone: MessageTombstone? = null,
    ) = TimelineItem(
        id = checkNotNull(message.id),
        conversationJid = room,
        from = message.from,
        body = message.body.orEmpty(),
        timestamp = message.timestamp,
        isMine = false,
        source = TimelineSource.Live(message),
        tombstone = tombstone,
        authorJid = jid,
    )

    private fun item(id: String, nick: String, at: String, thread: String? = null, jid: String? = null) =
        itemOf(message(id, nick, at, thread), jid)

    private fun file(mediaType: String, url: String = "https://upload.waddle.test/f") = WaddleSharedFile(
        url = url,
        name = null,
        mediaType = mediaType,
        size = null,
        width = null,
        height = null,
        desc = null,
        hashes = emptyList(),
        disposition = "attachment",
        encrypted = null,
    )

    private fun reply(body: String = "hi", files: List<WaddleSharedFile> = emptyList(), isSticker: Boolean = false) =
        itemOf(
            message("r", "bob", "2026-10-16T09:01:00Z", thread = "root")
                .copy(body = body, sharedFiles = files, isSticker = isSticker),
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
        assertEquals(
            LatestReply("bob", ReplySnippet.Text("hi"), Instant.parse("2026-10-16T09:03:00Z")),
            activity.latestReply,
        )
    }

    @Test
    fun `the latest reply is the newest by wire time, not arrival`() {
        val items = listOf(
            item("root", "alice", "2026-10-16T09:00:00Z"),
            itemOf(message("r1", "bob", "2026-10-16T09:05:00Z", thread = "root").copy(body = "late page, newer")),
            // An older archive page landing after a newer live reply.
            itemOf(message("r2", "carol", "2026-10-16T09:02:00Z", thread = "root").copy(body = "older")),
        )
        val latest = threadActivityOf(items, selfBareJid = null).getValue("root").latestReply

        assertEquals("bob", latest?.author)
        assertEquals(ReplySnippet.Text("late page, newer"), latest?.snippet)
    }

    @Test
    fun `a snippet is one plain line`() {
        assertEquals(
            ReplySnippet.Text("Love this, watching now"),
            replySnippetOf(reply(body = "  Love this,\n\n\twatching   now \n")),
        )
    }

    @Test
    fun `media-only replies read as a photo or an attachment`() {
        val image = file("image/png")
        val csv = file("text/csv", url = "https://upload.waddle.test/c")
        assertEquals(ReplySnippet.Photo, replySnippetOf(reply(body = "", files = listOf(image))))
        assertEquals(ReplySnippet.Photo, replySnippetOf(reply(body = image.url, files = listOf(image))))
        assertEquals(ReplySnippet.Photo, replySnippetOf(reply(body = "https://media.giphy.com/media/x/giphy.gif")))
        val sticker = reply(body = "waving penguin", files = listOf(image), isSticker = true)
        assertEquals(ReplySnippet.Photo, replySnippetOf(sticker))
        assertEquals(
            ReplySnippet.Attachment,
            replySnippetOf(reply(body = "", files = listOf(image, csv))),
        )
    }

    @Test
    fun `a reply with nothing to quote leaves the card without a latest line`() {
        val blank = reply(body = "   ")
        assertNull(replySnippetOf(blank))

        val items = listOf(item("root", "alice", "2026-10-16T09:00:00Z"), blank)
        val activity = threadActivityOf(items, selfBareJid = null).getValue("root")
        assertEquals(1, activity.replyCount)
        assertNull(activity.latestReply)
    }

    @Test
    fun `faces and the latest reply follow wire time, not arrival`() {
        val items = listOf(
            item("root", "alice", "2026-10-16T09:00:00Z"),
            item("r1", "bob", "2026-10-16T09:05:00Z", thread = "root"),
            item("r2", "carol", "2026-10-16T09:02:00Z", thread = "root"),
        )
        val activity = threadActivityOf(items, selfBareJid = null).getValue("root")

        assertEquals(listOf("bob", "carol"), activity.participants.map { it.name })
        assertEquals("bob", activity.latestReply?.author)
    }

    @Test
    fun `a sticker without its file still reads as a photo`() {
        assertEquals(ReplySnippet.Photo, replySnippetOf(reply(body = "waving penguin", isSticker = true)))
    }

    @Test
    fun `a captioned file quotes its caption`() {
        assertEquals(
            ReplySnippet.Text("the export"),
            replySnippetOf(reply(body = "the export", files = listOf(file("text/csv")))),
        )
    }

    @Test
    fun `a retracted reply reads as deleted`() {
        val retracted = itemOf(message("r", "bob", "2026-10-16T09:01:00Z"), tombstone = MessageTombstone.Retracted)
        assertEquals(ReplySnippet.Deleted, replySnippetOf(retracted))
    }

    @Test
    fun `more than three repliers collapse into a count`() {
        val replies = (1..7).map { n -> item("r$n", "p$n", "2026-10-16T09:0$n:00Z", thread = "root") }
        val activity = threadActivityOf(listOf(item("root", "alice", "2026-10-16T09:00:00Z")) + replies, null)
            .getValue("root")

        assertEquals(MAX_THREAD_FACES, activity.participants.size)
        assertEquals(4, activity.moreParticipants)
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
