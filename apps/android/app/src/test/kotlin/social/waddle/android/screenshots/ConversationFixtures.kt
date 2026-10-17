package social.waddle.android.screenshots

import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.flowOf
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.SendResult
import social.waddle.android.client.store.ReactionGroup
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.store.UnreadStore
import social.waddle.android.client.testMamPage
import social.waddle.android.client.testMessage
import social.waddle.android.client.testPresence
import social.waddle.android.feature.conversation.ConversationClock
import social.waddle.android.feature.conversation.ConversationIo
import social.waddle.android.feature.conversation.ConversationViewModel
import social.waddle.android.feature.conversation.LocalConversationClock
import social.waddle.android.ui.huddle.LocalReducedMotion
import social.waddle.client.ffi.WaddleLinkPreview
import social.waddle.client.ffi.WaddleMamPage
import social.waddle.client.ffi.WaddleMucAffiliation
import social.waddle.client.ffi.WaddlePresence
import social.waddle.client.ffi.WaddleReference
import social.waddle.client.ffi.WaddleReferenceType
import social.waddle.client.ffi.WaddleSendMessageOutcome
import social.waddle.client.ffi.WaddleSharedFile
import java.time.Instant
import java.time.ZoneOffset
import java.util.Locale

/**
 * A realistic room, DM and thread for the conversation goldens, fed
 * through the real [ConversationViewModel] (fake transport, fixed clock).
 */
internal object ConversationFixtures {
    const val ROOM = "general@muc.waddle.test"
    const val PEER = "alice@waddle.test"
    const val SELF = "david@waddle.test"
    const val BOT = "deploybot@waddle.test"
    const val THREAD_ROOT = "c-bob-latency"

    /** Fri 16 Oct 2026, 09:20 UTC, en-GB, 24-hour. */
    val CLOCK = ConversationClock(
        locale = Locale.UK,
        use24Hour = true,
        zone = ZoneOffset.UTC,
        today = ScreenshotFixtures.TODAY,
        now = { NOW },
    )

    private val NOW: Instant = Instant.parse("2026-10-16T09:20:00Z")
    private val OPENED_AT: Long = NOW.toEpochMilli()

    /** A transport whose newest page answers at once and holds nothing more. */
    private object SettledIo : ConversationIo {
        override suspend fun fetchHistory(maxMessages: UInt, beforeId: String?): WaddleMamPage =
            testMamPage(isComplete = true)

        override suspend fun send(body: String, extras: MessageSendExtras?): SendResult =
            SendResult(WaddleSendMessageOutcome.Sent("sent"))

        override val canPin: Boolean get() = true
    }

    fun viewModel(
        conversationJid: String,
        isGroupchat: Boolean,
        items: List<TimelineItem>,
        threadId: String? = null,
        unread: Int = 0,
        typing: List<String> = emptyList(),
    ): ConversationViewModel {
        val unreadStore = UnreadStore().apply { if (unread > 0) setUnlessActive(conversationJid, unread) }
        return ConversationViewModel(
            conversationJid = conversationJid,
            isGroupchat = isGroupchat,
            timeline = MutableStateFlow(items),
            events = MutableSharedFlow(),
            unreadStore = unreadStore,
            io = SettledIo,
            typingNames = flowOf(typing),
            threadId = threadId,
            occupantPresence = flowOf(if (isGroupchat) occupants() else emptyMap()),
            botJids = flowOf(if (isGroupchat) setOf(BOT) else emptySet()),
            clock = { OPENED_AT },
            selfBareJid = SELF,
            feedDispatcher = Dispatchers.Unconfined,
        )
    }

    /** Pinned clock and still motion for every conversation golden. */
    @Composable
    fun Pinned(content: @Composable () -> Unit) {
        CompositionLocalProvider(
            LocalConversationClock provides CLOCK,
            LocalReducedMotion provides true,
            content = content,
        )
    }

    private fun occupants(): Map<String, WaddlePresence> = mapOf(
        "alice" to testPresence(from = "$ROOM/alice", mucAffiliation = WaddleMucAffiliation.OWNER),
        "carol" to testPresence(from = "$ROOM/carol", mucAffiliation = WaddleMucAffiliation.ADMIN),
    )

