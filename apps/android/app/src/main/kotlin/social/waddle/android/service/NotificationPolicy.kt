package social.waddle.android.service

import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.messageMentionsBareJid
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.client.store.isDirectChat
import social.waddle.android.client.store.isTimelineMutation
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf
import social.waddle.android.jid.resourcepartOf
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleNotifyMode

/**
 * The should-this-message-notify decision for [MessageNotifier], free of
 * notification plumbing: own-JID filtering, pushed-replay (offline flush)
 * filtering, prefs gating, app visibility, message-type classification,
 * XEP-0492 per-conversation mode gating, and the XEP-0333/0490
 * displayed-target resolution.
 *
 * [notifyModeFor] is the XEP-0492 store's effective-mode snapshot
 * (conversation bare JID + kind → mode), injected as a function so the
 * policy stays pure and testable.
 */
class NotificationPolicy(
    private val userPrefs: UserPrefs,
    private val currentSession: StateFlow<WaddleSessionInfo?>,
    private val isAppVisible: () -> Boolean,
    private val notifyModeFor: (conversationJid: String, isGroupchat: Boolean) -> WaddleNotifyMode =
        { _, _ -> WaddleNotifyMode.ALWAYS },
    /** Whether the server pushes to this device (push status ACTIVE). */
    private val pushDelivering: () -> Boolean = { false },
) {
    /** A message that must notify, with everything the poster needs. */
    data class Notify(
        val conversationJid: String,
        val sender: String,
        val isGroupchat: Boolean,
        val body: String,
        val displayedTarget: XmppSessionManager.DisplayedTarget?,
        /** XEP-0372: broadcast or self-mention — post at high priority. */
        val isMention: Boolean = false,
    )

    /**
     * A replay of stored traffic that push already announced: the stanza
     * itself carries a XEP-0203 `<delay/>` (the FFI surfaces it as
     * [WaddleMessage.timestamp]), and the server pushes to this device.
     * The XEP-0160 offline flush stamps every message it replays this way
     * (§3), and while push is active everything stored offline was pushed
     * when it was stored, so the flush must not announce it again. Without
     * push (no distributor, or the stay-connected fallback) nothing else
     * announced it, so the flush still notifies.
     *
     * Live deliveries carry no delay of their own. A received carbon is
     * still live: the server stamps its XEP-0297 `<forwarded/>` wrapper with
     * a delay that the FFI falls back to, so carbons are told apart by
     * [WaddleMessage.carbon].
     */
    fun isPushedReplay(message: WaddleMessage): Boolean =
        message.timestamp != null && message.carbon == null && pushDelivering()

    /**
     * New content to announce: not a pushed replay, and not a correction
     * (which carries a body but only rewrites an existing row, so notifying
     * would re-announce old content as a new message).
     */
    private fun isNewContent(message: WaddleMessage): Boolean =
        !isPushedReplay(message) && !message.isTimelineMutation()

    /** `null` means the message must not raise a notification. */
    suspend fun decide(message: WaddleMessage): Notify? {
        val body = message.body ?: return null
        if (!isNewContent(message)) return null
        if (isAppVisible()) return null
        if (!userPrefs.notificationsEnabled.first()) return null
        val session = currentSession.value ?: return null
        val isGroupchat = message.isMuc || message.messageType == MESSAGE_TYPE_GROUPCHAT
        // Room private messages/invites (`muc#user`) carry `type=chat|normal`
        // from the room JID but are not 1:1 DMs, and neither the DM nor the
        // room reply path can answer them — they never notify.
        if (!isGroupchat && !message.isDirectChat()) return null
        val (conversationJid, sender) =
            foreignSenderOf(message.from, isGroupchat, session) ?: return null
        val isMention = messageMentionsBareJid(
            broadcastMention = message.broadcastMention,
            mentionUris = message.mentionUris,
            selfBareJid = bareJidOf(session.jid),
        )
        // XEP-0492 per-conversation gate: `never` mutes outright,
        // `on-mention` requires a XEP-0372 broadcast or self mention.
        when (notifyModeFor(conversationJid, isGroupchat)) {
            WaddleNotifyMode.NEVER -> return null
            WaddleNotifyMode.ON_MENTION -> if (!isMention) return null
            WaddleNotifyMode.ALWAYS -> Unit
        }
        return Notify(
            conversationJid = conversationJid,
            sender = sender,
            isGroupchat = isGroupchat,
            body = body,
            displayedTarget = displayedTargetOf(message, isGroupchat, conversationJid),
            isMention = isMention,
        )
    }

    /** `(conversationJid, sender)`, or `null` for the account's own traffic. */
    private fun foreignSenderOf(
        from: String?,
        isGroupchat: Boolean,
        session: WaddleSessionInfo,
    ): Pair<String, String>? {
        from ?: return null
        val conversationJid = bareJidOf(from)
        val sender = if (isGroupchat) resourcepartOf(from) ?: localpartOf(from) else localpartOf(from)
        // Own DM carbons come from the account bare JID; own MUC echoes
        // from room/<own nick> — neither should notify.
        if (!isGroupchat && conversationJid == bareJidOf(session.jid)) return null
        if (isGroupchat && sender == session.xmppLocalpart) return null
        return conversationJid to sender
    }

    /**
     * XEP-0333/0490 target for the mark-as-read action, carried in the
     * intent so the dispatch survives process death (the in-memory
     * timeline is empty when the receiver fires after a restart).
     * Same id-class rules as the session manager: room stanza id in
     * MUCs, author-assigned id in 1:1.
     */
    private fun displayedTargetOf(
        message: WaddleMessage,
        isGroupchat: Boolean,
        conversationJid: String,
    ): XmppSessionManager.DisplayedTarget? {
        // Authority-scanned like the session manager's own resolution:
        // the FIRST stanza-id is sender-controlled and must not become
        // a marker target or a published MDS pair.
        val ownBare = currentSession.value?.jid?.let(::bareJidOf)
        val mdsPair = if (isGroupchat) {
            message.stanzaIds.firstOrNull { it.by.equals(conversationJid, ignoreCase = true) }
        } else {
            ownBare?.let { own ->
                val domain = own.substringAfter('@')
                message.stanzaIds.firstOrNull {
                    it.by.equals(own, ignoreCase = true) || it.by.equals(domain, ignoreCase = true)
                }
            }
        }
        val markerId = if (isGroupchat) {
            mdsPair?.id
        } else {
            message.originId ?: message.id
        } ?: return null
        return XmppSessionManager.DisplayedTarget(
            markerId = markerId,
            stanzaId = mdsPair?.id,
            stanzaIdBy = mdsPair?.by,
            markerRequested = message.displayedMarkerRequested,
        )
    }

    private companion object {
        const val MESSAGE_TYPE_GROUPCHAT = "groupchat"
    }
}
