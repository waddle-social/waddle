package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import social.waddle.android.client.bareJid

/**
 * Per-conversation unread counters from live traffic: messages that are
 * not the account's own and arrive while the conversation is not being
 * viewed increment; opening the conversation clears.
 */
class UnreadStore {
    private val _counts = MutableStateFlow<Map<String, Int>>(emptyMap())

    /** conversation bare JID → unread count (absent = zero). */
    val counts: StateFlow<Map<String, Int>> = _counts.asStateFlow()

    @Volatile
    private var activeConversation: String? = null

    /** The conversation currently on screen; its messages never count. */
    fun setActiveConversation(conversationJid: String?) {
        activeConversation = conversationJid?.let(::bareJid)
    }

    fun onLiveMessage(conversationJid: String, isMine: Boolean) {
        val conversation = bareJid(conversationJid)
        if (isMine || conversation == activeConversation) return
        _counts.update { it + (conversation to (it[conversation] ?: 0) + 1) }
    }

    fun clear(conversationJid: String) {
        _counts.update { it - bareJid(conversationJid) }
    }

    fun clearAll() {
        _counts.value = emptyMap()
    }
}
