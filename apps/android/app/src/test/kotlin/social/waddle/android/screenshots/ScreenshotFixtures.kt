package social.waddle.android.screenshots

import social.waddle.android.client.store.ReactionGroup
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.testMessage
import social.waddle.android.feature.conversation.FeedContext
import social.waddle.android.feature.conversation.FeedEntry
import social.waddle.android.feature.conversation.PendingMessage
import social.waddle.android.feature.conversation.feedEntriesOf
import social.waddle.client.ffi.WaddleLinkPreview
import social.waddle.client.ffi.WaddleSharedFile
import java.time.LocalDate
import java.time.ZoneOffset

/** A fixed, realistic channel used by every timeline screenshot. */
internal object ScreenshotFixtures {
    const val ROOM = "general@muc.waddle.test"
    const val SELF = "david@waddle.test"
    val TODAY: LocalDate = LocalDate.of(2026, 10, 16)
    val ZONE: ZoneOffset = ZoneOffset.UTC

    private var counter = 0

    private fun row(
        nick: String,
        body: String,
        at: String,
        mine: Boolean = false,
        reactions: List<ReactionGroup> = emptyList(),
        edited: Boolean = false,
        replyTo: String? = null,
        thread: String? = null,
        mentionsSelf: Boolean = false,
        files: List<WaddleSharedFile> = emptyList(),
        previews: List<WaddleLinkPreview> = emptyList(),
    ): TimelineItem {
        counter += 1
        val id = "m$counter"
        val message = testMessage(
            id = id,
            stanzaId = id,
            stanzaIdBy = ROOM,
            from = "$ROOM/$nick",
            to = SELF,
            body = body,
            messageType = "groupchat",
            isMuc = true,
            timestamp = at,
            thread = thread,
            mentionUris = if (mentionsSelf) listOf("xmpp:$SELF") else emptyList(),
            sharedFiles = files,
            linkPreviews = previews,
        ).let { if (replyTo != null) it.copy(replyToId = replyTo, replyToSender = "$ROOM/alice") else it }
        return TimelineItem(
            id = id,
            conversationJid = ROOM,
            from = message.from,
            body = body,
            timestamp = at,
            isMine = mine,
            source = TimelineSource.Live(message),
            reactions = reactions,
            edited = edited,
            authorJid = if (mine) SELF else "$nick@waddle.test",
            presentationId = id,
            receivedAtMillis = 0L,
        )
    }

    fun channelItems(): List<TimelineItem> {
        counter = 0
        return yesterday() + today()
    }

    private fun yesterday(): List<TimelineItem> = listOf(
            row("alice", "Morning all. Staging is back up after the storage migration.", "2026-10-15T08:58:00Z"),
            row("alice", "Hot tier on Scylla, cold tier flushing to Parquet every 15 minutes.", "2026-10-15T08:59:00Z"),
            row(
                "bob",
                "Nice. Any numbers on MAM query latency yet?",
                "2026-10-15T09:20:00Z",
                reactions = listOf(ReactionGroup("👀", 3, mine = true)),
            ),
        )

    private fun today(): List<TimelineItem> = listOf(
            row(
                "alice",
                "p99 is 41ms for a 50-message page. Write-up is here: https://waddle.social/blog/storage",
                "2026-10-16T09:02:00Z",
                previews = listOf(
                    WaddleLinkPreview(
                        originalUrl = "https://waddle.social/blog/storage",
                        normalizedUrl = "https://waddle.social/blog/storage",
                        title = "How Waddle stores a billion messages",
                        description = "Channel-centric MAM on a hot columnar tier with Iceberg underneath.",
                        image = null,
                        video = null,
                        playerEmbed = null,
                        remoteMediaUnavailable = false,
                    ),
                ),
            ),
            row(
                "david",
                "Great work. Can we get the dashboards into the Monday review?",
                "2026-10-16T09:05:00Z",
                mine = true,
                thread = null,
            ),
            row(
                "carol",
                "@david yes, I'll pin the Grafana board in here before then.",
                "2026-10-16T09:11:00Z",
                mentionsSelf = true,
                reactions = listOf(ReactionGroup("🎉", 2, mine = false), ReactionGroup("👍", 1, mine = true)),
            ),
            row(
                "carol",
                "Export attached for anyone offline.",
                "2026-10-16T09:12:00Z",
                files = listOf(
                    WaddleSharedFile(
                        url = "https://upload.waddle.test/export.csv",
                        name = "mam-latency-oct.csv",
                        mediaType = "text/csv",
                        size = 48_213u,
                        width = null,
                        height = null,
                        desc = null,
                        hashes = emptyList(),
                        disposition = "attachment",
                        encrypted = null,
                    ),
                ),
                edited = true,
            ),
        )

    fun channelEntries(unreadAtOpen: Int = 2, pending: List<PendingMessage> = emptyList()): List<FeedEntry> =
        feedEntriesOf(
            items = channelItems(),
            pending = pending,
            context = FeedContext(
                threadId = null,
                selfBareJid = SELF,
                pinnedIds = emptySet(),
                unreadAtOpen = unreadAtOpen,
                zone = ZONE,
            ),
        )
}
