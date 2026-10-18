package social.waddle.android.feature.conversation

import androidx.compose.runtime.Immutable
import social.waddle.android.client.messageMentionsBareJid
import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.replyFallbackQuoteOf
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.store.authorBareJidOf
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.time.OffsetDateTime
import java.time.ZoneId

/**
 * One rendered timeline row with everything the UI needs precomputed:
 * grouping, separators, the resolved reply parent, reply count and
 * avatar identity. Built off the main thread once per timeline change
 * ([feedEntriesOf]) so composition only draws.
 */
@Immutable
data class FeedEntry(
    /** Stable LazyColumn key. */
    val key: String,
    val row: ConversationRow,
    /** Local day to label above this row, when it starts a new day on screen. */
    val daySeparator: LocalDate?,
    /**
     * The first row that was unread when the conversation opened. The
     * divider sits above it, or below it when the newest is on top.
     */
    val unreadDivider: Boolean,
    /** Top row of an author burst on screen: draws avatar and header. */
    val startsGroup: Boolean,
    /** Real bare JID behind the avatar; `null` = initials only. */
    val authorJid: String?,
    val sentAt: Instant,
    val mentionsSelf: Boolean,
    val isPinned: Boolean,
    /** Feed roots: replies loaded for this message's thread. */
    val replyCount: Int,
    /**
     * XEP-0461 parent, when it is loaded — including rows the feed hides
     * (a thread reply quoted from the main feed).
     */
    val replyParent: TimelineItem?,
    val replyParentAuthorJid: String?,
    /** The reply's own fallback quote of its parent, when [replyParent] is not loaded. */
    val replyFallbackQuote: String? = null,
    /** Feed roots: who replied in this message's thread, and when. */
    val threadActivity: ThreadActivity? = null,
) {
    val isCallRow: Boolean get() = isCallRow(row)
}

private fun isCallRow(row: ConversationRow): Boolean {
    val item = (row as? ConversationRow.Stored)?.item ?: return false
    return item.callAnchor != null || item.callEndedMarker != null
}

/** Inputs to [feedEntriesOf] that change rarely. */
data class FeedContext(
    val threadId: String?,
    val selfBareJid: String?,
    val pinnedIds: Set<String>,
    /** Incoming rows unread when the screen opened (divider placement). */
    val unreadAtOpen: Int,
    val zone: ZoneId = ZoneId.systemDefault(),
    /**
     * Pins the divider to the first unread message once it is known, so
     * messages arriving while the screen is open do not move it.
     */
    val unreadAnchor: UnreadAnchor? = null,
    /**
     * The screen's newest history page has loaded. Until then the cached
     * rows can be missing unread messages, so [unreadAnchor] does not pin.
     */
    val newestHistoryLoaded: Boolean = true,
    /**
     * Social layout: the newest row is drawn on top. Grouping and day
     * separators then run newest to oldest, as the reader sees them
     * (web `buildMessageDisplayMeta` over the flipped list).
     */
    val newestFirst: Boolean = false,
)

/**
 * The unread divider's position for one screen visit: the first unread
 * message, i.e. the oldest of the last [unreadAtOpen] incoming rows sent
 * before [openedAt] (later arrivals were not part of the count). It
 * follows the count while the newest history is still loading (a stale
 * cache can lack unread rows), then is held by identity from the first
 * derivation after that page lands. Derivations run one at a time on
 * the feed dispatcher.
 *
 * Archived rows carry the server's timestamp, so a server clock running
 * ahead of the device can push a message sent just before opening past
 * [openedAt]; the divider then lands one message late.
 */
class UnreadAnchor(private val unreadAtOpen: Int, private val openedAt: Instant) {
    @Volatile
    private var anchorKey: String? = null

    internal fun dividerIndex(rows: List<ConversationRow>, newestHistoryLoaded: Boolean): Int {
        anchorKey?.let { key -> return rows.indexOfFirst { rowKeyOf(it) == key } }
        val index = unreadDividerIndex(rows, unreadAtOpen, sentBefore = openedAt)
        if (newestHistoryLoaded && index >= 0) anchorKey = rowKeyOf(rows[index])
        return index
    }
}

