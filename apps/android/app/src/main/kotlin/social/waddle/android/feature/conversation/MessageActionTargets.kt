package social.waddle.android.feature.conversation

import social.waddle.android.client.store.TimelineItem

/**
 * Reaction/retraction/pin target (web parity): in a MUC strictly the
 * room-assigned XEP-0359 stanza id (`by` must be the room — no id means
 * the action is unavailable); in a DM strictly the AUTHOR-assigned id —
 * the local archive stanza id was stamped by our own server and the
 * peer's copy never carried it, so a reaction/retraction/reply
 * targeting it silently fails to apply on the other side.
 */
fun actionTargetIdOf(item: TimelineItem, isGroupchat: Boolean, conversationJid: String): String? =
    if (isGroupchat) {
        // Authority scan, never the first stanza-id (sender-controlled):
        // an occupant-injected foreign id must not disable actions on
        // the message.
        item.assignedStanzaId(conversationJid)?.id
    } else {
        item.originId ?: item.messageId
    }

/** XEP-0308 targets the AUTHOR-assigned id of the original send. */
fun correctionTargetIdOf(item: TimelineItem): String? = item.originId ?: item.messageId

/**
 * The thread a "reply in thread" on [item] opens: its own thread, else
 * a new thread rooted at the message (web `thread-action-target`
 * parity). The DM root id must be author-assigned for the same reason
 * as [actionTargetIdOf].
 */
fun threadIdFor(item: TimelineItem, isGroupchat: Boolean): String = item.threadId
    ?: if (isGroupchat) item.id else (item.originId ?: item.messageId ?: item.id)

/** Display name of [item]'s author: MUC nick, or the DM localpart. */
fun authorNameOf(item: TimelineItem, isGroupchat: Boolean): String? {
    val from = item.from ?: return null
    return if (isGroupchat) from.substringAfter('/', from) else bareJidOf(from).substringBefore('@')
}

internal fun bareJidOf(jid: String): String = jid.substringBefore('/')
