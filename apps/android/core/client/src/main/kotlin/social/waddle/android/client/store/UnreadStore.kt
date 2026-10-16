package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import social.waddle.android.client.bareJid

/**
 * Per-conversation unread counters from live traffic: messages that are
 * not the account's own and arrive while the conversation is not being
 * viewed increment; opening the conversation clears. One internal lock
 * covers counts, [mentions] AND the active-conversation marker so
 * compound decisions (recompute-unless-active) are atomic against
 * increments, and a mention is counted and cleared in the same step as
 * its unread.
 */
class UnreadStore(
    /** Written only from here, under the same lock as [counts]. */
    val mentions: MentionStore = MentionStore(),
) {
    private val lock = Any()
    private val _counts = MutableStateFlow<Map<String, Int>>(emptyMap())

    /** conversation bare JID → unread count (absent = zero). */
    val counts: StateFlow<Map<String, Int>> = _counts.asStateFlow()

    private var activeConversation: String? = null

    /** The conversation currently on screen; its messages never count. */
    fun setActiveConversation(conversationJid: String?) {
        synchronized(lock) {
            activeConversation = conversationJid?.let(::bareJid)
        }
    }

    /** Whether [conversationJid] is the on-screen conversation. */
    fun isActiveConversation(conversationJid: String): Boolean = synchronized(lock) {
        activeConversation == bareJid(conversationJid)
    }

    /**
     * Compare-and-clear for pause/dispose hooks: Navigation 3 keeps the
     * covered entry RESUMED through the exit animation, so screen A's
     * pause can run AFTER screen B's resume — an unconditional
     * `setActiveConversation(null)` from A would clobber B's marker and
     * the on-screen conversation would keep accruing unread.
     */
    fun clearActiveConversationIf(conversationJid: String) {
        synchronized(lock) {
            if (activeConversation == bareJid(conversationJid)) {
                activeConversation = null
            }
        }
    }

    /**
     * A live, feed-visible message counted as unread unless it is ours
     * or its conversation is on screen; [mentionsSelf] (it addresses
     * the account, see `messageMentionsBareJid`) also counts a mention.
     */
    fun onLiveMessage(conversationJid: String, isMine: Boolean, mentionsSelf: Boolean = false) {
        synchronized(lock) {
            val conversation = bareJid(conversationJid)
            if (isMine || conversation == activeConversation) return
            _counts.value = _counts.value + (conversation to (_counts.value[conversation] ?: 0) + 1)
            if (mentionsSelf) mentions.increment(conversation)
        }
    }

    /**
     * A live mention of the account whose unread the server inbox
     * already accounted (its absolute count covered the message): only
     * the mention counts, on the same not-ours/not-on-screen terms.
     */
    fun onLiveMentionAccounted(conversationJid: String, isMine: Boolean) {
        synchronized(lock) {
            val conversation = bareJid(conversationJid)
            if (isMine || conversation == activeConversation) return
            mentions.increment(conversation)
        }
    }

    fun clear(conversationJid: String) {
        synchronized(lock) {
            val conversation = bareJid(conversationJid)
            _counts.value = _counts.value - conversation
            mentions.clear(conversation)
        }
    }

    /**
     * Cross-device read sync (XEP-0490): a sibling device's displayed
     * cursor recomputes this conversation's count outright — unless the
     * conversation is on screen, whose badge the local read path owns.
     * The active check and the write are one atomic step: a check-then-
     * act would let a stale recompute stamp over the conversation the
     * user opened in between.
     */
    fun setUnlessActive(conversationJid: String, count: Int) {
        synchronized(lock) {
            val conversation = bareJid(conversationJid)
            if (conversation == activeConversation) return
            _counts.value =
                if (count <= 0) _counts.value - conversation else _counts.value + (conversation to count)
            mentions.capAt(conversation, count)
        }
    }

    /** A fresh stream: mention counts are live-only (see [MentionStore]). */
    fun resetMentions() {
        synchronized(lock) {
            mentions.clearAll()
        }
    }

    fun clearAll() {
        synchronized(lock) {
            _counts.value = emptyMap()
            mentions.clearAll()
        }
    }
}
