package social.waddle.android.feature.conversation

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.store.MessageTombstone
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testMessage
import social.waddle.client.ffi.WaddleCallThreadAnchor

class ConversationRowsTest {
    private fun item(
        stanzaId: String,
        thread: String? = null,
        from: String? = "$ROOM_JID/alice",
        body: String = "hello",
        callSid: String? = null,
        tombstone: MessageTombstone? = null,
    ): TimelineItem {
        val message = testMessage(
            id = "id-$stanzaId",
            stanzaId = stanzaId,
            from = from,
            body = body,
            messageType = "groupchat",
            isMuc = true,
            thread = thread,
        ).copy(
            callThread = callSid?.let {
                WaddleCallThreadAnchor(
                    kind = "call",
                    sid = it,
                    media = listOf("audio"),
                    initiator = "$ROOM_JID/alice",
                    started = "2026-07-15T10:00:00Z",
                )
            },
        )
        return TimelineItem(
            id = stanzaId,
            conversationJid = ROOM_JID,
            from = from,
            body = body,
            timestamp = null,
            isMine = false,
            source = TimelineSource.Live(message),
            tombstone = tombstone,
        )
    }

    private fun pending(
        localId: Long,
        stanzaId: String? = null,
        threadId: String? = null,
    ) = PendingMessage(
        localId = localId,
        stanzaId = stanzaId,
        body = "pending",
        timestampMillis = 1_000L,
        failed = false,
        extras = threadId?.let { MessageSendExtras(threadId = it) },
    )

    @Test
    fun `feed shows roots and call anchors but hides thread replies`() {
        val items = listOf(
            item("s-root"),
            item("s-reply", thread = "s-root"),
            // Call anchors carry a thread id that is NOT one of their own
            // ids (the call sid) yet belong in the feed.
            item("s-call", thread = "call-sid", callSid = "call-sid"),
        )

        val rows = visibleRows(items, pending = emptyList(), threadId = null)

        assertEquals(
            listOf("s-root", "s-call"),
            rows.map { (it as ConversationRow.Stored).item.id },
        )
    }

    @Test
    fun `thread mode shows the root and its replies only`() {
        val items = listOf(
            item("s-root"),
            item("s-reply", thread = "s-root"),
            item("s-other"),
            item("s-foreign-reply", thread = "s-other"),
        )

        val rows = visibleRows(items, pending = emptyList(), threadId = "s-root")

        assertEquals(
            listOf("s-root", "s-reply"),
            rows.map { (it as ConversationRow.Stored).item.id },
        )
    }

    @Test
    fun `pending rows dedupe against every stored identity`() {
        // The MUC reflection is keyed by the room-assigned stanza id;
        // the send returned the client origin id.
        val echo = item("room-assigned-id").let { stored ->
            stored.copy(
                source = TimelineSource.Live(
                    (stored.source as TimelineSource.Live).message.copy(originId = "client-origin-id"),
                ),
            )
        }

        val rows = visibleRows(
            items = listOf(echo),
            pending = listOf(pending(localId = 0, stanzaId = "client-origin-id"), pending(localId = 1)),
            threadId = null,
        )

        assertEquals(2, rows.size)
        assertTrue(rows[0] is ConversationRow.Stored)
        assertEquals(1L, (rows[1] as ConversationRow.Unconfirmed).message.localId)
    }

    @Test
    fun `pending rows filter by the screen's thread`() {
        val feedPending = pending(localId = 0)
        val threadPending = pending(localId = 1, threadId = "t-1")

        val feedRows = visibleRows(emptyList(), listOf(feedPending, threadPending), threadId = null)
        assertEquals(
            listOf(0L),
            feedRows.map { (it as ConversationRow.Unconfirmed).message.localId },
        )

        val threadRows = visibleRows(emptyList(), listOf(feedPending, threadPending), threadId = "t-1")
        assertEquals(
            listOf(1L),
            threadRows.map { (it as ConversationRow.Unconfirmed).message.localId },
        )
    }

    @Test
    fun `reply counts group by thread and skip roots and call anchors`() {
        val items = listOf(
            item("s-root"),
            item("s-reply-1", thread = "s-root"),
            item("s-reply-2", thread = "s-root"),
            item("s-call", thread = "call-sid", callSid = "call-sid"),
        )

        assertEquals(mapOf("s-root" to 2), replyCountsOf(items))
    }

    @Test
    fun `thread summaries resolve the root by identity and order by activity`() {
        val items = listOf(
            item("s-a", body = "first root", from = "$ROOM_JID/alice"),
            item("s-b", body = "second root"),
            item("s-a-reply", thread = "s-a"),
            item("s-b-reply", thread = "s-b"),
            item("s-a-reply-2", thread = "s-a"),
        )

        val summaries = threadSummariesOf(items) { it.from?.substringAfter('/') }

        // Newest activity first BY TIMELINE ORDER.
        assertEquals(listOf("s-a", "s-b"), summaries.map { it.threadId })
        val threadA = summaries.first()
        assertEquals("alice", threadA.rootAuthor)
        assertEquals("first root", threadA.rootPreview)
        assertEquals(2, threadA.replyCount)
    }

    @Test
    fun `tombstoned roots never leak their body through previews`() {
        val items = listOf(
            item("s-root", body = "secret", tombstone = MessageTombstone.Retracted),
            item("s-reply", thread = "s-root"),
        )

        val summary = threadSummariesOf(items) { null }.single()

        assertEquals("", summary.rootPreview)
        assertTrue(summary.rootTombstoned)
    }

    private companion object {
        const val ROOM_JID = "general@muc.waddle.test"
    }
}
