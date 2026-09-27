package social.waddle.android.feature.conversation

import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.TimelineSource
import social.waddle.android.client.store.authorBareJidOf
import java.time.Duration
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId

/**
 * Leading avatar of a received timeline row: [jid] is the author's
 * resolved real bare JID (`null` = unknown → initials); [visible] only
 * on the first row of a sender group, follow-ups keep an aligned gutter.
 */
data class MessageAvatar(val jid: String?, val visible: Boolean)

/**
 * Avatar gutters for the received rows of [rows] (chronological), keyed
 * by stored item id + sender. Web `buildMessageDisplayMeta` grouping: a
 * row continues the previous group when the same author wrote it on the
 * same day within five minutes. Own rows and pending sends get none.
 */
fun messageAvatarsOf(
    rows: List<ConversationRow>,
    occupantJids: Map<String, String>,
    selfBareJid: String?,
): Map<String, MessageAvatar> {
    val out = HashMap<String, MessageAvatar>()
    var previous: TimelineItem? = null
    for (row in rows) {
        val item = (row as? ConversationRow.Stored)?.item
        if (item == null || item.callAnchor != null || item.callEndedMarker != null) {
            previous = null
            continue
        }
        if (!item.isMine) {
            val grouped = previous?.let { continuesGroup(it, item) } ?: false
            out[avatarKeyOf(item)] = MessageAvatar(
                jid = authorBareJidOf(item, occupantJids, selfBareJid),
                visible = !grouped,
            )
        }
        previous = item
    }
    return out
}

/**
 * Real bare JID of the author a XEP-0461 reply quotes: the loaded
 * original's resolved author, else the reply's `to` attribute — an
 * occupant JID of this room resolves through [occupantJids], any other
 * JID is the author's own. `null` = unknown (initials).
 */
fun quotedAuthorJidOf(
    reply: TimelineItem,
    quoted: TimelineItem?,
    occupantJids: Map<String, String>,
    selfBareJid: String?,
): String? {
    if (quoted != null) return authorBareJidOf(quoted, occupantJids, selfBareJid)
    val sender = reply.replyToSender?.trim()?.takeIf { it.isNotEmpty() } ?: return null
    val bare = normalizedBareJid(sender)
    if (bare != normalizedBareJid(reply.conversationJid) || !isGroupchatRow(reply)) {
        return bare.takeIf { '@' in it }
    }
    return sender.substringAfter('/', "").ifEmpty { null }?.let(occupantJids::get)
}

private fun isGroupchatRow(item: TimelineItem): Boolean = when (val source = item.source) {
    is TimelineSource.Live -> source.message.isMuc || source.message.messageType == "groupchat"
    is TimelineSource.Archived -> source.message.messageType == "groupchat"
}

/** Stable per-row key: ids may collide across senders (see TimelineList). */
fun avatarKeyOf(item: TimelineItem): String = "${item.id}:${item.from.orEmpty()}"

private fun continuesGroup(previous: TimelineItem, current: TimelineItem): Boolean {
    if (previous.isMine || previous.from != current.from) return false
    val before = previous.timestamp?.let(::parseInstant)
    val after = current.timestamp?.let(::parseInstant)
    // Undelayed live rows carry no timestamp: consecutive ones group.
    if (before == null || after == null) return before == null && after == null
    val sameDay = before.atZone(ZoneId.systemDefault()).toLocalDate() ==
        after.atZone(ZoneId.systemDefault()).toLocalDate()
    return sameDay && Duration.between(before, after).abs() < GROUP_WINDOW
}

private fun parseInstant(timestamp: String): Instant? =
    runCatching { Instant.parse(timestamp) }.getOrNull()
        ?: runCatching { OffsetDateTime.parse(timestamp).toInstant() }.getOrNull()

private val GROUP_WINDOW: Duration = Duration.ofMinutes(5)
