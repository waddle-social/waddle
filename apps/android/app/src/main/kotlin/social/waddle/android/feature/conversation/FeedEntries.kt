package social.waddle.android.feature.conversation

import androidx.compose.runtime.Immutable
import social.waddle.android.client.messageMentionsBareJid
import social.waddle.android.client.normalizedBareJid
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
    /** Local day to label above this row, when it starts a new day. */
    val daySeparator: LocalDate?,
    /** The first row that was unread when the conversation opened. */
    val unreadDivider: Boolean,
    /** First row of an author burst: draws avatar and header. */
    val startsGroup: Boolean,
    /** Real bare JID behind the avatar; `null` = initials only. */
    val authorJid: String?,
    val sentAt: Instant,
    val mentionsSelf: Boolean,
    val isPinned: Boolean,
    /** Feed roots: replies loaded for this message's thread. */
    val replyCount: Int,
    /** XEP-0461 parent, when it is loaded. */
    val replyParent: TimelineItem?,
    val replyParentAuthorJid: String?,
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
)

/**
 * The visible rows ([visibleRows]) turned into feed entries, oldest first.
 * A row continues the previous author's group when the same person wrote
 * it on the same local day within [GROUP_WINDOW], with no day separator
 * or unread divider between them (web `buildMessageDisplayMeta` parity).
 */
fun feedEntriesOf(
    items: List<TimelineItem>,
    pending: List<PendingMessage>,
    context: FeedContext,
): List<FeedEntry> {
    val rows = visibleRows(items, pending, context.threadId)
    val derived = Derived(
        replyCounts = if (context.threadId == null) replyCountsOf(items) else emptyMap(),
        byIdentity = quotedMessagesByIdentity(rows),
        dividerIndex = unreadDividerIndex(rows, context.unreadAtOpen),
    )
    val out = ArrayList<FeedEntry>(rows.size)
    var previous: Placed? = null
    rows.forEachIndexed { index, row ->
        val placed = place(row, index, previous, context, derived)
        out += placed.entry
        previous = placed
    }
    return out
}

/** Per-conversation lookups computed once per derivation. */
private class Derived(
    val replyCounts: Map<String, Int>,
    val byIdentity: Map<String, TimelineItem>,
    val dividerIndex: Int,
)

/** An entry plus the grouping key the next row compares against. */
private class Placed(val entry: FeedEntry, val authorKey: String?)

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
    val authorJid = authorJidOf(row, context.selfBareJid)
    val authorKey = authorKeyOf(row, authorJid)
    val startsGroup = previous == null ||
        separator != null ||
        divider ||
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
    )
    return Placed(entry, authorKey)
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
 * Own rows never count as unread.
 */
private fun unreadDividerIndex(rows: List<ConversationRow>, unread: Int): Int {
    if (unread <= 0) return -1
    val incoming = rows.indices.filter { index ->
        val item = (rows[index] as? ConversationRow.Stored)?.item
        item != null && !item.isMine
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