/**
 * The visible rows ([visibleRows]) turned into feed entries, oldest first.
 * A row continues the group of the row drawn above it when the same
 * person wrote both on the same local day within [GROUP_WINDOW], with no
 * day separator or unread divider between them (web
 * `buildMessageDisplayMeta` parity). [FeedContext.newestFirst] flips
 * which row is drawn above; the list stays oldest first either way.
 */
fun feedEntriesOf(
    items: List<TimelineItem>,
    pending: List<PendingMessage>,
    context: FeedContext,
): List<FeedEntry> {
    val rows = visibleRows(items, pending, context.threadId)
    val derived = Derived(
        replyCounts = if (context.threadId == null) replyCountsOf(items) else emptyMap(),
        threadActivity = if (context.threadId == null) threadActivityOf(items, context.selfBareJid) else emptyMap(),
        // Every loaded row, not just the visible ones: a feed reply can
        // quote a message the feed hides (one inside a thread).
        byIdentity = quotedMessagesByIdentity(items.map(ConversationRow::Stored)),
        dividerIndex = context.unreadAnchor?.dividerIndex(rows, context.newestHistoryLoaded)
            ?: unreadDividerIndex(rows, context.unreadAtOpen),
    )
    val out = ArrayList<FeedEntry>(rows.size)
    var previous: Placed? = null
    val onScreen = if (context.newestFirst) rows.indices.reversed() else rows.indices
    for (index in onScreen) {
        val placed = place(rows[index], index, previous, context, derived)
        out += placed.entry
        previous = placed
    }
    if (context.newestFirst) out.reverse()
    return out
}

/** Per-conversation lookups computed once per derivation. */
private class Derived(
    val replyCounts: Map<String, Int>,
    val threadActivity: Map<String, ThreadActivity>,
    val byIdentity: Map<String, TimelineItem>,
    val dividerIndex: Int,
)

/** An entry plus the grouping key the next row compares against. */
private class Placed(val entry: FeedEntry, val authorKey: String?)

/** Places [row] under [previous], the row drawn just above it. */
private fun place(
    row: ConversationRow,
    index: Int,
    previous: Placed?,
    context: FeedContext,
    derived: Derived,
): Placed {
    val sentAt = sentAtOf(row)
    val day = sentAt.atZone(context.zone).toLocalDate()
    val separator = day.takeIf { it != previous?.entry?.sentAt?.atZone(context.zone)?.toLocalDate() }
    val divider = index == derived.dividerIndex
    // The divider is drawn above its row, or below it when the newest is on top.
    val dividerAbove = if (context.newestFirst) previous?.entry?.unreadDivider == true else divider
    val authorJid = authorJidOf(row, context.selfBareJid)
    val authorKey = authorKeyOf(row, authorJid)
    val startsGroup = previous == null ||
        separator != null ||
        dividerAbove ||
        !continuesGroup(previous, row, authorKey, sentAt)
    val item = (row as? ConversationRow.Stored)?.item
    val parent = item?.replyToId?.let { derived.byIdentity[it] }
    val entry = FeedEntry(
        key = rowKeyOf(row),
        row = row,
        daySeparator = separator,
        unreadDivider = divider,
        startsGroup = startsGroup,
        authorJid = authorJid,
        sentAt = sentAt,
        mentionsSelf = item != null && mentionsReader(item, context.selfBareJid),
        isPinned = item != null && item.identityIds.any { it in context.pinnedIds },
        replyCount = item?.let { replyCountOf(it, derived.replyCounts) } ?: 0,
        replyParent = parent,
        replyParentAuthorJid = item?.let { quotedAuthorJidOf(it, parent, context.selfBareJid) },
        replyFallbackQuote = item?.takeIf { parent == null }?.let(::fallbackQuoteOf),
        threadActivity = item?.let { root ->
            (listOfNotNull(root.threadId) + root.identityIds).firstNotNullOfOrNull { derived.threadActivity[it] }
        },
    )
    return Placed(entry, authorKey)
}