    private fun item(
        id: String,
        nick: String,
        body: String,
        at: String,
        conversation: String = ROOM,
        groupchat: Boolean = true,
        mine: Boolean = false,
        reactions: List<ReactionGroup> = emptyList(),
        edited: Boolean = false,
        replyTo: String? = null,
        thread: String? = null,
        references: List<WaddleReference> = emptyList(),
        mentionsSelf: Boolean = false,
        files: List<WaddleSharedFile> = emptyList(),
        previews: List<WaddleLinkPreview> = emptyList(),
    ): TimelineItem {
        val from = if (groupchat) "$conversation/$nick" else "$nick@waddle.test/phone"
        val message = testMessage(
            id = id,
            stanzaId = id,
            stanzaIdBy = if (groupchat) conversation else null,
            from = from,
            to = SELF,
            body = body,
            messageType = if (groupchat) "groupchat" else "chat",
            isMuc = groupchat,
            timestamp = at,
            thread = thread,
            mentionUris = if (mentionsSelf) listOf("xmpp:$SELF") else emptyList(),
            references = references,
            sharedFiles = files,
            linkPreviews = previews,
        ).let { if (replyTo != null) it.copy(replyToId = replyTo, replyToSender = "$conversation/david") else it }
        return TimelineItem(
            id = id,
            conversationJid = conversation,
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

    private fun mention(body: String, nick: String) = WaddleReference(
        refType = WaddleReferenceType.Mention,
        uri = "xmpp:$nick@waddle.test",
        begin = body.indexOf("@$nick").toUInt(),
        end = (body.indexOf("@$nick") + nick.length + 1).toUInt(),
        anchor = null,
    )

    /** A busy room across two days: hats, a bot, a thread, a link card, a mention, a file and a reply. */
    fun roomItems(): List<TimelineItem> = yesterdayItems() + todayItems()

    private fun yesterdayItems(): List<TimelineItem> = listOf(
            item(
                "c-alice-1",
                "alice",
                "Morning all. Staging is back up after the storage migration.",
                "2026-10-15T08:58:00Z",
            ),
            item(
                "c-alice-2",
                "alice",
                "Hot tier on Scylla, cold tier flushing to Parquet every 15 minutes.",
                "2026-10-15T08:59:00Z",
            ),
            item(
                THREAD_ROOT,
                "bob",
                "Nice. Any numbers on MAM query latency yet?",
                "2026-10-15T09:20:00Z",
                reactions = listOf(ReactionGroup("👀", 3, mine = true)),
            ),
            item("c-t1", "carol", "Benchmarks are running now.", "2026-10-15T09:24:00Z", thread = THREAD_ROOT),
            item("c-t2", "alice", "First pass looks good: under 50ms.", "2026-10-15T09:31:00Z", thread = THREAD_ROOT),
            item("c-t3", "erin", "Can we try with cold reads too?", "2026-10-15T10:02:00Z", thread = THREAD_ROOT),
        )

    private val storagePreview = WaddleLinkPreview(
        originalUrl = "https://waddle.social/blog/storage",
        normalizedUrl = "https://waddle.social/blog/storage",
        title = "How Waddle stores a billion messages",
        description = "Channel-centric MAM on a hot columnar tier with Iceberg underneath.",
        image = null,
        video = null,
        playerEmbed = null,
        remoteMediaUnavailable = false,
    )

    private val exportFile = WaddleSharedFile(
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
    )

    private fun todayItems(): List<TimelineItem> {
        val mentionBody = "@david yes, I'll pin the Grafana board in here before then."
        return listOf(
            item("c-bot", "deploybot", "Deployed waddle-server 0.42.1 to staging.", "2026-10-16T08:40:00Z"),
            item(
                "c-alice-3",
                "alice",
                "p99 is 41ms for a 50-message page. Write-up is here: https://waddle.social/blog/storage",
                "2026-10-16T09:02:00Z",
                previews = listOf(storagePreview),
            ),
            item(
                "c-david",
                "david",
                "Great work. Can we get the dashboards into the Monday review?",
                "2026-10-16T09:05:00Z",
                mine = true,
            ),
            item(
                "c-carol-1",
                "carol",
                mentionBody,
                "2026-10-16T09:11:00Z",
                mentionsSelf = true,
                references = listOf(mention(mentionBody, "david")),
                reactions = listOf(ReactionGroup("🎉", 2, mine = false), ReactionGroup("👍", 1, mine = true)),
            ),
            item(
                "c-carol-2",
                "carol",
                "Export attached for anyone offline.",
                "2026-10-16T09:12:00Z",
                edited = true,
                files = listOf(exportFile),
            ),
            item(
                "c-bob-2",
                "bob",
                "Monday works. I'll bring the p99 graphs.",
                "2026-10-16T09:16:00Z",
                replyTo = "c-david",
            ),
        )
    }

    /** The open thread: its root and three replies. */
    fun threadItems(): List<TimelineItem> = roomItems().filter { it.id == THREAD_ROOT || it.threadId == THREAD_ROOT }

    /** A quiet 1:1 with Alice. */
    fun dmItems(): List<TimelineItem> = listOf(
        item(
            "d1",
            "alice",
            "Are you around for a quick look at the reconnect fix?",
            "2026-10-14T17:40:00Z",
            conversation = PEER,
            groupchat = false,
        ),
        item(
            "d2",
            "david",
            "Yes, after standup tomorrow.",
            "2026-10-14T17:52:00Z",
            conversation = PEER,
            groupchat = false,
            mine = true,
        ),
        item(
            "d3",
            "alice",
            "Shipped it. Can you try it on a flaky train line?",
            "2026-10-16T08:31:00Z",
            conversation = PEER,
            groupchat = false,
            reactions = listOf(ReactionGroup("🚀", 1, mine = true)),
        ),
        item(
            "d4",
            "alice",
            "It should hold the queue and replay on the next tunnel exit.",
            "2026-10-16T08:32:00Z",
            conversation = PEER,
            groupchat = false,
        ),
    )
}
