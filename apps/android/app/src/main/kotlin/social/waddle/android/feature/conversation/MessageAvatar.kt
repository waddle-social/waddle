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
 * by stable local row identity. Web `buildMessageDisplayMeta` grouping: a
 * row continues the previous group when the same author wrote it on the
 * same day within five minutes. Own rows and pending sends get none.
 */
fun messageAvatarsOf(
    rows: List<ConversationRow>,
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
        val jid = authorBareJidOf(item, selfBareJid)
        // A row stamped with someone else's JID is theirs even if our nick
        // matched (a nick we took over after they wrote it).
        val ownRow = item.isMine && (jid == null || jid == selfBareJid?.let(::normalizedBareJid))
        if (!ownRow) {
            val grouped = previous?.let { prev ->
                // Same sender and window (continuesGroup) AND the same
                // resolved person: a reused nick, or known vs unknown,
                // splits; two unknown rows from one nick group (web
                // groups by nick) under one initials avatar.
                authorBareJidOf(prev, selfBareJid) == jid && continuesGroup(prev, item)
            } ?: false
            out[avatarKeyOf(item)] = MessageAvatar(jid = jid, visible = !grouped)
        }
        previous = item
    }
    return out
}

/**
 * Real bare JID of the author a XEP-0461 reply quotes: the loaded
 * original's stamped author. Without the original, the reply's `to`
 * attribute is the REPLYING sender's claim, so it is trusted only in a
 * 1:1 conversation and only when it names one of its two participants
 * (the account or the peer); a room reply falls back to initials — no
 * arbitrary face, and no avatar lookup aimed at a JID the sender picked.
 */
fun quotedAuthorJidOf(reply: TimelineItem, quoted: TimelineItem?, selfBareJid: String?): String? {
    if (quoted != null) return authorBareJidOf(quoted, selfBareJid)
    if (isGroupchatRow(reply)) return null
    val sender = reply.replyToSender?.trim()?.takeIf { it.isNotEmpty() } ?: return null
    val bare = normalizedBareJid(sender)
    val participants = setOfNotNull(selfBareJid?.let(::normalizedBareJid), normalizedBareJid(reply.conversationJid))
    return bare.takeIf { it in participants }
}

private fun isGroupchatRow(item: TimelineItem): Boolean = when (val source = item.source) {
    is TimelineSource.Live -> source.message.isMuc || source.message.messageType == "groupchat"
    is TimelineSource.Archived -> source.message.messageType == "groupchat"
}

/** Canonical room identities win; ambiguous sender aliases cannot attribute a quote. */
fun quotedMessagesByIdentity(rows: List<ConversationRow>): Map<String, TimelineItem> {
    val candidates = HashMap<String, MutableList<TimelineItem>>()
    rows.filterIsInstance<ConversationRow.Stored>().forEach { row ->
        (row.item.identityIds + row.item.id).forEach { id ->
            candidates.getOrPut(id) { mutableListOf() }.add(row.item)
        }
    }
    return buildMap {
        candidates.forEach { (id, items) ->
            val canonical = items.filter {
                isGroupchatRow(it) && it.assignedStanzaId(it.conversationJid)?.id == id
            }
            val target = if (canonical.isEmpty()) items.singleOrNull() else canonical.singleOrNull()
            if (target != null) put(id, target)
        }
    }
}

/** Stable per-row key even when both wire ids and occupant nicks collide. */
fun avatarKeyOf(item: TimelineItem): String = item.presentationId

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