/** A reply's XEP-0461 fallback quote of its parent, from the wire body. */
private fun fallbackQuoteOf(reply: TimelineItem): String? = reply.replyToId?.let {
    replyFallbackQuoteOf(reply.source.body.orEmpty(), reply.source.replyFallbackStart, reply.source.replyFallbackEnd)
}

/** Same author, within [GROUP_WINDOW], and neither row is a call marker. */
private fun continuesGroup(previous: Placed, row: ConversationRow, authorKey: String?, sentAt: Instant): Boolean =
    !isCallRow(row) &&
        !previous.entry.isCallRow &&
        authorKey == previous.authorKey &&
        Duration.between(previous.entry.sentAt, sentAt).abs() < GROUP_WINDOW

private fun mentionsReader(item: TimelineItem, selfBareJid: String?): Boolean =
    !item.isMine &&
        messageMentionsBareJid(
            broadcastMention = item.source.broadcastMention,
            mentionUris = item.source.mentionUris,
            selfBareJid = selfBareJid,
        )

/** Stable per-row key even when both wire ids and occupant nicks collide. */
fun rowKeyOf(row: ConversationRow): String = when (row) {
    is ConversationRow.Stored -> "s:${row.item.presentationId}"
    is ConversationRow.Unconfirmed -> "p:${row.message.localId}"
}

/** A root's replies key their `<thread/>` by ANY of the root's wire identities. */
private fun replyCountOf(item: TimelineItem, counts: Map<String, Int>): Int =
    (listOfNotNull(item.threadId) + item.identityIds).firstNotNullOfOrNull { counts[it] } ?: 0

/**
 * Index of the oldest of the last [unread] incoming stored rows, or -1.
 * Own rows never count as unread, nor rows sent at or after [sentBefore].
 */
private fun unreadDividerIndex(rows: List<ConversationRow>, unread: Int, sentBefore: Instant? = null): Int {
    if (unread <= 0) return -1
    val incoming = rows.indices.filter { index ->
        val item = (rows[index] as? ConversationRow.Stored)?.item
        item != null && !item.isMine && (sentBefore == null || sentAtOf(item) < sentBefore)
    }
    return incoming.getOrNull(incoming.size - unread) ?: -1
}

private fun authorJidOf(row: ConversationRow, selfBareJid: String?): String? = when (row) {
    is ConversationRow.Stored -> authorBareJidOf(row.item, selfBareJid)
    is ConversationRow.Unconfirmed -> selfBareJid?.let(::normalizedBareJid)
}

/**
 * Who a row is "by" for grouping: the resolved person when known, else
 * the wire sender (two unknown rows from one nick group, as on web).
 * Pending sends group with the account's own stored rows.
 */
private fun authorKeyOf(row: ConversationRow, authorJid: String?): String? = when (row) {
    is ConversationRow.Stored -> authorJid?.let { "jid:$it" } ?: row.item.from?.let { "from:$it" }
    is ConversationRow.Unconfirmed -> authorJid?.let { "jid:$it" } ?: "self"
}

/** Wire time when the stanza carries one, else when this device stored it. */
fun sentAtOf(row: ConversationRow): Instant = when (row) {
    is ConversationRow.Stored -> sentAtOf(row.item)
    is ConversationRow.Unconfirmed -> Instant.ofEpochMilli(row.message.timestampMillis)
}

fun sentAtOf(item: TimelineItem): Instant =
    item.timestamp?.let(::parseInstant) ?: Instant.ofEpochMilli(item.receivedAtMillis)

/** True for MUC rows (groupchat type or a live MUC stanza). */
fun isGroupchatRow(item: TimelineItem): Boolean = when (val source = item.source) {
    is TimelineSource.Live -> source.message.isMuc || source.message.messageType == "groupchat"
    is TimelineSource.Archived -> source.message.messageType == "groupchat"
}

internal fun parseInstant(timestamp: String): Instant? =
    runCatching { Instant.parse(timestamp) }.getOrNull()
        ?: runCatching { OffsetDateTime.parse(timestamp).toInstant() }.getOrNull()

private val GROUP_WINDOW: Duration = Duration.ofMinutes(5)
