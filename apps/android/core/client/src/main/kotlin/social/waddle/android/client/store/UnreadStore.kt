package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import social.waddle.android.client.bareJid

/**
 * Per-conversation unread counters from live traffic: messages that are
 * not the account's own and arrive while the conversation is not being
 * viewed increment; opening the conversation clears. One internal lock
 * covers counts AND the active-conversation marker so compound
 * decisions (recompute-unless-active) are atomic against increments.
 */
class UnreadStore {
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

    fun onLiveMessage(conversationJid: String, isMine: Boolean) {
        synchronized(lock) {
            val conversation = bareJid(conversationJid)
            if (isMine || conversation == activeConversation) return
            _counts.value = _counts.value + (conversation to (_counts.value[conversation] ?: 0) + 1)
        }
    }

    fun clear(conversationJid: String) {
        synchronized(lock) {
            _counts.value = _counts.value - bareJid(conversationJid)
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
        }
    }

    fun clearAll() {
        synchronized(lock) {
            _counts.value = emptyMap()
        }
    }
}
