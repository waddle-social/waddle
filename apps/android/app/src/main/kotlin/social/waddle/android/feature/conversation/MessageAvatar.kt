package social.waddle.android.feature.conversation

import social.waddle.android.client.normalizedBareJid
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.authorBareJidOf

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
